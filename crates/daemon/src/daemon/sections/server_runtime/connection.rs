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

    // Check SIGTERM/SIGINT shutdown flag.
    if state.signal_flags.shutdown.load(Ordering::Relaxed) {
        if let Some(log) = state.log_sink.as_ref() {
            let message =
                rsync_info!("received shutdown signal, stopping accept loop")
                    .with_role(Role::Daemon);
            log_message(log, &message);
        }
        return Ok(Some(true));
    }

    // Check SIGUSR1 graceful exit flag.
    // upstream: main.c - SIGUSR1 causes the daemon to stop accepting
    // new connections and exit after active transfers drain.
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

    // Check SIGHUP config reload flag.
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

    // Check SIGUSR2 progress dump flag.
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

/// Spawns a worker thread for an accepted connection.
///
/// Applies socket options, normalizes the peer address, and spawns a session
/// handler thread with `catch_unwind` panic isolation. Returns the join handle.
///
/// upstream: clientserver.c - fork per connection; we use threads with
/// `catch_unwind` for equivalent crash isolation.
fn spawn_connection_worker(
    stream: TcpStream,
    raw_peer_addr: SocketAddr,
    state: &AcceptLoopState<'_>,
) -> thread::JoinHandle<WorkerResult> {
    let peer_addr = normalize_peer_address(raw_peer_addr);
    let modules = Arc::clone(&state.modules);
    let motd_lines = Arc::clone(&state.motd_lines);
    let log_for_worker = state.log_sink.as_ref().map(Arc::clone);
    let conn_guard = state.connection_counter.acquire();
    let bandwidth_limit = state.bandwidth_limit;
    let bandwidth_burst = state.bandwidth_burst;
    let reverse_lookup = state.reverse_lookup;
    let proxy_protocol = state.proxy_protocol;

    thread::spawn(move || {
        let _conn_guard = conn_guard;
        // upstream rsync forks per connection, so a crash only
        // kills that child.  We use threads, so catch_unwind
        // isolates panics to the faulting connection and keeps
        // the daemon alive.
        let result = std::panic::catch_unwind(
            std::panic::AssertUnwindSafe(|| {
                let modules_vec = modules.as_ref();
                let motd_vec = motd_lines.as_ref();
                handle_session(
                    stream,
                    peer_addr,
                    SessionParams {
                        modules: modules_vec.as_slice(),
                        motd_lines: motd_vec.as_slice(),
                        daemon_limit: bandwidth_limit,
                        daemon_burst: bandwidth_burst,
                        log_sink: log_for_worker.clone(),
                        reverse_lookup,
                        proxy_protocol,
                    },
                )
            }),
        );
        match result {
            Ok(Ok(())) => Ok(()),
            Ok(Err(error)) => {
                Err((Some(peer_addr), error))
            }
            Err(payload) => {
                let description =
                    describe_panic_payload(payload);
                if let Some(log) = log_for_worker.as_ref() {
                    let text = format!(
                        "connection handler for {peer_addr} \
                         panicked: {description}"
                    );
                    let message = rsync_error!(
                        SOCKET_IO_EXIT_CODE,
                        text
                    )
                    .with_role(Role::Daemon);
                    log_message(log, &message);
                }
                Ok(())
            }
        }
    })
}

