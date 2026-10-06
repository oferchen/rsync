//! The QUIC driver's datagram I/O: `quinn-udp` for batched, offloaded socket
//! calls plus the wake channel the facades use to rouse a sleeping driver.
//!
//! `quinn-udp` adds what a std `UdpSocket` cannot express: UDP GSO (one
//! `sendmsg` carries a train of equal-sized datagrams via `UDP_SEGMENT`), GRO
//! and `recvmmsg` batching on receive, ECN marks in both directions, and the
//! don't-fragment bit that makes path-MTU discovery safe. Where the platform
//! lacks an offload `quinn-udp` reports one segment and the driver degrades to
//! one datagram per call, as before.
//!
//! Unix keeps the socket non-blocking and sleeps in `poll(2)` on the socket
//! and a `UnixStream` wake pair together. Windows has no `poll` over the
//! wake handle here, so it keeps the previous shape: a blocking receive
//! bounded by `SO_RCVTIMEO` and a loopback wake datagram, filtered out by its
//! source address.

use std::io::{self, IoSliceMut};
use std::net::UdpSocket;
use std::time::Duration;

use quinn_udp::{BATCH_SIZE, RecvMeta, Transmit, UdpSockRef, UdpSocketState};

/// Upper bound on datagrams per GSO train. quinn's own driver uses the same
/// cap: longer trains only add burstiness, since the pacer releases a window
/// at a time anyway.
const MAX_TRANSMIT_SEGMENTS: usize = 10;

/// Receive buffers per batched receive; each holds one GRO-coalesced train.
pub(super) const RECV_BATCH: usize = if BATCH_SIZE < 8 { BATCH_SIZE } else { 8 };

/// One endpoint's socket and receive buffers.
pub(super) struct UdpIo {
    socket: UdpSocket,
    state: UdpSocketState,
    bufs: [Vec<u8>; RECV_BATCH],
    meta: [RecvMeta; RECV_BATCH],
    wait: sys::Wait,
    /// Ceiling on datagrams per transmit: [`MAX_TRANSMIT_SEGMENTS`], or 1
    /// for [`UdpIo::single_datagram`].
    segment_cap: usize,
}

/// Handle a facade uses to wake the driver out of [`UdpIo::recv`].
pub(super) struct Waker(sys::Waker);

impl Waker {
    /// Ends the driver's current (or next) sleep. Best effort: a lost wake
    /// costs at most one bounded sleep, never correctness.
    pub(super) fn wake(&self) {
        self.0.wake();
    }
}

impl UdpIo {
    /// Takes over `socket`, enabling every offload the platform supports.
    ///
    /// `max_udp_payload` is the endpoint's advertised maximum datagram size;
    /// receive buffers hold that times [`recv_segments`].
    pub(super) fn new(socket: UdpSocket, max_udp_payload: usize) -> io::Result<(Self, Waker)> {
        let state = UdpSocketState::new(UdpSockRef::from(&socket))?;
        let (wait, waker) = sys::pair(&socket)?;
        let buf_len = max_udp_payload * recv_segments(&state);
        let io = Self {
            bufs: std::array::from_fn(|_| vec![0; buf_len]),
            meta: [RecvMeta::default(); RECV_BATCH],
            socket,
            state,
            wait,
            segment_cap: MAX_TRANSMIT_SEGMENTS,
        };
        Ok((io, Waker(waker)))
    }

    /// Like [`Self::new`], but never builds a GSO train, so Linux tests drive
    /// the one-datagram-per-send path that macOS, Windows without USO and the
    /// BSDs take.
    #[cfg(test)]
    pub(super) fn single_datagram(
        socket: UdpSocket,
        max_udp_payload: usize,
    ) -> io::Result<(Self, Waker)> {
        let (mut io, waker) = Self::new(socket, max_udp_payload)?;
        io.segment_cap = 1;
        Ok((io, waker))
    }

