//! The QUIC I/O thread: owns the UDP socket and drives the `quinn-proto`
//! state machines (datagrams, timers, transmits) plus the facade buffers.
//!
//! One driver serves every connection on its endpoint. A client endpoint
//! carries exactly the one connection it dialled; a server endpoint accepts
//! any number, each with its own facade [`Shared`] state, and hands each to
//! [`QuicAcceptor::accept`](super::QuicAcceptor::accept) once its
//! bidirectional stream exists.

use std::io;
use std::net::{IpAddr, SocketAddr, UdpSocket};
use std::sync::Arc;
use std::time::Instant;

use bytes::{Bytes, BytesMut};
use quinn_proto::{
    Connection, ConnectionError, ConnectionHandle, DatagramEvent, Dir, EcnCodepoint, Endpoint,
    EndpointConfig, Event, FinishError, ReadError, ReadableError, ServerConfig, StreamEvent,
    StreamId, Transmit, VarInt, WriteError,
};

use super::udp::{self, UdpIo, Waker};
use super::{DATAGRAM_BUF, Hub, MAX_SLEEP, RECV_HIGH_WATER, Shared, Terminal, error};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Role {
    Client,
    Server,
}

/// Connections a server endpoint holds that the application has not yet
/// accepted: those still handshaking plus those queued for
/// [`QuicAcceptor::accept`](super::QuicAcceptor::accept). Further incoming
/// connections are refused until the backlog drains, the QUIC counterpart of
/// a TCP listener's `listen(2)` backlog, so a peer flooding handshakes cannot
/// grow the endpoint's state without bound.
const ACCEPT_BACKLOG: usize = 128;

/// Datagrams handled per wake-up before the driver services timers and facade
/// writes again, so a firehose peer cannot starve them.
const BURST_MAX: usize = 256;

/// One received datagram, copied out of the receive batch.
struct Incoming {
    from: SocketAddr,
    local_ip: Option<IpAddr>,
    ecn: Option<EcnCodepoint>,
    data: BytesMut,
}

/// Per-connection driver bookkeeping.
struct ConnDriver {
    handle: ConnectionHandle,
    conn: Connection,
    shared: Arc<Shared>,
    stream: Option<StreamId>,
    readable: bool,
    fin_sent: bool,
    close_sent: bool,
}

impl ConnDriver {
    fn new(handle: ConnectionHandle, conn: Connection, shared: Arc<Shared>) -> Self {
        Self {
            handle,
            conn,
            shared,
            stream: None,
            readable: false,
            fin_sent: false,
            close_sent: false,
        }
    }

    /// Marks this connection's facade as finished: records `fault` as the
    /// terminal state unless one is already set, then wakes every waiter.
    fn release(&self, fault: Option<&error::TransportFault>) {
        let mut st = self.shared.lock();
        if st.terminal.is_none()
            && let Some(fault) = fault
        {
            st.terminal = Some(Terminal::Error(fault.clone()));
        }
        st.drained = true;
        self.shared.cond.notify_all();
    }
}

/// The I/O thread: owns the UDP socket and the quinn-proto state machines.
struct Driver {
    udp: UdpIo,
    endpoint: Endpoint,
    conns: Vec<ConnDriver>,
    hub: Arc<Hub>,
    role: Role,
    /// Whether this endpoint OPENS each connection's single bidirectional
    /// stream (speaks first) rather than accepting a peer-opened one. Both
    /// client-first (request/reply) and server-first (rsync daemon greeting)
    /// exchanges need the speaker to be the opener to avoid the
    /// frameless-stream deadlock.
    opens_stream: bool,
    /// Scratch buffer `poll_transmit`/`Endpoint::handle` write packets into.
    buf: Vec<u8>,
    /// Datagrams of the current receive batch, reused across batches.
    inbox: Vec<Incoming>,
    /// Lowest GSO segment limit seen so far, published to the hub so a
    /// facade thread can trace a runtime fallback.
    gso_segments: usize,
}

