//! The QUIC I/O thread: owns the UDP socket and drives the `quinn-proto`
//! state machines (datagrams, timers, transmits) plus the facade buffers.

use std::io;
use std::net::{SocketAddr, UdpSocket};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bytes::{Bytes, BytesMut};
use quinn_proto::{
    Connection, ConnectionHandle, DatagramEvent, Dir, Endpoint, Event, FinishError, ReadError,
    ReadableError, StreamEvent, StreamId, VarInt, WriteError,
};

use super::{DATAGRAM_BUF, Io, MAX_SLEEP, RECV_HIGH_WATER, Shared, Terminal, error, loopback_of};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Role {
    Client,
    Server,
}

/// Per-connection driver bookkeeping.
struct ConnDriver {
    handle: ConnectionHandle,
    conn: Connection,
    stream: Option<StreamId>,
    readable: bool,
    fin_sent: bool,
    close_sent: bool,
}

impl ConnDriver {
    fn new(handle: ConnectionHandle, conn: Connection) -> Self {
        Self {
            handle,
            conn,
            stream: None,
            readable: false,
            fin_sent: false,
            close_sent: false,
        }
    }
}

/// The I/O thread: owns the UDP socket and the quinn-proto state machines.
struct Driver {
    socket: UdpSocket,
    endpoint: Endpoint,
    conn: Option<ConnDriver>,
    shared: Arc<Shared>,
    wake_addr: SocketAddr,
    role: Role,
    /// Scratch buffer `poll_transmit`/`Endpoint::handle` write packets into.
    buf: Vec<u8>,
    /// Scratch buffer for incoming datagrams.
    recv_buf: Vec<u8>,
    /// Last read timeout applied to the socket, to skip redundant setsockopt.
    current_timeout: Option<Duration>,
}

impl Driver {
    fn run(mut self) {
        let result = self.drive();
        let mut st = self.shared.lock();
        if st.terminal.is_none() {
            if let Err(err) = result {
                st.terminal = Some(Terminal::Error(error::io_fault(&err)));
            }
        }
        st.drained = true;
        self.shared.cond.notify_all();
    }

    fn drive(&mut self) -> io::Result<()> {
        loop {
            let now = Instant::now();
            self.pump(now)?;
            if self.done() {
                return Ok(());
            }
            let deadline = self.conn.as_mut().and_then(|c| c.conn.poll_timeout());
            if let Some(d) = deadline {
                if d <= now {
                    if let Some(c) = &mut self.conn {
                        c.conn.handle_timeout(now);
                    }
                    continue;
                }
            }
            if !self.enter_sleep() {
                continue;
            }
            let received = self.recv(deadline);
            {
                let mut st = self.shared.lock();
                st.sleeping = false;
            }
            match received? {
                Some((len, from)) => {
                    let now = Instant::now();
                    if from != self.wake_addr {
                        self.handle_datagram(now, from, len)?;
                    }
                    self.recv_burst(now)?;
                }
                None => {
                    if let Some(d) = deadline {
                        let now = Instant::now();
                        if d <= now {
                            if let Some(c) = &mut self.conn {
                                c.conn.handle_timeout(now);
                            }
                        }
                    }
                }
            }
        }
    }

    /// Runs every driver sub-step until none makes progress; the sub-steps
    /// feed each other (facade bytes become stream frames become transmits).
    fn pump(&mut self, now: Instant) -> io::Result<()> {
        loop {
            let mut progress = self.pump_facade(now);
            progress |= self.pump_events();
            progress |= self.drain_recv();
            progress |= self.flush_transmits(now)?;
            if !progress {
                return Ok(());
            }
        }
    }

    fn done(&self) -> bool {
        match &self.conn {
            Some(c) => c.conn.is_drained(),
            None => {
                let st = self.shared.lock();
                st.shutdown
            }
        }
    }