    /// Whether datagrams may be fragmented in flight. Path-MTU discovery is
    /// only sound when this is `false` (the don't-fragment bit is set).
    pub(super) fn may_fragment(&self) -> bool {
        self.state.may_fragment()
    }

    /// The socket's current GSO segment limit. `quinn-udp` lowers it to 1
    /// at runtime when the kernel rejects a GSO send.
    pub(super) fn gso_segments(&self) -> usize {
        self.state.max_gso_segments()
    }

    /// Datagrams one receive buffer holds; see [`recv_segments`].
    pub(super) fn gro_segments(&self) -> usize {
        recv_segments(&self.state)
    }

    /// Most datagrams one [`Self::send`] may carry: the platform's GSO limit,
    /// capped at [`MAX_TRANSMIT_SEGMENTS`]. Always at least 1.
    pub(super) fn max_transmit_segments(&self) -> usize {
        self.gso_segments().clamp(1, self.segment_cap)
    }

    /// Sends one quinn-proto transmit (a single datagram or a GSO train).
    ///
    /// Waits for send-buffer room rather than dropping the train. `EMSGSIZE`
    /// is an oversized MTU probe and counts as sent, as in `quinn-udp`'s own
    /// `send`. Any other kernel refusal comes back as [`Sent::Dropped`]:
    /// QUIC loss recovery covers the drop, and the caller records why.
    /// `try_send` keeps `quinn-udp`'s GSO fallback on Linux `EIO`/`EINVAL`,
    /// which lives below both entry points.
    pub(super) fn send(&self, t: &quinn_proto::Transmit, contents: &[u8]) -> io::Result<Sent> {
        let transmit = Transmit {
            destination: t.destination,
            ecn: t
                .ecn
                .and_then(|e| quinn_udp::EcnCodepoint::from_bits(e as u8)),
            contents,
            segment_size: t.segment_size,
            src_ip: t.src_ip,
        };
        loop {
            match self
                .state
                .try_send(UdpSockRef::from(&self.socket), &transmit)
            {
                Ok(()) => return Ok(Sent::Delivered),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    self.wait.writable(&self.socket)?;
                }
                Err(e) if sys::is_message_too_large(&e) => return Ok(Sent::Delivered),
                Err(e) => return Ok(Sent::Dropped(e)),
            }
        }
    }

    /// Receives one batch. With `timeout` set, sleeps up to that long (or
    /// until woken) for the first datagram; with `None`, never blocks.
    ///
    /// Returns the batch entries in arrival order as `(meta, payload)`; each
    /// payload may hold several GRO-coalesced datagrams of `meta.stride`
    /// bytes. An empty result means timeout, wake, or nothing queued.
    pub(super) fn recv(&mut self, timeout: Option<Duration>) -> io::Result<Batch<'_>> {
        let count = self.wait.recv(&self.socket, timeout, |socket| {
            let mut slices = self.bufs.each_mut().map(|b| IoSliceMut::new(b));
            self.state
                .recv(UdpSockRef::from(socket), &mut slices, &mut self.meta)
        })?;
        let count = self.wait.filter(&mut self.meta[..count]);
        Ok(Batch {
            bufs: &self.bufs,
            meta: &self.meta[..count],
        })
    }
}

/// Outcome of [`UdpIo::send`].
#[derive(Debug)]
pub(super) enum Sent {
    /// Handed to the kernel (or an oversized MTU probe, which QUIC expects
    /// to lose).
    Delivered,
    /// The kernel refused the transmit; QUIC treats it as lost.
    Dropped(io::Error),
}

/// Send errors that `quinn-udp`'s `send` would have logged with its log
/// compiled out and then swallowed. Each one is a lost transmit to QUIC;
/// they are kept so a debug trace and a stall can name the cause.
#[derive(Debug, Default)]
pub(super) struct SendErrors {
    count: u64,
    last: Option<String>,
    /// Each distinct errno with its first error text, in first-seen order.
    distinct: Vec<(Option<i32>, String)>,
    /// How many of `distinct` have been traced.
    traced: usize,
}