impl Driver {
    fn run(mut self) {
        let fault = self.drive().err().map(|err| error::io_fault(&err));
        for c in &self.conns {
            c.release(fault.as_ref());
        }
        let mut hub = self.hub.lock();
        hub.driver_exited = true;
        hub.fault = fault;
        self.hub.cond.notify_all();
    }

    fn drive(&mut self) -> io::Result<()> {
        loop {
            let now = Instant::now();
            self.pump(now)?;
            self.release_drained();
            if self.done() {
                return Ok(());
            }
            let deadline = self.next_deadline();
            if let Some(d) = deadline
                && d <= now
            {
                self.fire_timeouts(now);
                continue;
            }
            if !self.enter_sleep() {
                continue;
            }
            // One sleep bounded by the earliest quinn timer and MAX_SLEEP,
            // then whatever else is queued, as one batch.
            let wait = deadline.map_or(MAX_SLEEP, |d| {
                MAX_SLEEP.min(d.saturating_duration_since(Instant::now()))
            });
            let received = self.receive(wait);
            self.hub.lock().sleeping = false;
            received?;
            let now = Instant::now();
            if deadline.is_some_and(|d| d <= now) {
                self.fire_timeouts(now);
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

    /// Drops every connection that has fully drained, telling its facade.
    fn release_drained(&mut self) {
        self.conns.retain(|c| {
            if c.conn.is_drained() {
                c.release(None);
                false
            } else {
                true
            }
        });
    }

    /// A client endpoint exits once its one connection has drained; a server
    /// endpoint exits once its acceptor is gone and every connection it
    /// accepted has drained.
    fn done(&self) -> bool {
        if !self.conns.is_empty() {
            return false;
        }
        match self.role {
            Role::Client => true,
            Role::Server => self.hub.lock().accept_closed,
        }
    }

    fn next_deadline(&mut self) -> Option<Instant> {
        self.conns
            .iter_mut()
            .filter_map(|c| c.conn.poll_timeout())
            .min()
    }

    fn fire_timeouts(&mut self, now: Instant) {
        for c in &mut self.conns {
            if c.conn.poll_timeout().is_some_and(|t| t <= now) {
                c.conn.handle_timeout(now);
            }
        }
    }

    /// Returns `false` (and consumes the pending flag) if facade work arrived
    /// after the last pump, in which case the caller must not sleep.
    fn enter_sleep(&self) -> bool {
        let mut hub = self.hub.lock();
        if hub.pending {
            hub.pending = false;
            return false;
        }
        hub.sleeping = true;
        true
    }

    /// Applies facade intents on every connection: queued bytes, FIN, and
    /// close requests.
    fn pump_facade(&mut self, now: Instant) -> bool {
        // A server whose acceptor is gone closes every connection nobody will
        // ever accept; those already handed out keep running.
        let orphan_unaccepted = self.role == Role::Server && self.hub.lock().accept_closed;
        let mut progress = false;
        for c in &mut self.conns {
            let mut st = c.shared.lock();
            if let Some(id) = c.stream {
                while !st.send.is_empty() {
                    let (front, back) = st.send.as_slices();
                    let chunk: &[u8] = if front.is_empty() { back } else { front };
                    match c.conn.send_stream(id).write(chunk) {
                        Ok(n) => {
                            st.send.drain(..n);
                            progress = true;
                            c.shared.cond.notify_all();
                        }
                        Err(WriteError::Blocked) => break,
                        Err(WriteError::Stopped(_) | WriteError::ClosedStream) => {
                            st.send.clear();
                            st.send_stopped = true;
                            st.send_finished = true;
                            c.shared.cond.notify_all();
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
                            c.shared.cond.notify_all();
                        }
                    }
                }
            }
            let orphaned = orphan_unaccepted && !st.accepted;
            if (st.close_requested || st.shutdown || orphaned) && !c.close_sent {
                c.close_sent = true;
                progress = true;
                c.conn.close(now, VarInt::from_u32(0), Bytes::new());
            }
        }
        progress
    }

    /// Shuttles connection<->endpoint events and application events.
    fn pump_events(&mut self) -> bool {
        let mut progress = false;
        for c in &mut self.conns {
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
                    Event::Stream(
                        StreamEvent::Finished { id } | StreamEvent::Stopped { id, .. },
                    ) => {
                        if c.stream == Some(id) {
                            let mut st = c.shared.lock();
                            st.send_finished = true;
                            c.shared.cond.notify_all();
                        }
                    }
                    Event::ConnectionLost { reason } => {
                        let mut st = c.shared.lock();
                        if st.terminal.is_none() {
                            let mut terminal = Terminal::from_loss(&reason);
                            // An idle timeout after refused sends names the
                            // send errno, the likeliest cause of the silence.
                            if let (ConnectionError::TimedOut, Terminal::Error(fault)) =
                                (&reason, &mut terminal)
                                && let Some(note) = self.hub.send_errors().stall_note()
                            {
                                fault.append(&note);
                            }
                            st.terminal = Some(terminal);
                        }
                        c.shared.cond.notify_all();
                    }
                    _ => {}
                }
            }
            if c.stream.is_none() && !c.conn.is_handshaking() {
                // A QUIC stream only becomes visible to its peer once a frame
                // is sent on it, so the party that speaks first must be the
                // one that OPENS the single bidirectional stream - otherwise
                // the peer's `accept` blocks on a frameless stream while the
                // opener waits to read, and both sides deadlock. The rsync
                // daemon is server-speaks-first (it writes the `@RSYNCD:`
                // greeting before the client sends anything), so its acceptor
                // opens and the client connecting to it accepts.
                let opened = if self.opens_stream {
                    c.conn.streams().open(Dir::Bi)
                } else {
                    c.conn.streams().accept(Dir::Bi)
                };
                if let Some(id) = opened {
                    c.stream = Some(id);
                    c.readable = true;
                    progress = true;
                    let peer = c.conn.remote_address();
                    {
                        let mut st = c.shared.lock();
                        st.stream_ready = true;
                        st.peer = Some(peer);
                        // A client endpoint's driver runs one connection, so
                        // the suite it last keyed is the negotiated one.
                        if self.role == Role::Client {
                            st.negotiated_suite = super::cipher::keyed_suite();
                        }
                        c.shared.cond.notify_all();
                    }
                    if self.role == Role::Server {
                        let mut hub = self.hub.lock();
                        if !hub.accept_closed {
                            hub.queue.push_back(Arc::clone(&c.shared));
                            self.hub.cond.notify_all();
                        }
                    }
                }
            }
        }
        progress
    }

    /// Moves ordered stream data into each facade buffer, up to the
    /// high-water mark; beyond it the data stays in quinn so flow control
    /// throttles the peer.
    fn drain_recv(&mut self) -> bool {
        let mut progress = false;
        for c in &mut self.conns {
            if !c.readable {
                continue;
            }
            let Some(id) = c.stream else {
                continue;
            };
            let mut st = c.shared.lock();
            if st.recv_len >= RECV_HIGH_WATER {
                st.recv_paused = true;
                continue;
            }
            let mut delivered = false;
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
                                delivered = true;
                            }
                            Ok(None) => {
                                st.recv_fin = true;
                                c.readable = false;
                                delivered = true;
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
                                delivered = true;
                                break;
                            }
                        }
                    }
                    // Flow-control (MAX_STREAM_DATA) updates ride the next
                    // flush_transmits pass.
                    let _ = chunks.finalize();
                    if delivered {
                        c.shared.cond.notify_all();
                    }
                }
                Err(ReadableError::ClosedStream | ReadableError::IllegalOrderedRead) => {
                    c.readable = false;
                }
            }
            progress |= delivered;
        }
        progress
    }

    /// Sends every packet each connection wants on the wire right now.
    fn flush_transmits(&mut self, now: Instant) -> io::Result<bool> {
        let mut progress = false;
        // quinn-udp drops to one segment when the kernel rejects a GSO send,
        // and its own log of that is compiled out.
        let gso = self.udp.gso_segments();
        if gso < self.gso_segments {
            self.gso_segments = gso;
            self.hub
                .gso_segments
                .store(gso, std::sync::atomic::Ordering::Relaxed);
        }
        // Several datagrams per transmit: a GSO train leaves in one sendmsg.
        let max_datagrams = self.udp.max_transmit_segments();
        for c in &mut self.conns {
            loop {
                self.buf.clear();
                let Some(t) = c.conn.poll_transmit(now, max_datagrams, &mut self.buf) else {
                    break;
                };
                progress = true;
                #[cfg(test)]
                if t.segment_size.is_some() {
                    self.hub
                        .gso_trains
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
                transmit(&self.udp, &self.hub, &t, &self.buf[..t.size])?;
            }
        }
        Ok(progress)
    }

    /// Sleeps up to `wait` (or until woken) for datagrams, then keeps
    /// draining the socket without blocking, up to [`BURST_MAX`] datagrams.
    /// Feeding a burst to the state machines as a batch gives one pump pass
    /// and one coalesced set of ACKs per burst instead of per datagram.
    fn receive(&mut self, wait: std::time::Duration) -> io::Result<()> {
        let mut timeout = Some(wait);
        let mut inbox = std::mem::take(&mut self.inbox);
        let mut handled = 0;
        let result = loop {
            if handled >= BURST_MAX {
                break Ok(());
            }
            match self.udp.recv(timeout.take()) {
                Ok(batch) if batch.is_empty() => break Ok(()),
                Ok(batch) => inbox.extend(batch.datagrams().map(|(meta, data)| Incoming {
                    from: meta.addr,
                    local_ip: meta.dst_ip,
                    ecn: udp::ecn_of(meta),
                    data: BytesMut::from(data),
                })),
                Err(err) => break Err(err),
            }
            handled += inbox.len();
            let now = Instant::now();
            if let Err(err) = inbox
                .drain(..)
                .try_for_each(|incoming| self.handle_datagram(now, incoming))
            {
                break Err(err);
            }
        };
        inbox.clear();
        self.inbox = inbox;
        result
    }

    /// Whether a server endpoint may take on another connection now.
    fn has_backlog_room(&self) -> bool {
        let unaccepted = self
            .conns
            .iter()
            .filter(|c| !c.shared.lock().accepted)
            .count();
        unaccepted < ACCEPT_BACKLOG
    }

    /// Feeds one datagram to the endpoint and dispatches the outcome.
    fn handle_datagram(&mut self, now: Instant, incoming: Incoming) -> io::Result<()> {
        self.buf.clear();
        match self.endpoint.handle(
            now,
            incoming.from,
            incoming.local_ip,
            incoming.ecn,
            incoming.data,
            &mut self.buf,
        ) {
            Some(DatagramEvent::ConnectionEvent(ch, event)) => {
                if let Some(c) = self.conns.iter_mut().find(|c| c.handle == ch) {
                    c.conn.handle_event(event);
                }
            }
            Some(DatagramEvent::NewConnection(incoming)) => {
                let admit = self.role == Role::Server
                    && self.has_backlog_room()
                    && !self.hub.lock().accept_closed;
                self.buf.clear();
                if admit {
                    match self.endpoint.accept(incoming, now, &mut self.buf, None) {
                        Ok((handle, conn)) => {
                            let shared = Arc::new(Shared::new());
                            self.conns.push(ConnDriver::new(handle, conn, shared));
                        }
                        Err(err) => {
                            if let Some(t) = err.response {
                                transmit(&self.udp, &self.hub, &t, &self.buf[..t.size])?;
                            }
                        }
                    }
                } else {
                    let t = self.endpoint.refuse(incoming, &mut self.buf);
                    transmit(&self.udp, &self.hub, &t, &self.buf[..t.size])?;
                }
            }
            Some(DatagramEvent::Response(t)) => {
                transmit(&self.udp, &self.hub, &t, &self.buf[..t.size])?;
            }
            None => {}
        }
        Ok(())
    }
}

