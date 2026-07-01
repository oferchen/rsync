/// Strategy for sourcing accepted client connections in the daemon accept loop.
///
/// Selected once at accept-loop entry from the bound listener topology
/// (single-family vs dual-stack). Each implementation hides *how* the next
/// connection becomes ready - non-blocking `accept` on one listener, or an
/// acceptor-thread-per-listener fan-in for many - behind a uniform [`poll`]
/// interface. The accept loop body (signal handling, capacity refusal, worker
/// spawn) is therefore identical regardless of listener count or platform.
///
/// This is the seam the per-platform readiness engines (io_uring multishot
/// `IORING_OP_ACCEPT`, kqueue `EVFILT_READ`, IOCP `AcceptEx`) plug into without
/// touching the shared loop body.
///
/// upstream: socket.c `start_accept_loop()` runs a single `select(2)` over all
/// listener descriptors; the engine abstraction preserves that "one loop over
/// N listeners" shape while letting the readiness mechanism vary.
///
/// [`poll`]: AcceptEngine::poll
trait AcceptEngine {
    /// Polls for the next accepted connection.
    ///
    /// Blocks for at most one internal poll interval (bounding signal-flag
    /// inspection latency) before yielding control. The returned [`TcpStream`]
    /// is always in blocking mode, matching upstream's synchronous per-session
    /// I/O model.
    fn poll(&mut self) -> Result<AcceptOutcome, DaemonError>;

    /// Stops the engine, joining any acceptor threads. Idempotent.
    fn shutdown(&mut self);
}

/// Result of polling an [`AcceptEngine`].
enum AcceptOutcome {
    /// A client connection was accepted (stream already set to blocking).
    Connection(TcpStream, SocketAddr),
    /// No connection was ready within the poll interval. The engine has
    /// already waited the appropriate amount, so the caller must re-check
    /// signal flags and poll again without adding its own sleep.
    Idle,
    /// Every listener has shut down; the accept loop terminates.
    Closed,
}

/// Single-listener accept engine: non-blocking `accept` with a 50ms idle sleep.
///
/// Used when exactly one address family is bound (IPv4-only or IPv6-only). The
/// 50ms WouldBlock interval bounds first-connection latency on a quiet daemon
/// while still letting the loop body re-check signal flags promptly.
struct SingleListenerEngine {
    listener: TcpListener,
    local_addr: SocketAddr,
    log_sink: Option<SharedLogSink>,
}

impl SingleListenerEngine {
    fn new(
        listener: TcpListener,
        local_addr: SocketAddr,
        log_sink: Option<SharedLogSink>,
    ) -> Result<Self, DaemonError> {
        listener
            .set_nonblocking(true)
            .map_err(|error| bind_error(local_addr, error))?;
        Ok(Self {
            listener,
            local_addr,
            log_sink,
        })
    }
}