impl SendErrors {
    pub(super) fn record(&mut self, e: &io::Error) {
        self.count += 1;
        let text = e.to_string();
        let errno = e.raw_os_error();
        if self.distinct.iter().all(|(seen, _)| *seen != errno) {
            self.distinct.push((errno, text.clone()));
        }
        self.last = Some(text);
    }

    /// Errors with an errno not traced before, marking them traced.
    pub(super) fn untraced(&mut self) -> impl Iterator<Item = &str> {
        let start = std::mem::replace(&mut self.traced, self.distinct.len());
        self.distinct[start..].iter().map(|(_, text)| text.as_str())
    }

    /// What an idle timeout should add to its error text, if any send failed.
    pub(super) fn stall_note(&self) -> Option<String> {
        let last = self.last.as_deref()?;
        Some(format!("{} UDP send error(s), last: {last}", self.count))
    }
}

/// One received batch: `(meta, buffer)` pairs.
pub(super) struct Batch<'a> {
    bufs: &'a [Vec<u8>],
    meta: &'a [RecvMeta],
}

impl Batch<'_> {
    /// Whether the receive returned no datagrams.
    pub(super) fn is_empty(&self) -> bool {
        self.meta.is_empty()
    }

    /// Every datagram in the batch, GRO trains split at their stride, as
    /// `(meta, datagram)`.
    pub(super) fn datagrams(&self) -> impl Iterator<Item = (&RecvMeta, &[u8])> {
        self.meta.iter().zip(self.bufs).flat_map(|(meta, buf)| {
            let stride = meta.stride.max(1);
            buf[..meta.len].chunks(stride).map(move |d| (meta, d))
        })
    }
}

/// Datagrams one receive may coalesce into a single buffer.
///
/// Windows reports 64 regardless of the socket (quinn-udp windows.rs
/// `gro_segments`), but receive coalescing (URO) stays off there: quinn-udp
/// never calls `set_gro`, because of quinn-rs/quinn#2041. Every Windows
/// receive is one datagram, so buffers sized for 64 would be dead weight.
/// Windows still sends GSO trains via USO.
fn recv_segments(state: &UdpSocketState) -> usize {
    if cfg!(windows) {
        1
    } else {
        state.gro_segments()
    }
}

/// Converts a received ECN mark into quinn-proto's type.
pub(super) fn ecn_of(meta: &RecvMeta) -> Option<quinn_proto::EcnCodepoint> {
    meta.ecn
        .and_then(|e| quinn_proto::EcnCodepoint::from_bits(e as u8))
}

#[cfg(unix)]
mod sys {
    use std::io::{self, Read, Write};
    use std::net::UdpSocket;
    use std::os::fd::AsFd;
    use std::os::unix::net::UnixStream;
    use std::time::Duration;

    /// Upper bound on one wait for send-buffer room; a stuck socket surfaces
    /// as a retry, not a hang, since the driver loop re-checks its state.
    const SEND_WAIT: Duration = Duration::from_millis(100);

    /// Driver-side half: sleeps in `poll(2)` on the socket and the wake pair.
    pub(super) struct Wait {
        wake_rx: UnixStream,
    }

    /// Facade-side half: one byte on the stream ends the driver's `poll`.
    pub(super) struct Waker {
        wake_tx: UnixStream,
    }

    pub(super) fn pair(_socket: &UdpSocket) -> io::Result<(Wait, Waker)> {
        let (wake_tx, wake_rx) = UnixStream::pair()?;
        wake_tx.set_nonblocking(true)?;
        wake_rx.set_nonblocking(true)?;
        Ok((Wait { wake_rx }, Waker { wake_tx }))
    }

    impl Waker {
        pub(super) fn wake(&self) {
            // A full pipe already guarantees a pending wake.
            let _ = (&self.wake_tx).write(&[0]);
        }
    }