/// Sends one transmit. A transmit the kernel refuses is recorded on the hub
/// and left to QUIC loss recovery, as `quinn-udp`'s own `send` would; only a
/// failure of the socket wait itself ends the driver.
fn transmit(udp: &UdpIo, hub: &Hub, t: &Transmit, contents: &[u8]) -> io::Result<()> {
    if let udp::Sent::Dropped(e) = udp.send(t, contents)? {
        hub.send_errors().record(&e);
    }
    Ok(())
}

/// Takes over `socket` and builds the quinn-proto endpoint on it.
///
/// Path-MTU discovery is enabled exactly when the socket sets the
/// don't-fragment bit: an unprotected probe could otherwise be fragmented in
/// flight and falsely validate a size the path cannot carry unfragmented.
pub(super) fn bind_endpoint(
    socket: UdpSocket,
    server_config: Option<Arc<ServerConfig>>,
) -> io::Result<(Endpoint, UdpIo, Waker)> {
    let config = Arc::new(EndpointConfig::default());
    let max_udp_payload = usize::try_from(config.get_max_udp_payload_size())
        .unwrap_or(DATAGRAM_BUF)
        .min(DATAGRAM_BUF);
    #[cfg(not(test))]
    let (udp, waker) = UdpIo::new(socket, max_udp_payload)?;
    #[cfg(test)]
    let (udp, waker) = if SINGLE_DATAGRAM.get() {
        UdpIo::single_datagram(socket, max_udp_payload)?
    } else {
        UdpIo::new(socket, max_udp_payload)?
    };
    logging::debug_log!(
        Connect,
        1,
        "quic udp offload: gso={} gro={} batch={} may_fragment={}",
        udp.gso_segments(),
        udp.gro_segments(),
        udp::RECV_BATCH,
        udp.may_fragment()
    );
    let endpoint = Endpoint::new(config, server_config, !udp.may_fragment(), None);
    Ok((endpoint, udp, waker))
}

