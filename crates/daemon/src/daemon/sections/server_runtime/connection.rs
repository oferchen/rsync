/// Shared state for the connection accept loops.
///
/// Groups the mutable and immutable context needed by both the single-listener
/// and dual-stack accept loops, avoiding excessive parameter lists.
struct AcceptLoopState<'a> {
    signal_flags: &'a SignalFlags,
    workers: Vec<SessionWorker>,
    served: usize,
    active_connections: usize,
    connection_counter: ConnectionCounter,
    start_time: SystemTime,
    max_sessions: Option<usize>,
    /// Concurrent connection cap consulted by the accept loop before
    /// dispatching a worker. `None` disables the check.
    ///
    /// upstream: clientserver.c:746-758 enforces the per-module `max
    /// connections` directive via `claim_connection()`; this cap mirrors
    /// the same behaviour at the daemon level.
    max_connections: Option<usize>,
    config_path: &'a Option<PathBuf>,
    connection_limiter: &'a Option<Arc<ConnectionLimiter>>,
    modules: Arc<Vec<ModuleRuntime>>,
    motd_lines: Arc<Vec<String>>,
    log_sink: &'a Option<SharedLogSink>,
    notifier: &'a systemd::ServiceNotifier,
    client_socket_options: Arc<Vec<SocketOption>>,
    bandwidth_limit: Option<NonZeroU64>,
    reverse_lookup: bool,
    proxy_policy: ProxyProtocolPolicy,
    /// Daemon-wide `timeout`, bounding each connection's pre-module handshake.
    ///
    /// upstream: clientserver.c:1441 arms the handshake deadline from
    /// `daemon_handshake_timeout(-1)`, the GLOBAL value.
    daemon_timeout: Option<NonZeroU64>,
    /// Listener descriptors a forked session child must close before it serves.
    ///
    /// Filled from the accept engine once that engine owns the listeners - it
    /// is the only thing that can name them. Empty until then, which is safe
    /// because no connection can be accepted, and therefore no child forked,
    /// before the loop starts.
    ///
    /// upstream: `socket.c:753-760` `start_accept_loop()`.
    #[cfg(unix)]
    listener_fds: Vec<std::os::fd::RawFd>,
}

/// Checks signal flags and performs maintenance tasks between accept iterations.
///
/// Returns `Some(true)` to break the loop, `None` to continue. Nothing this
/// step does can end the daemon: reaping a worker only reports one finished
/// session, and reload/status failures are logged in place. That mirrors
/// upstream's accept loop, whose body has no error exit at all
/// (socket.c:724-778).
fn check_signals_and_maintain(state: &mut AcceptLoopState<'_>) -> Option<bool> {
    reap_finished_workers(&mut state.workers, state.log_sink.as_ref());

    if state.signal_flags.shutdown.load(Ordering::Relaxed) {
        if let Some(log) = state.log_sink.as_ref() {
            let message = rsync_info!("received shutdown signal, stopping accept loop")
                .with_role(Role::Daemon);
            log_message(log, &message);
        }
        return Some(true);
    }

    // upstream: main.c - SIGUSR1 stops accepting new connections
    // and exits after active transfers drain.
    if state.signal_flags.graceful_exit.load(Ordering::Relaxed) {
        if let Some(log) = state.log_sink.as_ref() {
            let text = format!(
                "received SIGUSR1, draining {} active connection(s) before exit",
                state.workers.len()
            );
            let message = rsync_info!(text).with_role(Role::Daemon);
            log_message(log, &message);
        }
        if let Err(error) = state
            .notifier
            .status("Graceful exit: draining active transfers")
        {
            log_sd_notify_failure(state.log_sink.as_ref(), "graceful exit status", &error);
        }
        return Some(true);
    }

    if state
        .signal_flags
        .reload_config
        .swap(false, Ordering::Relaxed)
    {
        reload_daemon_config(
            state.config_path.as_deref(),
            state.connection_limiter,
            &mut state.modules,
            &mut state.motd_lines,
            state.log_sink.as_ref(),
            state.notifier,
        );
    }

    // upstream: main.c - SIGUSR2 outputs transfer statistics.
    if state
        .signal_flags
        .progress_dump
        .swap(false, Ordering::Relaxed)
    {
        log_progress_summary(
            state.log_sink.as_ref(),
            state.workers.len(),
            state.served,
            state.start_time,
        );
    }

    let current_active = state.workers.len();
    if current_active != state.active_connections {
        let status = format_connection_status(current_active);
        if let Err(error) = state.notifier.status(&status) {
            log_sd_notify_failure(state.log_sink.as_ref(), "connection status update", &error);
        }
        state.active_connections = current_active;
    }

    None
}