    /// Returns `false` (and consumes the pending flag) if facade work arrived
    /// after the last pump, in which case the caller must not sleep.
    fn enter_sleep(&self) -> bool {
        let mut st = self.shared.lock();
        if st.pending {
            st.pending = false;
            return false;
        }
        st.sleeping = true;
        true
    }

    /// Applies facade intents: queued bytes, FIN, and close requests.
    fn pump_facade(&mut self, now: Instant) -> bool {
        let Some(c) = &mut self.conn else {
            return false;
        };
        let mut progress = false;
        let mut st = self.shared.lock();
        if let Some(id) = c.stream {
            while !st.send.is_empty() {
                let (front, back) = st.send.as_slices();
                let chunk: &[u8] = if front.is_empty() { back } else { front };
                match c.conn.send_stream(id).write(chunk) {
                    Ok(n) => {
                        st.send.drain(..n);
                        progress = true;
                        self.shared.cond.notify_all();
                    }
                    Err(WriteError::Blocked) => break,
                    Err(WriteError::Stopped(_) | WriteError::ClosedStream) => {
                        st.send.clear();
                        st.send_stopped = true;
                        st.send_finished = true;
                        self.shared.cond.notify_all();
                        break;
                    }
                }
            }
            if st.send_fin && st.send.is_empty() && !c.fin_sent {
                c.fin_sent = true;
                progress = true;
                match c.conn.send_stream(id).finish() {
                    Ok(()) => {}
                    Err(FinishError::Stopped(_) | FinishError::ClosedStream) => {
                        st.send_finished = true;
                        self.shared.cond.notify_all();
                    }
                }
            }
        }
        if (st.close_requested || st.shutdown) && !c.close_sent {
            c.close_sent = true;
            progress = true;
            c.conn.close(now, VarInt::from_u32(0), Bytes::new());
        }
        progress
    }

    /// Shuttles connection<->endpoint events and application events.
    fn pump_events(&mut self) -> bool {
        let Some(c) = &mut self.conn else {
            return false;
        };
        let mut progress = false;
        while let Some(event) = c.conn.poll_endpoint_events() {
            progress = true;
            if let Some(conn_event) = self.endpoint.handle_event(c.handle, event) {
                c.conn.handle_event(conn_event);
            }
        }
        while let Some(event) = c.conn.poll() {
            progress = true;
            match event {
                Event::Stream(StreamEvent::Readable { .. } | StreamEvent::Opened { .. }) => {
                    c.readable = true;
                }
                Event::Stream(StreamEvent::Finished { id } | StreamEvent::Stopped { id, .. }) => {
                    if c.stream == Some(id) {
                        let mut st = self.shared.lock();
                        st.send_finished = true;
                        self.shared.cond.notify_all();
                    }
                }
                Event::ConnectionLost { reason } => {
                    let mut st = self.shared.lock();
                    if st.terminal.is_none() {
                        st.terminal = Some(Terminal::from_loss(&reason));
                    }
                    self.shared.cond.notify_all();
                }
                _ => {}
            }
        }
        if c.stream.is_none() && !c.conn.is_handshaking() {
            let opened = match self.role {
                Role::Client => c.conn.streams().open(Dir::Bi),
                Role::Server => c.conn.streams().accept(Dir::Bi),
            };
            if let Some(id) = opened {
                c.stream = Some(id);
                c.readable = true;
                progress = true;
                let mut st = self.shared.lock();
                st.stream_ready = true;
                self.shared.cond.notify_all();
            }
        }
        progress
    }