#[cfg(test)]
thread_local! {
    /// Endpoints bound on this thread never build GSO trains; see
    /// [`UdpIo::single_datagram`].
    pub(super) static SINGLE_DATAGRAM: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Spawns the driver thread and builds the shared endpoint handle.
///
/// `conn` is the connection a client endpoint dialled, paired with the facade
/// state its stream will use; a server endpoint passes `None` and accepts its
/// connections as they arrive.
pub(super) fn spawn_io(
    (endpoint, udp, waker): (Endpoint, UdpIo, Waker),
    role: Role,
    opens_stream: bool,
    conn: Option<(ConnectionHandle, Connection, Arc<Shared>)>,
) -> io::Result<Arc<Hub>> {
    let gso_segments = udp.gso_segments();
    let hub = Arc::new(Hub::new(waker, role, gso_segments));
    let driver = Driver {
        udp,
        endpoint,
        conns: conn
            .map(|(handle, c, shared)| ConnDriver::new(handle, c, shared))
            .into_iter()
            .collect(),
        hub: Arc::clone(&hub),
        role,
        opens_stream,
        buf: Vec::with_capacity(DATAGRAM_BUF),
        inbox: Vec::new(),
        gso_segments,
    };
    let handle = std::thread::Builder::new()
        .name("quic-io".to_owned())
        .spawn(move || driver.run())?;
    hub.set_thread(handle);
    Ok(hub)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    /// A send the kernel refuses (broadcast without `SO_BROADCAST`) must not
    /// end the driver. It is counted on the hub, its errno is traced once
    /// however often it recurs, and a later idle timeout can name it.
    #[test]
    fn refused_transmit_is_recorded_not_fatal() {
        let socket = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind");
        let (udp, waker) = UdpIo::new(socket, 1472).expect("udp");
        let hub = Hub::new(waker, Role::Client, udp.gso_segments());
        let payload = [0u8; 32];
        let t = Transmit {
            destination: SocketAddr::from((Ipv4Addr::BROADCAST, 9)),
            ecn: None,
            size: payload.len(),
            segment_size: None,
            src_ip: None,
        };
        transmit(&udp, &hub, &t, &payload).expect("first refused send is not fatal");
        transmit(&udp, &hub, &t, &payload).expect("second refused send is not fatal");
        let mut errors = hub.send_errors();
        assert_eq!(errors.untraced().count(), 1);
        let note = errors.stall_note().expect("refusals recorded");
        assert!(note.starts_with("2 UDP send error(s), last: "), "{note}");
    }
}