/// Refuses an accepted socket once the daemon hits its concurrent
/// connection cap.
///
/// Returns `true` if the socket was refused (the caller should skip
/// spawning a worker and drop the stream), or `false` if admission
/// should proceed. When the cap is hit, writes
/// `@ERROR: max connections (N) reached -- try again later\n` to the
/// stream (matching upstream's wording byte for byte). The accept loop
/// keeps running.
///
/// upstream: clientserver.c:746-758 - `claim_connection()` enforces the
/// per-module `lp_max_connections()` cap and emits
/// `@ERROR: max connections (%d) reached -- try again later\n` to the
/// client via `io_printf(f_out, ...)`.
fn refuse_if_at_capacity(
    stream: &mut DaemonStream,
    peer_addr: SocketAddr,
    state: &AcceptLoopState<'_>,
) -> bool {
    let Some(limit) = state.max_connections else {
        return false;
    };

    let current = state.connection_counter.active();
    if current < limit {
        return false;
    }

    // Mirror upstream wording exactly. The trailing newline is part of the
    // protocol-framed `@ERROR:` reply (`io_printf` writes the literal `\n`).
    let payload = AtError::MaxConnections {
        limit: limit as i64,
    }
    .to_wire();
    if let Err(error) = stream.write_all(&payload)
        && let Some(log) = state.log_sink.as_ref()
    {
        let text = format!("failed to deliver max-connections refusal to {peer_addr}: {error}");
        let message = rsync_warning!(text).with_role(Role::Daemon);
        log_message(log, &message);
    }
    let _ = stream.flush();

    if let Some(log) = state.log_sink.as_ref() {
        log_max_connections_rejection(log, peer_addr, "global", limit, current);
    }

    true
}

/// Emits a structured warning describing a connection rejected by the
/// daemon's `--max-connections` cap.
///
/// Operators rely on this signal to tune the cap from observable evidence,
/// so the fields are stable and named: `which` distinguishes the global
/// cap from a per-module cap, `peer` records the rejected client address,
/// `cap` is the limit that triggered the refusal, and `current` is the
/// active connection count observed at refusal time. The line is emitted
/// at warning level to separate it from routine connect/disconnect info
/// chatter while staying below error severity (the daemon keeps serving).
pub(crate) fn log_max_connections_rejection(
    log: &SharedLogSink,
    peer: SocketAddr,
    which: &str,
    cap: usize,
    current: usize,
) {
    let text = format!(
        "max-connections cap reached: which={which} peer={peer} cap={cap} current={current}"
    );
    let message = rsync_warning!(text).with_role(Role::Daemon);
    log_message(log, &message);
}

/// Spawns a worker for an accepted connection.
///
/// Normalizes the peer address, claims the connection's `max connections`
/// slot, and spawns a session handler thread with `catch_unwind` panic
/// isolation. Returns the [`SessionWorker`] pairing the two.
///
/// The slot is claimed here but held by the returned worker rather than by the
/// session, so the accept loop's reap is what releases it. See
/// [`SessionWorker`] for why that placement is load-bearing.
///
/// Returns `None` when no session could be started at all - only a failed
/// `fork` does that, and upstream treats it the same way, by continuing the
/// accept loop rather than ending the daemon.
///
/// upstream: `socket.c:753-772` `start_accept_loop()` forks per connection;
/// oc does the same on Unix and keeps a thread backing on Windows, which has
/// no `fork`. See [`SessionBacking`].
fn spawn_connection_worker(
    stream: DaemonStream,
    raw_peer_addr: SocketAddr,
    state: &AcceptLoopState<'_>,
) -> Option<SessionWorker> {
    let peer_addr = normalize_peer_address(raw_peer_addr);
    // Build the shareable per-connection context once; the same context type
    // and `serve_session` core drive the async accept path, keeping the wire
    // behaviour byte-identical across both accept engines.
    let context = ConnectionContext::new(
        Arc::clone(&state.modules),
        Arc::clone(&state.motd_lines),
        state.log_sink.as_ref().map(Arc::clone),
        Arc::clone(&state.client_socket_options),
        state.bandwidth_limit,
        state.reverse_lookup,
        state.proxy_policy.clone(),
        state.daemon_timeout,
    );
    let slot = state.connection_counter.acquire();

    #[cfg(not(unix))]
    let backing = {
        let handle = thread::spawn(move || {
            // `serve_session` isolates panics via `catch_unwind` so a faulting
            // connection cannot tear down the daemon - the thread-backed
            // stand-in for the crash isolation a forked child gets for free.
            match context.serve_session(stream, raw_peer_addr) {
                Ok(()) => Ok(()),
                Err(error) => Err((Some(peer_addr), error)),
            }
        });
        SessionBacking {
            handle: Some(handle),
        }
    };

    #[cfg(unix)]
    let backing = fork_session_backing(context, stream, raw_peer_addr, peer_addr, state)?;

    Some(SessionWorker {
        backing,
        _slot: slot,
    })
}