impl AcceptEngine for SingleListenerEngine {
    fn poll(&mut self) -> Result<AcceptOutcome, DaemonError> {
        match self.listener.accept() {
            Ok((tcp_stream, raw_peer_addr)) => {
                if let Err(error) = tcp_stream.set_nonblocking(false) {
                    if let Some(log) = self.log_sink.as_ref() {
                        let text =
                            format!("failed to set accepted socket to blocking: {error}");
                        let message = rsync_warning!(text).with_role(Role::Daemon);
                        log_message(log, &message);
                    }
                    return Ok(AcceptOutcome::Idle);
                }
                Ok(AcceptOutcome::Connection(tcp_stream, raw_peer_addr))
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                // No pending connection - sleep briefly then let the caller
                // re-check signal flags. The 50ms interval matches the
                // dual-stack engine so first-connection latency on a quiet
                // daemon is bounded by half the sleep interval.
                thread::sleep(Duration::from_millis(50));
                Ok(AcceptOutcome::Idle)
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => Ok(AcceptOutcome::Idle),
            Err(error) => Err(accept_error(self.local_addr, error)),
        }
    }

    fn shutdown(&mut self) {}
}

/// Accepted-connection item handed from an acceptor thread to the poll loop:
/// either a blocking [`TcpStream`] with its peer address, or a per-family
/// accept error tagged with the local address that produced it.
type AcceptItem = Result<(TcpStream, SocketAddr), (SocketAddr, io::Error)>;

/// Bound on the dual-stack accept-relay channel.
///
/// Each queued item holds an accepted file descriptor, so an unbounded relay
/// would let a connection flood accumulate thousands of open fds ahead of the
/// `--max-connections` admission gate (which is only consulted *after* an item
/// is dequeued), risking fd exhaustion. Bounding the relay caps in-flight
/// accepted-but-unhandled connections; a full relay makes acceptors apply
/// backpressure so the kernel listen backlog absorbs the burst instead. The
/// single-listener engine accepts inline and needs no such queue, so this only
/// applies to the dual-stack fan-in. Sized well above a typical listen backlog
/// (128) to smooth legitimate bursts while staying clear of default fd limits.
const ACCEPT_RELAY_CAPACITY: usize = 256;

/// Relays one accepted item onto the bounded dual-stack channel, applying
/// backpressure without losing shutdown responsiveness.
///
/// On a full relay the acceptor sleeps and retries so the kernel listen backlog
/// absorbs the burst, rather than blocking inside `send()` where it could not
/// observe the shutdown flag and would wedge `join()` at teardown. Returns
/// `false` if the channel has closed, or if shutdown/graceful-exit was requested
/// while waiting for capacity, signalling the acceptor thread to stop.
fn relay_accept_item(
    tx: &std::sync::mpsc::SyncSender<AcceptItem>,
    mut item: AcceptItem,
    shutdown: &AtomicBool,
    graceful_exit: &AtomicBool,
) -> bool {
    loop {
        match tx.try_send(item) {
            Ok(()) => return true,
            Err(std::sync::mpsc::TrySendError::Full(returned)) => {
                if shutdown.load(Ordering::Relaxed) || graceful_exit.load(Ordering::Relaxed) {
                    return false;
                }
                item = returned;
                thread::sleep(Duration::from_millis(50));
            }
            Err(std::sync::mpsc::TrySendError::Disconnected(_)) => return false,
        }
    }
}

/// Dual-stack accept engine: one acceptor thread per listener, fanned into an
/// MPSC channel.
///
/// Used when multiple address families are bound. Each acceptor thread runs a
/// non-blocking accept loop so it can observe the shutdown flag, and forwards
/// accepted (blocking) streams through the channel. A single family failing
/// does not tear down the daemon: the engine tracks live acceptors and only
/// escalates an accept error to a fatal exit once every family has dropped out.
///
/// upstream: socket.c `start_accept_loop()` - one busted descriptor does not
/// collapse the `select(2)` over the others.
struct MultiListenerEngine {
    rx: std::sync::mpsc::Receiver<AcceptItem>,
    acceptor_handles: Vec<thread::JoinHandle<()>>,
    shutdown: Arc<AtomicBool>,
    alive_acceptors: usize,
    log_sink: Option<SharedLogSink>,
    joined: bool,
}

impl MultiListenerEngine {
    fn new(
        listeners: Vec<TcpListener>,
        bound_addresses: &[SocketAddr],
        state: &AcceptLoopState<'_>,
    ) -> Result<Self, DaemonError> {
        let (tx, rx) = std::sync::mpsc::sync_channel::<AcceptItem>(ACCEPT_RELAY_CAPACITY);
        let shutdown = Arc::clone(&state.signal_flags.shutdown);
        let graceful_exit = Arc::clone(&state.signal_flags.graceful_exit);
        let total_acceptors = listeners.len();
        let mut acceptor_handles: Vec<thread::JoinHandle<()>> =
            Vec::with_capacity(total_acceptors);

        for (listener, local_addr) in listeners.into_iter().zip(bound_addresses.iter().copied()) {
            let tx = tx.clone();
            let shutdown = Arc::clone(&shutdown);
            let graceful_exit = Arc::clone(&graceful_exit);

            // Set non-blocking so acceptor threads can check the shutdown flag
            // without getting stuck in a blocking accept() call.
            if let Err(error) = listener.set_nonblocking(true) {
                return Err(bind_error(local_addr, error));
            }

            let handle = thread::spawn(move || {
                while !shutdown.load(Ordering::Relaxed)
                    && !graceful_exit.load(Ordering::Relaxed)
                {
                    match listener.accept() {
                        Ok((stream, peer_addr)) => {
                            // BSD-derived kernels (macOS, FreeBSD) propagate the
                            // listener's O_NONBLOCK flag to the accepted socket,
                            // which would cause the legacy handshake reader to
                            // fail with EAGAIN before the client writes its
                            // greeting. Reset to blocking so the worker thread
                            // sees the upstream-compatible synchronous I/O model.
                            if let Err(error) = stream.set_nonblocking(false) {
                                let _ =
                                    relay_accept_item(&tx, Err((local_addr, error)), &shutdown, &graceful_exit);
                                break;
                            }
                            if !relay_accept_item(&tx, Ok((stream, peer_addr)), &shutdown, &graceful_exit)
                            {
                                break;
                            }
                        }
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(50));
                            continue;
                        }
                        Err(error) if error.kind() == io::ErrorKind::Interrupted => {
                            continue;
                        }
                        Err(error) => {
                            let _ = relay_accept_item(
                                &tx,
                                Err((local_addr, error)),
                                &shutdown,
                                &graceful_exit,
                            );
                            break;
                        }
                    }
                }
            });
            acceptor_handles.push(handle);
        }

