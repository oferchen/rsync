/// Shared state for the connection accept loops.
///
/// Groups the mutable and immutable context needed by both the single-listener
/// and dual-stack accept loops, avoiding excessive parameter lists.
struct AcceptLoopState<'a> {
    signal_flags: &'a SignalFlags,
    workers: Vec<thread::JoinHandle<WorkerResult>>,
    served: usize,
    active_connections: usize,
    connection_counter: ConnectionCounter,
    start_time: SystemTime,
    max_sessions: Option<usize>,
    /// Concurrent connection cap consulted by the accept loop before
    /// dispatching a worker. `None` disables the check.
    ///
    /// upstream: clientserver.c:744-756 enforces the per-module `max
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
    bandwidth_burst: Option<NonZeroU64>,
    reverse_lookup: bool,
    proxy_protocol: bool,
}

/// Checks signal flags and performs maintenance tasks between accept iterations.
///
/// Returns `Some(true)` to break the loop, `None` to continue. Propagates
/// errors from worker reaping.
fn check_signals_and_maintain(
    state: &mut AcceptLoopState<'_>,
) -> Result<Option<bool>, DaemonError> {
    reap_finished_workers(&mut state.workers)?;

    if state.signal_flags.shutdown.load(Ordering::Relaxed) {
        if let Some(log) = state.log_sink.as_ref() {
            let message =
                rsync_info!("received shutdown signal, stopping accept loop")
                    .with_role(Role::Daemon);
            log_message(log, &message);
        }
        return Ok(Some(true));
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
        if let Err(error) = state.notifier.status("Graceful exit: draining active transfers") {
            log_sd_notify_failure(
                state.log_sink.as_ref(),
                "graceful exit status",
                &error,
            );
        }
        return Ok(Some(true));
    }

    if state.signal_flags.reload_config.swap(false, Ordering::Relaxed) {
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
    if state.signal_flags.progress_dump.swap(false, Ordering::Relaxed) {
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

    Ok(None)
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
/// upstream: clientserver.c:744-756 - `claim_connection()` enforces the
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
    let payload = format!("{}\n", MODULE_MAX_CONNECTIONS_PAYLOAD.replace("{limit}", &limit.to_string()));
    if let Err(error) = stream.write_all(payload.as_bytes())
        && let Some(log) = state.log_sink.as_ref()
    {
        let text =
            format!("failed to deliver max-connections refusal to {peer_addr}: {error}");
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

/// Spawns a worker thread for an accepted connection.
///
/// Applies socket options, normalizes the peer address, and spawns a session
/// handler thread with `catch_unwind` panic isolation. Returns the join handle.
///
/// upstream: clientserver.c - fork per connection; we use threads with
/// `catch_unwind` for equivalent crash isolation.
fn spawn_connection_worker(
    stream: DaemonStream,
    raw_peer_addr: SocketAddr,
    state: &AcceptLoopState<'_>,
) -> thread::JoinHandle<WorkerResult> {
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
        state.bandwidth_burst,
        state.reverse_lookup,
        state.proxy_protocol,
    );
    let conn_guard = state.connection_counter.acquire();

    thread::spawn(move || {
        let _conn_guard = conn_guard;
        // upstream rsync forks per connection, so a crash only kills that
        // child. `serve_session` isolates panics via `catch_unwind` so a
        // faulting connection cannot tear down the daemon.
        match context.serve_session(stream, raw_peer_addr) {
            Ok(()) => Ok(()),
            Err(error) => Err((Some(peer_addr), error)),
        }
    })
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
            log_sd_notify_failure(
                state.log_sink.as_ref(),
                "connection status update",
                &error,
            );
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

    let Some(mut stream) = wrap_accepted_stream(tcp_stream, state) else {
        return false;
    };

    apply_client_options(&stream, &state.client_socket_options, state.log_sink.as_ref());

    if refuse_if_at_capacity(&mut stream, raw_peer_addr, state) {
        drop(stream);
        return false;
    }

    let handle = spawn_connection_worker(stream, raw_peer_addr, state);
    state.workers.push(handle);
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