/// Applies socket options to an accepted stream and logs any failure.
fn apply_client_options(
    stream: &TcpStream,
    client_socket_options: &[SocketOption],
    log_sink: Option<&SharedLogSink>,
) {
    // upstream: clientserver.c - set_socket_options() is called
    // on the accepted client fd before the session handler runs.
    if !client_socket_options.is_empty() {
        if let Err(error) =
            apply_socket_options_to_stream(stream, client_socket_options)
        {
            if let Some(log) = log_sink {
                let text = format!(
                    "failed to apply socket options to client connection: {error}"
                );
                let message = rsync_warning!(text).with_role(Role::Daemon);
                log_message(log, &message);
            }
        }
    }
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

/// Runs the accept loop for a single TCP listener.
///
/// Uses non-blocking accept with periodic signal flag checks. This is the
/// simpler path used when only one address family is bound (e.g., IPv4-only
/// or IPv6-only).
fn run_single_listener_loop(
    listener: TcpListener,
    local_addr: SocketAddr,
    state: &mut AcceptLoopState<'_>,
) -> Result<(), DaemonError> {
    listener
        .set_nonblocking(true)
        .map_err(|error| bind_error(local_addr, error))?;

    loop {
        if let Some(true) = check_signals_and_maintain(state)? {
            break;
        }

        match listener.accept() {
            Ok((stream, raw_peer_addr)) => {
                if let Err(error) = stream.set_nonblocking(false) {
                    if let Some(log) = state.log_sink.as_ref() {
                        let text = format!(
                            "failed to set accepted socket to blocking: {error}"
                        );
                        let message = rsync_warning!(text).with_role(Role::Daemon);
                        log_message(log, &message);
                    }
                    continue;
                }

                apply_client_options(&stream, &state.client_socket_options, state.log_sink.as_ref());

                let handle = spawn_connection_worker(stream, raw_peer_addr, state);
                state.workers.push(handle);
                state.served = state.served.saturating_add(1);

                update_connection_status_after_accept(state);
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                // No pending connection - sleep briefly then re-check flags.
                thread::sleep(SIGNAL_CHECK_INTERVAL);
                continue;
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {
                continue;
            }
            Err(error) => {
                return Err(accept_error(local_addr, error));
            }
        }

        if let Some(limit) = state.max_sessions
            && state.served >= limit {
                if let Err(error) = state.notifier.status("Draining worker threads") {
                    log_sd_notify_failure(state.log_sink.as_ref(), "connection status update", &error);
                }
                break;
            }
    }

    Ok(())
}

/// Runs the accept loop for multiple TCP listeners (dual-stack mode).
///
/// Spawns an acceptor thread per listener and multiplexes accepted connections
/// through an MPSC channel. Signal flags are shared with acceptor threads so
/// shutdown propagates promptly.
fn run_dual_stack_loop(
    listeners: Vec<TcpListener>,
    bound_addresses: &[SocketAddr],
    state: &mut AcceptLoopState<'_>,
) -> Result<(), DaemonError> {
    use std::sync::mpsc;

    let (tx, rx) = mpsc::channel::<Result<(TcpStream, SocketAddr), (SocketAddr, io::Error)>>();
    let shutdown = Arc::clone(&state.signal_flags.shutdown);
    let graceful_exit = Arc::clone(&state.signal_flags.graceful_exit);

    let mut acceptor_handles: Vec<thread::JoinHandle<()>> = Vec::with_capacity(listeners.len());

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

    // Drop our copy of the sender so the channel closes when acceptors exit
    drop(tx);

    // Main loop: receive connections from any listener
    loop {
        if let Some(true) = check_signals_and_maintain(state)? {
            break;
        }

        // Use recv_timeout to allow periodic worker reaping and signal checks
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(Ok((stream, raw_peer_addr))) => {
                apply_client_options(&stream, &state.client_socket_options, state.log_sink.as_ref());

                let handle = spawn_connection_worker(stream, raw_peer_addr, state);
                state.workers.push(handle);
                state.served = state.served.saturating_add(1);

                update_connection_status_after_accept(state);

                if let Some(limit) = state.max_sessions
                    && state.served >= limit {
                        if let Err(error) = state.notifier.status("Draining worker threads") {
                            log_sd_notify_failure(state.log_sink.as_ref(), "connection status update", &error);
                        }
                        shutdown.store(true, Ordering::Relaxed);
                        break;
                    }
            }
            Ok(Err((local_addr, error))) => {
                shutdown.store(true, Ordering::Relaxed);
                // Wait for acceptor threads to finish
                for handle in acceptor_handles {
                    let _ = handle.join();
                }
                return Err(accept_error(local_addr, error));
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                continue;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                break;
            }
        }
    }

    // Signal acceptors to stop and wait for them
    shutdown.store(true, Ordering::Relaxed);
    for handle in acceptor_handles {
        let _ = handle.join();
    }

    Ok(())
}
