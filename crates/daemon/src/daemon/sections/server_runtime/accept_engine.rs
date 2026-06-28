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
    rx: std::sync::mpsc::Receiver<Result<(TcpStream, SocketAddr), (SocketAddr, io::Error)>>,
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
        let (tx, rx) = std::sync::mpsc::channel();
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
                                let _ = tx.send(Err((local_addr, error)));
                                break;
                            }
                            if tx.send(Ok((stream, peer_addr))).is_err() {
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
                            let _ = tx.send(Err((local_addr, error)));
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

/// Builds the accept engine for the bound listener topology.
///
/// A single bound listener uses [`SingleListenerEngine`]; multiple listeners
/// (dual-stack) use [`MultiListenerEngine`]. The choice is made once here and
/// never re-evaluated inside the loop.
fn build_accept_engine(
    mut listeners: Vec<TcpListener>,
    bound_addresses: &[SocketAddr],
    state: &AcceptLoopState<'_>,
) -> Result<Box<dyn AcceptEngine>, DaemonError> {
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