    impl Wait {
        /// Tries `recv` first; only if the socket is empty does it sleep on
        /// socket-or-wake and try once more. The socket stays non-blocking
        /// throughout, so no mode switching is needed per burst.
        pub(super) fn recv(
            &mut self,
            socket: &UdpSocket,
            timeout: Option<Duration>,
            mut recv: impl FnMut(&UdpSocket) -> io::Result<usize>,
        ) -> io::Result<usize> {
            match nonblocking(recv(socket))? {
                0 => {}
                n => return Ok(n),
            }
            let Some(timeout) = timeout else {
                return Ok(0);
            };
            fast_io::readiness::wait_any_readable([socket.as_fd(), self.wake_rx.as_fd()], timeout)
                .or_else(interrupted)?;
            self.drain_wakes();
            nonblocking(recv(socket))
        }

        /// Unix wakes never reach the socket, so nothing is filtered.
        pub(super) fn filter(&self, meta: &mut [quinn_udp::RecvMeta]) -> usize {
            meta.len()
        }

        pub(super) fn writable(&self, socket: &UdpSocket) -> io::Result<()> {
            fast_io::readiness::wait_writable(socket.as_fd(), SEND_WAIT)
                .or_else(interrupted)
                .map(drop)
        }

        fn drain_wakes(&mut self) {
            let mut sink = [0u8; 64];
            while matches!(self.wake_rx.read(&mut sink), Ok(n) if n > 0) {}
        }
    }

    fn nonblocking(result: io::Result<usize>) -> io::Result<usize> {
        match result {
            Err(e) if is_transient(&e) => Ok(0),
            other => other,
        }
    }

    pub(super) fn is_message_too_large(e: &io::Error) -> bool {
        fast_io::readiness::is_message_too_large(e)
    }

    fn interrupted(e: io::Error) -> io::Result<bool> {
        if e.kind() == io::ErrorKind::Interrupted {
            Ok(false)
        } else {
            Err(e)
        }
    }

    /// Nothing queued, a signal, or an ICMP-derived error for an earlier
    /// send: none ends the endpoint; QUIC's loss handling covers the drop.
    fn is_transient(e: &io::Error) -> bool {
        matches!(
            e.kind(),
            io::ErrorKind::WouldBlock
                | io::ErrorKind::Interrupted
                | io::ErrorKind::ConnectionRefused
                | io::ErrorKind::ConnectionReset
        )
    }
}