        // Drop our copy of the sender so the channel closes when acceptors exit.
        drop(tx);

        Ok(Self {
            rx,
            acceptor_handles,
            shutdown,
            alive_acceptors: total_acceptors,
            log_sink: state.log_sink.clone(),
            joined: false,
        })
    }

    fn join_acceptors(&mut self) {
        for handle in self.acceptor_handles.drain(..) {
            let _ = handle.join();
        }
        self.joined = true;
    }
}

impl AcceptEngine for MultiListenerEngine {
    fn poll(&mut self) -> Result<AcceptOutcome, DaemonError> {
        // recv_timeout allows periodic worker reaping and signal checks in the
        // shared loop body between accepted connections.
        match self.rx.recv_timeout(Duration::from_millis(100)) {
            Ok(Ok((tcp_stream, raw_peer_addr))) => {
                Ok(AcceptOutcome::Connection(tcp_stream, raw_peer_addr))
            }
            Ok(Err((local_addr, error))) => {
                // The dual-stack loop only escalates an accept error to a fatal
                // daemon exit when every family has dropped out - a single
                // family failing logs a warning and the survivors keep serving.
                self.alive_acceptors = self.alive_acceptors.saturating_sub(1);
                if self.alive_acceptors == 0 {
                    self.shutdown.store(true, Ordering::Relaxed);
                    self.join_acceptors();
                    return Err(accept_error(local_addr, error));
                }
                warn_per_family_accept_failure(self.log_sink.as_ref(), local_addr, &error);
                Ok(AcceptOutcome::Idle)
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => Ok(AcceptOutcome::Idle),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => Ok(AcceptOutcome::Closed),
        }
    }

    fn shutdown(&mut self) {
        if self.joined {
            return;
        }
        self.shutdown.store(true, Ordering::Relaxed);
        self.join_acceptors();
    }
}

/// macOS `kqueue` accept engine: one `EVFILT_READ` registration per listener,
/// readiness-driven `accept` with no per-iteration sleep.
///
/// Replaces the busy-wait shape of the portable engines (non-blocking `accept`
/// plus a 50ms `WouldBlock` sleep) with a single `kevent(2)` wait over all
/// listener fds. On a quiet daemon the thread parks in the kernel until a
/// connection arrives or the 100ms signal-check timeout elapses, so
/// first-connection latency drops from up to 50ms to a syscall round-trip while
/// still bounding shutdown-flag inspection to the same 100ms cadence the
/// dual-stack engine uses.
///
/// The listeners are set non-blocking and registered `EVFILT_READ | EV_CLEAR`
/// (edge-triggered). Because edge-triggered readiness fires once per readable
/// transition, each ready listener is drained with repeated `accept` calls until
/// `WouldBlock`, so a burst that queues several connections behind one
/// notification is never dropped. Surplus accepted streams are buffered and
/// handed out one per [`poll`](AcceptEngine::poll), preserving the "one
/// connection per poll" contract the shared loop body relies on.
///
/// Admission (`--max-connections`) and the N-listener fan-out are unchanged:
/// this engine only sources accepted streams; the shared loop body still gates
/// every returned connection through the process-global admission counter.
///
/// Selected by [`build_accept_engine`] on macOS with a graceful fallback to the
/// portable engines if `kqueue(2)` setup fails, so a kqueue error never breaks
/// connection service.
#[cfg(target_os = "macos")]
struct KqueueAcceptEngine {
    /// Registered listeners keyed by their `EVFILT_READ` user-data index.
    listeners: Vec<(TcpListener, SocketAddr)>,
    /// kqueue event surface; dropped (closing its fd) on [`Self::shutdown`].
    kq: fast_io::KqueueLoop,
    /// Accepted streams drained from a readiness burst but not yet handed out.
    pending: std::collections::VecDeque<(TcpStream, SocketAddr)>,
    log_sink: Option<SharedLogSink>,
}

#[cfg(target_os = "macos")]
impl KqueueAcceptEngine {
    /// Signal-check cadence for the `kevent(2)` wait, matching the dual-stack
    /// engine's `recv_timeout` interval so shutdown latency is identical.
    const WAIT_TIMEOUT: Duration = Duration::from_millis(100);