/// Forks a child to serve one connection, mirroring upstream's accept loop.
///
/// The child owns the accepted stream and the session; the parent keeps only
/// the pid. A `chroot()` or working-directory change the session makes is then
/// confined to that child, which is the whole point - both are process-wide,
/// so a thread-backed session leaks them into every later connection.
///
/// upstream: `socket.c:753-765` `start_accept_loop()`.
#[cfg(unix)]
fn fork_session_backing(
    context: ConnectionContext,
    stream: DaemonStream,
    raw_peer_addr: SocketAddr,
    peer_addr: SocketAddr,
    state: &AcceptLoopState<'_>,
) -> Option<SessionBacking> {
    match platform::session_fork::fork_session() {
        Ok(platform::session_fork::ForkSide::Child) => {
            // Before anything else the child sheds the listening sockets it
            // inherited: it serves exactly one already-accepted connection, and
            // a listener held open here keeps the port bound for this child's
            // whole lifetime.
            // upstream: `socket.c:753-760`.
            platform::session_fork::close_inherited_listeners(&state.listener_fds);
            let code = serve_forked_session(&context, stream, raw_peer_addr, peer_addr);
            // `_exit`, never a return: the child shares the parent's buffered
            // stdio and its `Drop`s (the pid-file guard above all), so
            // unwinding would flush and remove state the parent still owns.
            platform::session_fork::exit_child(code);
        }
        Ok(platform::session_fork::ForkSide::Parent { child_pid }) => {
            // The parent must not hold the accepted socket: while it stays
            // open the peer cannot observe the child's close.
            // upstream: `socket.c:772` `close(fd)` in the parent arm.
            drop(stream);
            Some(SessionBacking { child_pid })
        }
        Err(error) => {
            // upstream: `socket.c:766-770` reports the failure, closes the
            // socket and KEEPS ACCEPTING - a fork failure ends one connection,
            // never the daemon.
            report_fork_failure(&error, peer_addr, state.log_sink.as_ref());
            drop(stream);
            None
        }
    }
}

/// Runs one session in the forked child and reduces it to an exit status.
///
/// The child owns the log sink, so it reports its own failure here rather than
/// handing an error back across a process boundary it cannot cross. That is
/// why the parent's [`SessionOutcome::EndedWithStatus`] carries no message: it
/// would only repeat this line.
#[cfg(unix)]
fn serve_forked_session(
    context: &ConnectionContext,
    stream: DaemonStream,
    raw_peer_addr: SocketAddr,
    peer_addr: SocketAddr,
) -> i32 {
    match context.serve_session(stream, raw_peer_addr) {
        Ok(()) => 0,
        Err(error) => {
            report_session_failure(Some(peer_addr), &error, context.log_sink.as_ref());
            SOCKET_IO_EXIT_CODE
        }
    }
}

/// Reports a `fork` that failed, against the peer whose connection it ends.
///
/// upstream: `socket.c:766-770` `rsyserr(FERROR, errno, "could not create
/// child server process")`.
#[cfg(unix)]
fn report_fork_failure(error: &io::Error, peer: SocketAddr, log_sink: Option<&SharedLogSink>) {
    let text = format!("could not create child server process for {peer}: {error}");
    match log_sink {
        Some(log) => {
            let message = rsync_error!(SOCKET_IO_EXIT_CODE, text).with_role(Role::Daemon);
            log_message(log, &message);
        }
        None => eprintln!("{text} [daemon={}]", env!("CARGO_PKG_VERSION")),
    }
}

