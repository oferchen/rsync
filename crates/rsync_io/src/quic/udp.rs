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
//! and a `UnixDatagram` wake pair together. Windows has no `poll` over the
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
const RECV_BATCH: usize = if BATCH_SIZE < 8 { BATCH_SIZE } else { 8 };

/// One endpoint's socket and receive buffers.
pub(super) struct UdpIo {
    socket: UdpSocket,
    state: UdpSocketState,
    bufs: [Vec<u8>; RECV_BATCH],
    meta: [RecvMeta; RECV_BATCH],
    wait: platform::Wait,
}

/// Handle a facade uses to wake the driver out of [`UdpIo::recv`].
pub(super) struct Waker(platform::Waker);

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
    /// receive buffers hold that times the GRO segment count.
    pub(super) fn new(socket: UdpSocket, max_udp_payload: usize) -> io::Result<(Self, Waker)> {
        let state = UdpSocketState::new(UdpSockRef::from(&socket))?;
        let (wait, waker) = platform::pair(&socket)?;
        let buf_len = max_udp_payload * state.gro_segments();
        let io = Self {
            bufs: std::array::from_fn(|_| vec![0; buf_len]),
            meta: [RecvMeta::default(); RECV_BATCH],
            socket,
            state,
            wait,
        };
        Ok((io, Waker(waker)))
    }

    /// Whether datagrams may be fragmented in flight. Path-MTU discovery is
    /// only sound when this is `false` (the don't-fragment bit is set).
    pub(super) fn may_fragment(&self) -> bool {
        self.state.may_fragment()
    }

    /// Most datagrams one [`Self::send`] may carry: the platform's GSO limit,
    /// capped at [`MAX_TRANSMIT_SEGMENTS`]. Always at least 1.
    pub(super) fn max_transmit_segments(&self) -> usize {
        self.state
            .max_gso_segments()
            .clamp(1, MAX_TRANSMIT_SEGMENTS)
    }

    /// Sends one quinn-proto transmit (a single datagram or a GSO train).
    ///
    /// Waits for send-buffer room rather than dropping the train. Other send
    /// errors are the network's (ICMP unreachable and the like): `quinn-udp`
    /// swallows them and QUIC loss recovery covers the drop.
    pub(super) fn send(&self, t: &quinn_proto::Transmit, contents: &[u8]) -> io::Result<()> {
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
            match self.state.send(UdpSockRef::from(&self.socket), &transmit) {
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    self.wait.writable(&self.socket)?;
                }
                other => return other,
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

/// Converts a received ECN mark into quinn-proto's type.
pub(super) fn ecn_of(meta: &RecvMeta) -> Option<quinn_proto::EcnCodepoint> {
    meta.ecn
        .and_then(|e| quinn_proto::EcnCodepoint::from_bits(e as u8))
}

#[cfg(unix)]
mod platform {
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
mod platform {
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
        tx.send(&t, &payload).expect("send");
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

    #[test]
    fn nonblocking_recv_returns_empty_on_a_quiet_socket() {
        let (mut rx, _waker) = UdpIo::new(bound(), 1472).expect("rx");
        assert!(rx.recv(None).expect("recv").is_empty());
    }

    /// Linux supports UDP GSO in software even without NIC offload, so the
    /// driver must be allowed to request trains there.
    #[cfg(target_os = "linux")]
    #[test]
    fn linux_allows_multi_datagram_transmits() {
        let (io, _waker) = UdpIo::new(bound(), 1472).expect("io");
        assert!(io.max_transmit_segments() > 1);
        assert!(
            !io.may_fragment(),
            "IP_PMTUDISC_PROBE must be accepted on Linux"
        );
    }
}