    /// Builds the engine, registering an `EVFILT_READ` event per listener.
    ///
    /// Returns an `io::Error` (not a [`DaemonError`]) so the caller can fall
    /// back to the portable engines on any kqueue setup failure without
    /// aborting daemon startup.
    fn new(
        listeners: Vec<TcpListener>,
        bound_addresses: &[SocketAddr],
        log_sink: Option<SharedLogSink>,
    ) -> io::Result<Self> {
        use std::os::unix::io::AsRawFd;

        let kq = fast_io::KqueueLoop::new()?;
        let mut registered: Vec<(TcpListener, SocketAddr)> = Vec::with_capacity(listeners.len());
        for (index, (listener, local_addr)) in listeners
            .into_iter()
            .zip(bound_addresses.iter().copied())
            .enumerate()
        {
            // Non-blocking so the drain-accept loop terminates on WouldBlock
            // instead of blocking after the edge-triggered notification.
            listener.set_nonblocking(true)?;
            kq.submit_read(listener.as_raw_fd(), index as u64)?;
            registered.push((listener, local_addr));
        }
        Ok(Self {
            listeners: registered,
            kq,
            pending: std::collections::VecDeque::new(),
            log_sink,
        })
    }

    /// Drains all currently-pending connections from one ready listener.
    ///
    /// Edge-triggered readiness fires once per readable transition, so a single
    /// notification may cover several queued connections. Accept until
    /// `WouldBlock` to avoid stranding a connection behind the edge until the
    /// next transition. Accepted streams are reset to blocking (BSD kernels
    /// propagate the listener's `O_NONBLOCK` to the accepted socket) and pushed
    /// onto the pending queue. Returns the local address paired with a fatal
    /// accept error, or `None` when the listener drained cleanly.
    fn drain_listener(&mut self, index: usize) -> Option<(SocketAddr, io::Error)> {
        let (listener, local_addr) = &self.listeners[index];
        let local_addr = *local_addr;
        loop {
            match listener.accept() {
                Ok((stream, peer_addr)) => {
                    if let Err(error) = stream.set_nonblocking(false) {
                        if let Some(log) = self.log_sink.as_ref() {
                            let text =
                                format!("failed to set accepted socket to blocking: {error}");
                            let message = rsync_warning!(text).with_role(Role::Daemon);
                            log_message(log, &message);
                        }
                        continue;
                    }
                    self.pending.push_back((stream, peer_addr));
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return None,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Some((local_addr, error)),
            }
        }
    }
}

#[cfg(target_os = "macos")]
impl AcceptEngine for KqueueAcceptEngine {
    fn poll(&mut self) -> Result<AcceptOutcome, DaemonError> {
        if let Some((stream, peer_addr)) = self.pending.pop_front() {
            return Ok(AcceptOutcome::Connection(stream, peer_addr));
        }

        let events = match self.kq.wait(Some(Self::WAIT_TIMEOUT)) {
            Ok(events) => events,
            // EINTR is folded into an empty result by KqueueLoop::wait; any
            // other kevent failure means the readiness surface is unusable.
            // Surface it against the first listener so the loop body exits
            // rather than spinning on a broken kqueue.
            Err(error) => return Err(accept_error(self.listeners[0].1, error)),
        };
        if events.is_empty() {
            // Timeout with no readiness: let the caller re-check signal flags.
            return Ok(AcceptOutcome::Idle);
        }

        let mut fatal: Option<(SocketAddr, io::Error)> = None;
        for event in events {
            let index = event.user_data as usize;
            if index >= self.listeners.len() {
                continue;
            }
            if let Some(err) = self.drain_listener(index) {
                fatal.get_or_insert(err);
            }
        }

        if let Some((stream, peer_addr)) = self.pending.pop_front() {
            return Ok(AcceptOutcome::Connection(stream, peer_addr));
        }
        if let Some((local_addr, error)) = fatal {
            return Err(accept_error(local_addr, error));
        }
        Ok(AcceptOutcome::Idle)
    }