#[cfg(not(unix))]
mod sys {
    use std::io;
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, UdpSocket};
    use std::time::Duration;

    /// Driver-side half: a blocking socket with a cached receive timeout,
    /// plus the wake socket's address so its datagrams are discarded.
    pub(super) struct Wait {
        wake_addr: SocketAddr,
        current_timeout: Option<Duration>,
    }

    /// Facade-side half: a loopback socket connected to the endpoint.
    pub(super) struct Waker {
        wake: UdpSocket,
    }

    pub(super) fn pair(socket: &UdpSocket) -> io::Result<(Wait, Waker)> {
        // quinn-udp leaves the socket non-blocking; the timed receive below
        // needs blocking mode, and a blocking send doubles as back-pressure.
        socket.set_nonblocking(false)?;
        let local = socket.local_addr()?;
        let loopback = match local.ip() {
            IpAddr::V4(_) => IpAddr::V4(Ipv4Addr::LOCALHOST),
            IpAddr::V6(_) => IpAddr::V6(Ipv6Addr::LOCALHOST),
        };
        let wake = UdpSocket::bind(SocketAddr::new(loopback, 0))?;
        let target = if local.ip().is_unspecified() {
            SocketAddr::new(loopback, local.port())
        } else {
            local
        };
        wake.connect(target)?;
        let wait = Wait {
            wake_addr: wake.local_addr()?,
            current_timeout: None,
        };
        Ok((wait, Waker { wake }))
    }

    impl Waker {
        pub(super) fn wake(&self) {
            let _ = self.wake.send(&[0]);
        }
    }

    impl Wait {
        pub(super) fn recv(
            &mut self,
            socket: &UdpSocket,
            timeout: Option<Duration>,
            mut recv: impl FnMut(&UdpSocket) -> io::Result<usize>,
        ) -> io::Result<usize> {
            let result = match timeout {
                Some(timeout) => {
                    // Quantize to whole milliseconds, never past the deadline,
                    // so bulk transfers do not re-issue setsockopt per wait.
                    let ms = u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX);
                    let ms = if ms >= 20 { ms - ms % 20 } else { ms.max(1) };
                    let timeout = Duration::from_millis(ms);
                    if self.current_timeout != Some(timeout) {
                        socket.set_read_timeout(Some(timeout))?;
                        self.current_timeout = Some(timeout);
                    }
                    recv(socket)
                }
                None => {
                    socket.set_nonblocking(true)?;
                    let result = recv(socket);
                    socket.set_nonblocking(false)?;
                    result
                }
            };
            match result {
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::WouldBlock
                            | io::ErrorKind::TimedOut
                            | io::ErrorKind::Interrupted
                            | io::ErrorKind::ConnectionRefused
                            | io::ErrorKind::ConnectionReset
                    ) =>
                {
                    Ok(0)
                }
                other => other,
            }
        }

        /// Drops wake datagrams (compacting the batch) and returns the
        /// number of peer datagrams left.
        pub(super) fn filter(&self, meta: &mut [quinn_udp::RecvMeta]) -> usize {
            let mut kept = 0;
            for i in 0..meta.len() {
                if meta[i].addr != self.wake_addr {
                    meta.swap(kept, i);
                    kept += 1;
                }
            }
            kept
        }

        pub(super) fn writable(&self, _socket: &UdpSocket) -> io::Result<()> {
            // The socket is blocking here, so WouldBlock does not occur.
            Ok(())
        }
    }

    /// Winsock's `WSAEMSGSIZE`, which std exposes only as a raw code.
    const WSAEMSGSIZE: i32 = 10040;

    pub(super) fn is_message_too_large(e: &io::Error) -> bool {
        e.raw_os_error() == Some(WSAEMSGSIZE)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, SocketAddr};

    fn bound() -> UdpSocket {
        UdpSocket::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0))).expect("bind")
    }

    /// A payload sent through the GSO-capable path arrives intact and in
    /// order, whether or not the host coalesces it on receive.
    #[test]
    fn gso_train_arrives_as_separate_datagrams() {
        let (tx, _tx_waker) = UdpIo::new(bound(), 1472).expect("tx");
        let (mut rx, _rx_waker) = UdpIo::new(bound(), 1472).expect("rx");
        let dest = rx.socket.local_addr().expect("addr");
        let segments = tx.max_transmit_segments().min(3);
        let payload: Vec<u8> = (0..segments * 100).map(|i| (i / 100) as u8).collect();
        let t = quinn_proto::Transmit {
            destination: dest,
            ecn: None,
            size: payload.len(),
            segment_size: (segments > 1).then_some(100),
            src_ip: None,
        };
        assert!(matches!(
            tx.send(&t, &payload).expect("send"),
            Sent::Delivered
        ));
        let mut got = Vec::new();
        while got.len() < segments {
            let batch = rx.recv(Some(Duration::from_secs(5))).expect("recv");
            assert!(!batch.is_empty(), "datagram lost on loopback");
            got.extend(batch.datagrams().map(|(_, d)| d.to_vec()));
        }
        assert_eq!(got.len(), segments);
        for (i, d) in got.iter().enumerate() {
            assert_eq!(d.len(), 100);
            assert!(d.iter().all(|&b| b == i as u8));
        }
    }

    /// The facade's wake must end a sleep promptly even with no traffic;
    /// otherwise queued writes wait out the full timer.
    #[test]
    fn wake_ends_a_sleep_without_yielding_a_datagram() {
        let (mut rx, waker) = UdpIo::new(bound(), 1472).expect("rx");
        waker.wake();
        let start = std::time::Instant::now();
        let batch = rx.recv(Some(Duration::from_secs(5))).expect("recv");
        assert!(batch.is_empty());
        assert!(start.elapsed() < Duration::from_secs(4));
    }

    /// quinn-proto ends a GSO train with a short datagram whenever the last
    /// packet is smaller than the segment size. That tail must arrive as its
    /// own datagram at its own length, not padded or merged into the one
    /// before it, or the peer cannot decrypt either packet.
    #[test]
    fn short_final_segment_arrives_as_its_own_datagram() {
        let (tx, _tx_waker) = UdpIo::new(bound(), 1472).expect("tx");
        let (mut rx, _rx_waker) = UdpIo::new(bound(), 1472).expect("rx");
        let dest = rx.socket.local_addr().expect("addr");
        let lens: &[usize] = if tx.max_transmit_segments() >= 3 {
            &[100, 100, 37]
        } else {
            &[37]
        };
        let payload: Vec<u8> = lens
            .iter()
            .enumerate()
            .flat_map(|(i, &len)| std::iter::repeat_n(i as u8, len))
            .collect();
        let t = quinn_proto::Transmit {
            destination: dest,
            ecn: None,
            size: payload.len(),
            segment_size: (lens.len() > 1).then_some(100),
            src_ip: None,
        };
        assert!(matches!(
            tx.send(&t, &payload).expect("send"),
            Sent::Delivered
        ));
        let mut got = Vec::new();
        while got.len() < lens.len() {
            let batch = rx.recv(Some(Duration::from_secs(5))).expect("recv");
            assert!(!batch.is_empty(), "datagram lost on loopback");
            got.extend(batch.datagrams().map(|(_, d)| d.to_vec()));
        }
        let got_lens: Vec<usize> = got.iter().map(Vec::len).collect();
        assert_eq!(got_lens, lens);
        for (i, d) in got.iter().enumerate() {
            assert!(d.iter().all(|&b| b == i as u8), "datagram {i} corrupted");
        }
    }

    /// A GRO-coalesced receive is split at `stride`; the last chunk keeps the
    /// short remainder. Pure arithmetic over a synthetic batch, so it holds
    /// on every platform whether or not the kernel coalesces.
    #[test]
    fn gro_train_splits_at_stride_and_keeps_the_short_tail() {
        let meta = RecvMeta {
            len: 237,
            stride: 100,
            ..RecvMeta::default()
        };
        let bufs = [(0..237).map(|i| (i / 100) as u8).collect::<Vec<u8>>()];
        let batch = Batch {
            bufs: &bufs,
            meta: std::slice::from_ref(&meta),
        };
        let split: Vec<&[u8]> = batch.datagrams().map(|(_, d)| d).collect();
        assert_eq!(
            split.iter().map(|d| d.len()).collect::<Vec<_>>(),
            [100, 100, 37]
        );
        for (i, d) in split.iter().enumerate() {
            assert!(d.iter().all(|&b| b == i as u8));
        }
    }

    /// The fallback shape: one datagram per transmit, no segment size. This
    /// is all the driver sends where GSO is missing (macOS, BSD).
    #[test]
    fn single_datagram_transmit_round_trips() {
        let (tx, _tx_waker) = UdpIo::new(bound(), 1472).expect("tx");
        let (mut rx, _rx_waker) = UdpIo::new(bound(), 1472).expect("rx");
        let dest = rx.socket.local_addr().expect("addr");
        let payload = [7u8; 1200];
        let t = quinn_proto::Transmit {
            destination: dest,
            ecn: None,
            size: payload.len(),
            segment_size: None,
            src_ip: None,
        };
        assert!(matches!(
            tx.send(&t, &payload).expect("send"),
            Sent::Delivered
        ));
        let batch = rx.recv(Some(Duration::from_secs(5))).expect("recv");
        let got: Vec<&[u8]> = batch.datagrams().map(|(_, d)| d).collect();
        assert_eq!(got, [&payload[..]]);
    }

    /// Queued datagrams come back from as few receive calls as the socket
    /// allows: up to `RECV_BATCH` per call (`recvmmsg`, or GRO coalescing),
    /// one per call where the platform has neither. The rest drain in order
    /// on the following calls. Loopback delivery is synchronous, so every
    /// datagram is queued before the first receive.
    #[test]
    fn one_receive_drains_as_many_queued_datagrams_as_the_socket_batches() {
        const COUNT: usize = 5;
        let (mut rx, _rx_waker) = UdpIo::new(bound(), 1472).expect("rx");
        let dest = rx.socket.local_addr().expect("addr");
        let tx = bound();
        for i in 0..COUNT {
            tx.send_to(&[i as u8; 64], dest).expect("send");
        }
        let first: Vec<u8> = rx
            .recv(Some(Duration::from_secs(5)))
            .expect("recv")
            .datagrams()
            .map(|(_, d)| d[0])
            .collect();
        assert!(
            first.len() >= COUNT.min(RECV_BATCH),
            "one receive returned {} of {COUNT} queued datagrams with a batch of {RECV_BATCH}",
            first.len()
        );
        let mut got = first;
        while got.len() < COUNT {
            let batch = rx.recv(Some(Duration::from_secs(5))).expect("recv");
            assert!(!batch.is_empty(), "datagram lost on loopback");
            got.extend(batch.datagrams().map(|(_, d)| d[0]));
        }
        assert_eq!(got, (0..COUNT as u8).collect::<Vec<_>>());
    }

    #[test]
    fn nonblocking_recv_returns_empty_on_a_quiet_socket() {
        let (mut rx, _waker) = UdpIo::new(bound(), 1472).expect("rx");
        assert!(rx.recv(None).expect("recv").is_empty());
    }

    /// The driver's train length is the socket's reported GSO limit, capped,
    /// on every platform. Prints the offload capabilities so CI logs record
    /// what each runner's socket offers.
    #[test]
    fn transmit_segments_follow_the_reported_capability() {
        let (io, _waker) = UdpIo::new(bound(), 1472).expect("io");
        let gso = io.gso_segments();
        eprintln!(
            "udp-capabilities: os={} max_gso_segments={gso} gro_segments={} reported_gro_segments={} batch_size={BATCH_SIZE} may_fragment={}",
            std::env::consts::OS,
            io.gro_segments(),
            io.state.gro_segments(),
            io.may_fragment(),
        );
        assert_eq!(
            io.max_transmit_segments(),
            gso.clamp(1, MAX_TRANSMIT_SEGMENTS)
        );
    }

    /// Linux has offered UDP GSO in software since 4.18, whatever the NIC,
    /// and accepts IP_PMTUDISC_PROBE; a Linux driver stuck at one datagram
    /// per send, or with path-MTU discovery off, is a regression. Other
    /// platforms report their own capability and are covered above.
    #[cfg(target_os = "linux")]
    #[test]
    fn linux_offers_gso_and_the_dont_fragment_bit() {
        let (io, _waker) = UdpIo::new(bound(), 1472).expect("io");
        assert!(io.max_transmit_segments() > 1);
        assert!(
            !io.may_fragment(),
            "IP_PMTUDISC_PROBE must be accepted on Linux"
        );
    }

    /// The single-datagram constructor must cap every transmit at one
    /// datagram even where the socket offers GSO; otherwise the tests that
    /// rely on it would silently exercise the train path instead.
    #[test]
    fn single_datagram_socket_never_offers_a_train() {
        let (io, _waker) = UdpIo::single_datagram(bound(), 1472).expect("io");
        assert_eq!(io.max_transmit_segments(), 1);
    }

    /// Each receive buffer holds exactly one coalesced train of the
    /// effective GRO size: smaller would truncate a train, larger wastes
    /// memory per endpoint.
    #[test]
    fn receive_buffers_hold_one_train_of_the_effective_gro_size() {
        let (io, _waker) = UdpIo::new(bound(), 1472).expect("io");
        assert!(io.gro_segments() >= 1);
        assert!(io.bufs.iter().all(|b| b.len() == 1472 * io.gro_segments()));
    }

    /// Windows reports 64 GRO segments while receive coalescing stays off
    /// (quinn-rs/quinn#2041), so its buffers must be sized for one datagram.
    /// Windows-only because the over-report is specific to quinn-udp's
    /// Windows backend.
    #[cfg(windows)]
    #[test]
    fn windows_receive_buffers_hold_one_datagram_while_uro_is_off() {
        let (io, _waker) = UdpIo::new(bound(), 1472).expect("io");
        assert_eq!(io.gro_segments(), 1);
    }

    fn transmit_to(destination: SocketAddr, size: usize) -> quinn_proto::Transmit {
        quinn_proto::Transmit {
            destination,
            ecn: None,
            size,
            segment_size: None,
            src_ip: None,
        }
    }

    /// A send the kernel refuses (here: broadcast without `SO_BROADCAST`)
    /// must come back as a drop carrying its errno, not end the endpoint and
    /// not vanish the way `quinn-udp`'s `send` makes it.
    #[test]
    fn refused_send_is_a_drop_that_keeps_its_errno() {
        let (tx, _waker) = UdpIo::new(bound(), 1472).expect("tx");
        let broadcast = SocketAddr::from((Ipv4Addr::BROADCAST, 9));
        let payload = [0u8; 32];
        match tx.send(&transmit_to(broadcast, payload.len()), &payload) {
            Ok(Sent::Dropped(e)) => assert!(e.raw_os_error().is_some(), "{e}"),
            other => panic!("expected a dropped transmit, got {other:?}"),
        }
    }

    /// An oversized datagram is what an MTU probe past the path limit looks
    /// like. quinn expects to lose those, so it must not count as an error.
    #[test]
    fn oversized_datagram_counts_as_sent() {
        let (tx, _tx_waker) = UdpIo::new(bound(), 1472).expect("tx");
        let (rx, _rx_waker) = UdpIo::new(bound(), 1472).expect("rx");
        let dest = rx.socket.local_addr().expect("addr");
        let payload = vec![0u8; 70_000];
        let sent = tx
            .send(&transmit_to(dest, payload.len()), &payload)
            .expect("send");
        assert!(matches!(sent, Sent::Delivered), "{sent:?}");
    }

    fn os_error(code: i32) -> io::Error {
        io::Error::from_raw_os_error(code)
    }

    /// Each errno is traced once, however often it recurs, while the count
    /// and the last error keep moving: the trace stays readable during a
    /// stall and the timeout still names the latest cause.
    #[test]
    fn send_errors_trace_each_errno_once_and_keep_the_last() {
        let mut errors = SendErrors::default();
        assert_eq!(errors.stall_note(), None);
        errors.record(&os_error(13));
        errors.record(&os_error(13));
        errors.record(&os_error(101));
        let first: Vec<String> = errors.untraced().map(str::to_owned).collect();
        assert_eq!(first, [os_error(13).to_string(), os_error(101).to_string()]);
        errors.record(&os_error(13));
        assert_eq!(errors.untraced().count(), 0);
        errors.record(&os_error(111));
        let second: Vec<String> = errors.untraced().map(str::to_owned).collect();
        assert_eq!(second, [os_error(111).to_string()]);
        assert_eq!(
            errors.stall_note().as_deref(),
            Some(format!("5 UDP send error(s), last: {}", os_error(111)).as_str())
        );
    }
}