    /// Moves ordered stream data into the facade buffer, up to the high-water
    /// mark; beyond it the data stays in quinn so flow control throttles the
    /// peer.
    fn drain_recv(&mut self) -> bool {
        let Some(c) = &mut self.conn else {
            return false;
        };
        if !c.readable {
            return false;
        }
        let Some(id) = c.stream else {
            return false;
        };
        let mut st = self.shared.lock();
        if st.recv_len >= RECV_HIGH_WATER {
            st.recv_paused = true;
            return false;
        }
        let mut progress = false;
        match c.conn.recv_stream(id).read(true) {
            Ok(mut chunks) => {
                loop {
                    if st.recv_len >= RECV_HIGH_WATER {
                        st.recv_paused = true;
                        break;
                    }
                    match chunks.next(usize::MAX) {
                        Ok(Some(chunk)) => {
                            st.recv_len += chunk.bytes.len();
                            st.recv.push_back(chunk.bytes);
                            progress = true;
                        }
                        Ok(None) => {
                            st.recv_fin = true;
                            c.readable = false;
                            progress = true;
                            break;
                        }
                        Err(ReadError::Blocked) => {
                            c.readable = false;
                            break;
                        }
                        Err(ReadError::Reset(code)) => {
                            if st.terminal.is_none() {
                                st.terminal = Some(Terminal::Error(error::stream_reset(code)));
                            }
                            c.readable = false;
                            progress = true;
                            break;
                        }
                    }
                }
                // Flow-control (MAX_STREAM_DATA) updates ride the next
                // flush_transmits pass.
                let _ = chunks.finalize();
                if progress {
                    self.shared.cond.notify_all();
                }
            }
            Err(ReadableError::ClosedStream | ReadableError::IllegalOrderedRead) => {
                c.readable = false;
            }
        }
        progress
    }