    fn shutdown(&mut self) {
        // The KqueueLoop closes its fd on drop; there are no acceptor threads to
        // join. Clearing the listeners drops their fds too, matching the
        // portable engines' teardown. Idempotent: a second call finds both empty.
        self.pending.clear();
        self.listeners.clear();
    }
}

/// Attempts to build the macOS kqueue accept engine.
///
/// Returns `Ok(Some(engine))` on success, `Ok(None)` if kqueue setup fails so
/// the caller falls back to the portable engines, threading `listeners` back out
/// unchanged on failure. Any kqueue error is non-fatal: connection service must
/// continue through the blocking engine.
#[cfg(target_os = "macos")]
fn try_build_kqueue_engine(
    listeners: Vec<TcpListener>,
    bound_addresses: &[SocketAddr],
    state: &AcceptLoopState<'_>,
) -> Result<Box<dyn AcceptEngine>, Vec<TcpListener>> {
    // Clone the listeners up front so a mid-registration failure can hand the
    // originals back to the fallback path untouched.
    let mut clones: Vec<TcpListener> = Vec::with_capacity(listeners.len());
    for listener in &listeners {
        match listener.try_clone() {
            Ok(clone) => clones.push(clone),
            Err(_) => return Err(listeners),
        }
    }
    match KqueueAcceptEngine::new(clones, bound_addresses, state.log_sink.clone()) {
        Ok(engine) => Ok(Box::new(engine)),
        Err(error) => {
            if let Some(log) = state.log_sink.as_ref() {
                let text =
                    format!("kqueue accept engine unavailable, using blocking accept: {error}");
                let message = rsync_info!(text).with_role(Role::Daemon);
                log_message(log, &message);
            }
            Err(listeners)
        }
    }
}

/// Builds the accept engine for the bound listener topology.
///
/// On macOS a [`KqueueAcceptEngine`] is tried first, falling back to the
/// portable engines if `kqueue(2)` setup fails. Otherwise a single bound
/// listener uses [`SingleListenerEngine`]; multiple listeners (dual-stack) use
/// [`MultiListenerEngine`]. The choice is made once here and never re-evaluated
/// inside the loop.
fn build_accept_engine(
    listeners: Vec<TcpListener>,
    bound_addresses: &[SocketAddr],
    state: &AcceptLoopState<'_>,
) -> Result<Box<dyn AcceptEngine>, DaemonError> {
    #[cfg(target_os = "macos")]
    let listeners = match try_build_kqueue_engine(listeners, bound_addresses, state) {
        Ok(engine) => return Ok(engine),
        Err(listeners) => listeners,
    };

    let mut listeners = listeners;
    if listeners.len() == 1 {
        let listener = listeners.remove(0);
        let engine =
            SingleListenerEngine::new(listener, bound_addresses[0], state.log_sink.clone())?;
        Ok(Box::new(engine))
    } else {
        let engine = MultiListenerEngine::new(listeners, bound_addresses, state)?;
        Ok(Box::new(engine))
    }
}

/// Drives the daemon accept loop over an [`AcceptEngine`].
///
/// The loop body is identical regardless of engine: check signal flags, poll
/// for the next connection, and dispatch it through the shared admission and
/// worker-spawn path. Polling cadence and readiness mechanism are entirely the
/// engine's concern.
fn run_accept_loop(
    engine: &mut dyn AcceptEngine,
    state: &mut AcceptLoopState<'_>,
) -> Result<(), DaemonError> {
    loop {
        if let Some(true) = check_signals_and_maintain(state)? {
            break;
        }

        match engine.poll()? {
            AcceptOutcome::Connection(tcp_stream, raw_peer_addr) => {
                if handle_accepted_connection(tcp_stream, raw_peer_addr, state) {
                    break;
                }
            }
            AcceptOutcome::Idle => continue,
            AcceptOutcome::Closed => break,
        }
    }

    engine.shutdown();
    Ok(())
}