/// Applies socket options to an accepted stream and logs any failure.
fn apply_client_options(
    stream: &DaemonStream,
    client_socket_options: &[SocketOption],
    log_sink: Option<&SharedLogSink>,
) {
    // upstream: clientserver.c - set_socket_options() is called
    // on the accepted client fd before the session handler runs.
    // Skipped for stdio streams which have no underlying TCP socket.
    // upstream: socket.c:730-733 - each option that fails to apply warns and
    // the loop continues; a single failure never rejects the connection.
    if !client_socket_options.is_empty() {
        let Some(tcp) = stream.tcp_stream() else {
            return;
        };
        apply_socket_options_to_stream(tcp, client_socket_options, log_sink);
    }
}

/// Wraps an accepted `TcpStream` into a [`DaemonStream::Plain`].
fn wrap_accepted_stream(
    tcp_stream: TcpStream,
    _state: &AcceptLoopState<'_>,
) -> Option<DaemonStream> {
    Some(DaemonStream::plain(tcp_stream))
}

/// Updates the systemd connection status after a new connection is accepted.
fn update_connection_status_after_accept(state: &mut AcceptLoopState<'_>) {
    let current_active = state.workers.len();
    if current_active != state.active_connections {
        let status = format_connection_status(current_active);
        if let Err(error) = state.notifier.status(&status) {
            log_sd_notify_failure(state.log_sink.as_ref(), "connection status update", &error);
        }
        state.active_connections = current_active;
    }
}

/// Admits one accepted connection: applies socket options, enforces the
/// concurrent-connection cap, and spawns a session worker.
///
/// Shared by every [`AcceptEngine`] so admission semantics (capacity refusal,
/// worker spawn, session accounting) are identical regardless of how the
/// connection was sourced. Returns `true` when the `--max-sessions` limit has
/// been reached and the accept loop should stop.
fn handle_accepted_connection(
    tcp_stream: TcpStream,
    raw_peer_addr: SocketAddr,
    state: &mut AcceptLoopState<'_>,
) -> bool {
    apply_accepted_stream_tcp_notsent_lowat(&tcp_stream);
    // upstream: clientserver.c:1396 - the daemon unconditionally enables
    // SO_KEEPALIVE on the accepted client socket, independent of the per-module
    // `socket options` config applied below.
    enable_accepted_stream_keepalive(&tcp_stream, state.log_sink.as_ref());

    let Some(mut stream) = wrap_accepted_stream(tcp_stream, state) else {
        return false;
    };

    apply_client_options(
        &stream,
        &state.client_socket_options,
        state.log_sink.as_ref(),
    );

    // Release the slots of sessions that ended while the loop was blocked in
    // `poll`, so the capacity decision below reads freshly-reaped state.
    //
    // The slot guard is parent-owned (see `SessionWorker`), so a finished
    // worker keeps its slot until a reap. Without this call the next reap is
    // the following iteration's, and a session that ended during the poll
    // would refuse a connection the daemon has room for. Under a forked
    // backing this is also where the per-pid `session_fork::try_reap`
    // belongs - one call per worker the parent owns, never a bulk sweep.
    reap_finished_workers(&mut state.workers, state.log_sink.as_ref());

    if refuse_if_at_capacity(&mut stream, raw_peer_addr, state) {
        drop(stream);
        return false;
    }

    let Some(worker) = spawn_connection_worker(stream, raw_peer_addr, state) else {
        // The session never started, so there is no worker to track and
        // nothing was served. The slot guard went with the worker that was
        // never built, releasing capacity for the next connection.
        return false;
    };
    state.workers.push(worker);
    state.served = state.served.saturating_add(1);

    update_connection_status_after_accept(state);

    if let Some(limit) = state.max_sessions
        && state.served >= limit
    {
        if let Err(error) = state.notifier.status("Draining worker threads") {
            log_sd_notify_failure(state.log_sink.as_ref(), "connection status update", &error);
        }
        return true;
    }

    false
}