    /// Sends every packet the connection wants on the wire right now.
    fn flush_transmits(&mut self, now: Instant) -> io::Result<bool> {
        let Some(c) = &mut self.conn else {
            return Ok(false);
        };
        let mut progress = false;
        loop {
            self.buf.clear();
            // max_datagrams = 1: no GSO with a std UdpSocket.
            let Some(t) = c.conn.poll_transmit(now, 1, &mut self.buf) else {
                break;
            };
            progress = true;
            match self.socket.send_to(&self.buf[..t.size], t.destination) {
                Ok(_) => {}
                // ICMP-derived errors surface here on some platforms when the
                // peer is gone; QUIC's own loss handling covers the drop.
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::ConnectionRefused | io::ErrorKind::ConnectionReset
                    ) => {}
                Err(e) => return Err(e),
            }
        }
        Ok(progress)
    }

    /// One blocking receive, bounded by the quinn timer deadline and
    /// [`MAX_SLEEP`]. Returns `None` on timeout.
    fn recv(&mut self, deadline: Option<Instant>) -> io::Result<Option<(usize, SocketAddr)>> {
        let now = Instant::now();
        let mut wait = MAX_SLEEP;
        if let Some(d) = deadline {
            wait = wait.min(d.saturating_duration_since(now));
        }
        // Quantize to whole milliseconds (never rounding up past the
        // deadline) so bulk transfers do not re-issue setsockopt every
        // datagram; sleeping too short is always safe.
        let ms = u64::try_from(wait.as_millis()).unwrap_or(u64::MAX);
        let ms = if ms >= 20 { ms - ms % 20 } else { ms.max(1) };
        let wait = Duration::from_millis(ms);
        if self.current_timeout != Some(wait) {
            self.socket.set_read_timeout(Some(wait))?;
            self.current_timeout = Some(wait);
        }
        match self.socket.recv_from(&mut self.recv_buf) {
            Ok((len, from)) => Ok(Some((len, from))),
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock
                        | io::ErrorKind::TimedOut
                        | io::ErrorKind::Interrupted
                        | io::ErrorKind::ConnectionReset
                ) =>
            {
                Ok(None)
            }
            Err(e) => Err(e),
        }
    }

    /// After a blocking receive delivered one datagram, drains whatever else
    /// is already queued on the socket without blocking, so a burst is fed to
    /// the state machine as a batch: one pump pass (and one coalesced set of
    /// ACKs) per burst instead of per datagram.
    fn recv_burst(&mut self, now: Instant) -> io::Result<()> {
        // Bounded so a firehose peer cannot starve timers and facade writes.
        const BURST_MAX: usize = 256;
        self.socket.set_nonblocking(true)?;
        let mut result = Ok(());
        for _ in 0..BURST_MAX {
            match self.socket.recv_from(&mut self.recv_buf) {
                Ok((len, from)) => {
                    if from != self.wake_addr {
                        if let Err(err) = self.handle_datagram(now, from, len) {
                            result = Err(err);
                            break;
                        }
                    }
                }
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::WouldBlock
                            | io::ErrorKind::Interrupted
                            | io::ErrorKind::ConnectionReset
                    ) =>
                {
                    break;
                }
                Err(e) => {
                    result = Err(e);
                    break;
                }
            }
        }
        // Restore blocking mode; SO_RCVTIMEO is untouched by this flag, so
        // the cached timeout stays valid.
        self.socket.set_nonblocking(false)?;
        result
    }

    /// Feeds one datagram to the endpoint and dispatches the outcome.
    fn handle_datagram(&mut self, now: Instant, from: SocketAddr, len: usize) -> io::Result<()> {
        let data = BytesMut::from(&self.recv_buf[..len]);
        self.buf.clear();
        match self
            .endpoint
            .handle(now, from, None, None, data, &mut self.buf)
        {
            Some(DatagramEvent::ConnectionEvent(ch, event)) => {
                if let Some(c) = &mut self.conn {
                    if c.handle == ch {
                        c.conn.handle_event(event);
                    }
                }
            }
            Some(DatagramEvent::NewConnection(incoming)) => {
                if self.conn.is_some() || self.role == Role::Client {
                    // One connection per endpoint: refuse any further ones.
                    self.buf.clear();
                    let t = self.endpoint.refuse(incoming, &mut self.buf);
                    let _ = self.socket.send_to(&self.buf[..t.size], t.destination);
                } else {
                    self.buf.clear();
                    match self.endpoint.accept(incoming, now, &mut self.buf, None) {
                        Ok((handle, conn)) => {
                            self.conn = Some(ConnDriver::new(handle, conn));
                        }
                        Err(err) => {
                            if let Some(t) = err.response {
                                let _ = self.socket.send_to(&self.buf[..t.size], t.destination);
                            }
                        }
                    }
                }
            }
            Some(DatagramEvent::Response(t)) => {
                let _ = self.socket.send_to(&self.buf[..t.size], t.destination);
            }
            None => {}
        }
        Ok(())
    }
}

/// Spawns the driver thread and builds the shared facade handle.
pub(super) fn spawn_io(
    socket: UdpSocket,
    endpoint: Endpoint,
    role: Role,
    conn: Option<(ConnectionHandle, Connection)>,
) -> io::Result<Arc<Io>> {
    let local = socket.local_addr()?;
    let wake = UdpSocket::bind(SocketAddr::new(loopback_of(local.ip()), 0))?;
    let wake_addr = wake.local_addr()?;
    let target = if local.ip().is_unspecified() {
        SocketAddr::new(loopback_of(local.ip()), local.port())
    } else {
        local
    };
    wake.connect(target)?;
    let shared = Arc::new(Shared::new());
    let driver = Driver {
        socket,
        endpoint,
        conn: conn.map(|(handle, c)| ConnDriver::new(handle, c)),
        shared: Arc::clone(&shared),
        wake_addr,
        role,
        buf: Vec::with_capacity(DATAGRAM_BUF),
        recv_buf: vec![0; DATAGRAM_BUF],
        current_timeout: None,
    };
    let handle = std::thread::Builder::new()
        .name("quic-io".to_owned())
        .spawn(move || driver.run())?;
    Ok(Arc::new(Io {
        shared,
        wake,
        thread: Mutex::new(Some(handle)),
    }))
}
