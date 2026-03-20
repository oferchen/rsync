/// Re-reads and re-parses the daemon configuration file on SIGHUP.
///
/// On success, replaces `modules` and `motd_lines` with freshly parsed values
/// so that subsequent connections use the new configuration. Existing
/// connections retain the old config via their `Arc` clones.
///
/// On failure (missing file, parse error), the error is logged and the daemon
/// continues with the previous configuration - matching upstream rsync
/// behaviour where a bad config reload is non-fatal.
///
/// upstream: clientserver.c - `re_read_config()` called from SIGHUP handler.
fn reload_daemon_config(
    config_path: Option<&Path>,
    connection_limiter: &Option<Arc<ConnectionLimiter>>,
    modules: &mut Arc<Vec<ModuleRuntime>>,
    motd_lines: &mut Arc<Vec<String>>,
    log_sink: Option<&SharedLogSink>,
    notifier: &systemd::ServiceNotifier,
) {
    if let Some(log) = log_sink {
        let message = rsync_info!("received SIGHUP, reloading configuration")
            .with_role(Role::Daemon);
        log_message(log, &message);
    }
    if let Err(error) = notifier.status("Reloading configuration") {
        log_sd_notify_failure(log_sink, "config reload status", &error);
    }

    let path = match config_path {
        Some(path) => path,
        None => {
            if let Some(log) = log_sink {
                let message = rsync_info!(
                    "SIGHUP ignored: no config file was loaded at startup"
                )
                .with_role(Role::Daemon);
                log_message(log, &message);
            }
            return;
        }
    };

    let parsed = match parse_config_modules(path) {
        Ok(parsed) => parsed,
        Err(error) => {
            if let Some(log) = log_sink {
                let text = format!(
                    "config reload failed, keeping old configuration: {error}"
                );
                let message = rsync_warning!(text).with_role(Role::Daemon);
                log_message(log, &message);
            }
            return;
        }
    };

    let new_modules: Vec<ModuleRuntime> = parsed
        .modules
        .into_iter()
        .map(|definition| ModuleRuntime::new(definition, connection_limiter.clone()))
        .collect();
    let module_count = new_modules.len();
    *modules = Arc::new(new_modules);
    *motd_lines = Arc::new(parsed.motd_lines);

    if let Some(log) = log_sink {
        let text = format!(
            "configuration reloaded successfully ({module_count} module{})",
            if module_count == 1 { "" } else { "s" }
        );
        let message = rsync_info!(text).with_role(Role::Daemon);
        log_message(log, &message);
    }

    let status = format!("Configuration reloaded ({module_count} modules)");
    if let Err(error) = notifier.status(&status) {
        log_sd_notify_failure(log_sink, "config reload status", &error);
    }
}

/// Accepts TCP connections and spawns a thread per session.
///
/// Unlike upstream rsync which forks a child process per connection
/// (giving each session its own address space), this function uses
/// `std::thread::spawn` with `catch_unwind` to isolate panics.  A panic
/// in one session is caught and logged without tearing down the daemon,
/// matching upstream's crash-isolation semantics.
///
/// See `docs/DAEMON_PROCESS_MODEL.md` for details on the thread-vs-fork
/// trade-offs.
fn serve_connections(options: RuntimeOptions) -> Result<(), DaemonError> {
    // Register signal handlers before entering the accept loop so SIGPIPE is
    // ignored and SIGHUP/SIGTERM/SIGINT flags are captured from the start.
    // upstream: main.c SIGACT(SIGPIPE, SIG_IGN) and rsync_panic_handler setup.
    let signal_flags = register_signal_handlers().map_err(|error| {
        DaemonError::new(
            FEATURE_UNAVAILABLE_EXIT_CODE,
            rsync_error!(
                FEATURE_UNAVAILABLE_EXIT_CODE,
                format!("failed to register signal handlers: {error}")
            )
            .with_role(Role::Daemon),
        )
    })?;

    let manifest = manifest();
    let version = manifest.rust_version();
    let detach = options.detach();
    let listen_backlog = options.listen_backlog();
    let socket_options_str = options.socket_options().map(str::to_string);
    let RuntimeOptions {
        bind_address,
        port,
        max_sessions,
        modules,
        motd_lines,
        bandwidth_limit,
        bandwidth_burst,
        log_file,
        pid_file,
        reverse_lookup,
        lock_file,
        address_family,
        bind_address_overridden,
        config_path,
        syslog_facility,
        syslog_tag,
        daemon_uid,
        daemon_gid,
        proxy_protocol,
        ..
    } = options;

    let log_sink = if let Some(path) = log_file {
        Some(open_log_sink(&path, Brand::Oc)?)
    } else {
        None
    };

    // Open syslog connection when no log file is configured (matching upstream
    // rsync's behaviour: log.c routes to syslog when logfile_name is NULL).
    // The guard is held for the daemon's lifetime; dropping it calls closelog(3).
    #[cfg(unix)]
    let _syslog_guard = if log_sink.is_none() {
        let facility = syslog_facility
            .as_deref()
            .and_then(logging_sink::syslog::SyslogFacility::from_name)
            .unwrap_or_default();
        let tag = syslog_tag
            .as_deref()
            .unwrap_or(logging_sink::syslog::DEFAULT_SYSLOG_TAG);
        let config = logging_sink::syslog::SyslogConfig::new(facility, tag);
        Some(config.open())
    } else {
        None
    };

    // Suppress unused-variable warnings on non-Unix.
    #[cfg(not(unix))]
    let _ = (&syslog_facility, &syslog_tag);

    let connection_limiter = if let Some(path) = lock_file {
        Some(Arc::new(ConnectionLimiter::open(path)?))
    } else {
        None
    };

    let mut modules: Arc<Vec<ModuleRuntime>> = Arc::new(
        modules
            .into_iter()
            .map(|definition| ModuleRuntime::new(definition, connection_limiter.clone()))
            .collect(),
    );
    let mut motd_lines = Arc::new(motd_lines);

    // Determine bind addresses based on address_family and bind_address_overridden.
    // When no specific family or address is configured, bind to both IPv4 and IPv6
    // (dual-stack), matching upstream rsync behavior.
    let bind_addresses: Vec<IpAddr> = if bind_address_overridden {
        vec![bind_address]
    } else {
        match address_family {
            Some(AddressFamily::Ipv4) => vec![IpAddr::V4(Ipv4Addr::UNSPECIFIED)],
            Some(AddressFamily::Ipv6) => vec![IpAddr::V6(Ipv6Addr::UNSPECIFIED)],
            None => {
                vec![
                    IpAddr::V6(Ipv6Addr::UNSPECIFIED),
                    IpAddr::V4(Ipv4Addr::UNSPECIFIED),
                ]
            }
        }
    };

    // upstream: daemon-parm.txt - listen_backlog INTEGER, default 5.
    // Using socket2 to create the listener allows explicit control over the
    // backlog argument passed to listen(2).
    const DEFAULT_LISTEN_BACKLOG: i32 = 5;
    let backlog = listen_backlog.map_or(DEFAULT_LISTEN_BACKLOG, |v| v as i32);

    let mut listeners: Vec<TcpListener> = Vec::with_capacity(bind_addresses.len());
    let mut bound_addresses: Vec<SocketAddr> = Vec::with_capacity(bind_addresses.len());

    for addr in &bind_addresses {
        let requested_addr = SocketAddr::new(*addr, port);
        match bind_with_backlog(requested_addr, backlog) {
            Ok(listener) => {
                let local_addr = listener.local_addr().unwrap_or(requested_addr);
                bound_addresses.push(local_addr);
                listeners.push(listener);
            }
            Err(error) => {
                // If binding to one family fails (e.g., IPv6 not available), continue
                // with the other family if we're in dual-stack mode. Otherwise, fail.
                if bind_addresses.len() > 1 && !listeners.is_empty() {
                    continue;
                }
                return Err(bind_error(requested_addr, error));
            }
        }
    }

    if listeners.is_empty() {
        let requested_addr = SocketAddr::new(bind_addresses[0], port);
        return Err(bind_error(
            requested_addr,
            io::Error::new(io::ErrorKind::AddrNotAvailable, "no addresses available to bind"),
        ));
    }

    // upstream: socket.c:set_socket_options() - apply socket options to each
    // listener socket before accepting connections, and to each accepted
    // client connection before the session handler runs.
    let client_socket_options: Arc<Vec<SocketOption>> = if let Some(ref opts_str) =
        socket_options_str
    {
        let parsed = parse_socket_options(opts_str).map_err(|msg| {
            DaemonError::new(
                FEATURE_UNAVAILABLE_EXIT_CODE,
                rsync_error!(
                    FEATURE_UNAVAILABLE_EXIT_CODE,
                    format!("invalid socket options: {msg}")
                )
                .with_role(Role::Daemon),
            )
        })?;
        for listener in &listeners {
            apply_socket_options_to_listener(listener, &parsed).map_err(|error| {
                DaemonError::new(
                    FEATURE_UNAVAILABLE_EXIT_CODE,
                    rsync_error!(
                        FEATURE_UNAVAILABLE_EXIT_CODE,
                        format!("failed to set socket options: {error}")
                    )
                    .with_role(Role::Daemon),
                )
            })?;
        }
        Arc::new(parsed)
    } else {
        Arc::new(Vec::new())
    };

    // Detach from terminal if --detach is active (Unix default).
    // Must happen after binding so startup errors reach stderr, and before
    // PID file creation so the file records the child's PID.
    // upstream: clientserver.c:1518-1521 -- become_daemon() called before accept loop.
    #[cfg(unix)]
    if detach {
        become_daemon()?;
    }

    // Suppress unused-variable warning on platforms where fork is unavailable.
    #[cfg(not(unix))]
    let _ = detach;

    // Write the PID file after binding so the file only appears once the port
    // is ready to accept connections - matching upstream main.c write_pid_file().
    let pid_guard = if let Some(path) = pid_file {
        Some(PidFileGuard::create(path)?)
    } else {
        None
    };

    // Drop daemon-level privileges after binding (which may require root for
    // ports < 1024), daemonizing, and writing the PID file.
    // upstream: clientserver.c - setuid/setgid from global uid/gid params happen
    // after the socket is bound and the daemon has forked.
    if daemon_uid.is_some() || daemon_gid.is_some() {
        let fallback_sink = open_privilege_fallback_sink();
        let sink = log_sink.as_ref().unwrap_or(&fallback_sink);
        drop_privileges(daemon_uid, daemon_gid, sink).map_err(|error| {
            DaemonError::new(
                FEATURE_UNAVAILABLE_EXIT_CODE,
                rsync_error!(
                    FEATURE_UNAVAILABLE_EXIT_CODE,
                    format!("failed to drop daemon privileges: {error}")
                )
                .with_role(Role::Daemon),
            )
        })?;
    }

    let notifier = systemd::ServiceNotifier::new();
    let ready_status = if bound_addresses.len() == 1 {
        format!("Listening on {}", bound_addresses[0])
    } else {
        let addrs: Vec<String> = bound_addresses.iter().map(ToString::to_string).collect();
        format!("Listening on {}", addrs.join(" and "))
    };
    if let Err(error) = notifier.ready(Some(&ready_status)) {
        log_sd_notify_failure(log_sink.as_ref(), "service readiness", &error);
    }

    if let Some(log) = log_sink.as_ref() {
        let text = format!(
            "rsyncd version {version} starting, listening on port {port}"
        );
        let message = rsync_info!(text).with_role(Role::Daemon);
        log_message(log, &message);
    }

    let mut served = 0usize;
    let mut workers: Vec<thread::JoinHandle<WorkerResult>> = Vec::new();
    let max_sessions = max_sessions.map(NonZeroUsize::get);
    let mut active_connections = 0usize;
    let connection_counter = ConnectionCounter::new();
    let start_time = SystemTime::now();

    // For dual-stack (multiple listeners), use channels to receive accepted connections
    // from listener threads. For single listener, use direct accept for simplicity.
    use std::sync::mpsc;

    if listeners.len() == 1 {
        // Single listener with non-blocking accept so signal flags are
        // checked periodically instead of blocking indefinitely.
        let listener = listeners.remove(0);
        let local_addr = bound_addresses[0];
        listener
            .set_nonblocking(true)
            .map_err(|error| bind_error(local_addr, error))?;

        loop {
            reap_finished_workers(&mut workers)?;

            // Check SIGTERM/SIGINT shutdown flag.
            if signal_flags.shutdown.load(Ordering::Relaxed) {
                if let Some(log) = log_sink.as_ref() {
                    let message =
                        rsync_info!("received shutdown signal, stopping accept loop")
                            .with_role(Role::Daemon);
                    log_message(log, &message);
                }
                break;
            }

            // Check SIGUSR1 graceful exit flag.
            // upstream: main.c - SIGUSR1 causes the daemon to stop accepting
            // new connections and exit after active transfers drain.
            if signal_flags.graceful_exit.load(Ordering::Relaxed) {
                if let Some(log) = log_sink.as_ref() {
                    let text = format!(
                        "received SIGUSR1, draining {} active connection(s) before exit",
                        workers.len()
                    );
                    let message = rsync_info!(text).with_role(Role::Daemon);
                    log_message(log, &message);
                }
                if let Err(error) = notifier.status("Graceful exit: draining active transfers") {
                    log_sd_notify_failure(
                        log_sink.as_ref(),
                        "graceful exit status",
                        &error,
                    );
                }
                break;
            }

            // Check SIGHUP config reload flag.
            if signal_flags.reload_config.swap(false, Ordering::Relaxed) {
                reload_daemon_config(
                    config_path.as_deref(),
                    &connection_limiter,
                    &mut modules,
                    &mut motd_lines,
                    log_sink.as_ref(),
                    &notifier,
                );
            }

            // Check SIGUSR2 progress dump flag.
            // upstream: main.c - SIGUSR2 outputs transfer statistics.
            if signal_flags.progress_dump.swap(false, Ordering::Relaxed) {
                log_progress_summary(
                    log_sink.as_ref(),
                    workers.len(),
                    served,
                    start_time,
                );
            }

            let current_active = workers.len();
            if current_active != active_connections {
                let status = format_connection_status(current_active);
                if let Err(error) = notifier.status(&status) {
                    log_sd_notify_failure(log_sink.as_ref(), "connection status update", &error);
                }
                active_connections = current_active;
            }

            match listener.accept() {
                Ok((stream, raw_peer_addr)) => {
                    if let Err(error) = stream.set_nonblocking(false) {
                        if let Some(log) = log_sink.as_ref() {
                            let text = format!(
                                "failed to set accepted socket to blocking: {error}"
                            );
                            let message = rsync_warning!(text).with_role(Role::Daemon);
                            log_message(log, &message);
                        }
                        continue;
                    }

                    // upstream: clientserver.c - set_socket_options() is called
                    // on the accepted client fd before the session handler runs.
                    if !client_socket_options.is_empty() {
                        if let Err(error) =
                            apply_socket_options_to_stream(&stream, &client_socket_options)
                        {
                            if let Some(log) = log_sink.as_ref() {
                                let text = format!(
                                    "failed to apply socket options to client connection: {error}"
                                );
                                let message = rsync_warning!(text).with_role(Role::Daemon);
                                log_message(log, &message);
                            }
                        }
                    }

                    let peer_addr = normalize_peer_address(raw_peer_addr);
                    let modules = Arc::clone(&modules);
                    let motd_lines = Arc::clone(&motd_lines);
                    let log_for_worker = log_sink.as_ref().map(Arc::clone);
                    let conn_guard = connection_counter.acquire();
                    let handle = thread::spawn(move || {
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
                    });
                    workers.push(handle);
                    served = served.saturating_add(1);

                    let current_active = workers.len();
                    if current_active != active_connections {
                        let status = format_connection_status(current_active);
                        if let Err(error) = notifier.status(&status) {
                            log_sd_notify_failure(
                                log_sink.as_ref(),
                                "connection status update",
                                &error,
                            );
                        }
                        active_connections = current_active;
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    // No pending connection -- sleep briefly then re-check flags.
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

            if let Some(limit) = max_sessions
                && served >= limit {
                    if let Err(error) = notifier.status("Draining worker threads") {
                        log_sd_notify_failure(log_sink.as_ref(), "connection status update", &error);
                    }
                    break;
                }
        }
    } else {
        // Multiple listeners (dual-stack) - spawn acceptor threads and use channel.
        // Share signal-based flags with acceptor threads so SIGTERM/SIGINT/SIGUSR1
        // stop all listeners promptly.
        let (tx, rx) = mpsc::channel::<Result<(TcpStream, SocketAddr), (SocketAddr, io::Error)>>();
        let shutdown = Arc::clone(&signal_flags.shutdown);
        let graceful_exit = Arc::clone(&signal_flags.graceful_exit);

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
            reap_finished_workers(&mut workers)?;

            // Check SIGTERM/SIGINT shutdown flag.
            if signal_flags.shutdown.load(Ordering::Relaxed) {
                if let Some(log) = log_sink.as_ref() {
                    let message =
                        rsync_info!("received shutdown signal, stopping accept loop")
                            .with_role(Role::Daemon);
                    log_message(log, &message);
                }
                break;
            }

            // Check SIGUSR1 graceful exit flag.
            if signal_flags.graceful_exit.load(Ordering::Relaxed) {
                if let Some(log) = log_sink.as_ref() {
                    let text = format!(
                        "received SIGUSR1, draining {} active connection(s) before exit",
                        workers.len()
                    );
                    let message = rsync_info!(text).with_role(Role::Daemon);
                    log_message(log, &message);
                }
                if let Err(error) = notifier.status("Graceful exit: draining active transfers") {
                    log_sd_notify_failure(
                        log_sink.as_ref(),
                        "graceful exit status",
                        &error,
                    );
                }
                break;
            }

            // Check SIGHUP config reload flag.
            if signal_flags.reload_config.swap(false, Ordering::Relaxed) {
                reload_daemon_config(
                    config_path.as_deref(),
                    &connection_limiter,
                    &mut modules,
                    &mut motd_lines,
                    log_sink.as_ref(),
                    &notifier,
                );
            }

            // Check SIGUSR2 progress dump flag.
            if signal_flags.progress_dump.swap(false, Ordering::Relaxed) {
                log_progress_summary(
                    log_sink.as_ref(),
                    workers.len(),
                    served,
                    start_time,
                );
            }

            let current_active = workers.len();
            if current_active != active_connections {
                let status = format_connection_status(current_active);
                if let Err(error) = notifier.status(&status) {
                    log_sd_notify_failure(log_sink.as_ref(), "connection status update", &error);
                }
                active_connections = current_active;
            }

            // Use recv_timeout to allow periodic worker reaping and signal checks
            match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(Ok((stream, raw_peer_addr))) => {
                    // upstream: clientserver.c - set_socket_options() is called
                    // on the accepted client fd before the session handler runs.
                    if !client_socket_options.is_empty() {
                        if let Err(error) =
                            apply_socket_options_to_stream(&stream, &client_socket_options)
                        {
                            if let Some(log) = log_sink.as_ref() {
                                let text = format!(
                                    "failed to apply socket options to client connection: {error}"
                                );
                                let message = rsync_warning!(text).with_role(Role::Daemon);
                                log_message(log, &message);
                            }
                        }
                    }

                    let peer_addr = normalize_peer_address(raw_peer_addr);
                    let modules = Arc::clone(&modules);
                    let motd_lines = Arc::clone(&motd_lines);
                    let log_for_worker = log_sink.as_ref().map(Arc::clone);
                    let conn_guard = connection_counter.acquire();
                    let handle = thread::spawn(move || {
                        let _conn_guard = conn_guard;
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
                    });
                    workers.push(handle);
                    served = served.saturating_add(1);

                    let current_active = workers.len();
                    if current_active != active_connections {
                        let status = format_connection_status(current_active);
                        if let Err(error) = notifier.status(&status) {
                            log_sd_notify_failure(
                                log_sink.as_ref(),
                                "connection status update",
                                &error,
                            );
                        }
                        active_connections = current_active;
                    }

                    if let Some(limit) = max_sessions
                        && served >= limit {
                            if let Err(error) = notifier.status("Draining worker threads") {
                                log_sd_notify_failure(log_sink.as_ref(), "connection status update", &error);
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
    }

    let result = drain_workers(&mut workers);

    let shutdown_status = match served {
        0 => String::from("No connections handled; shutting down"),
        1 => String::from("Served 1 connection; shutting down"),
        count => format!("Served {count} connections; shutting down"),
    };
    if let Err(error) = notifier.status(&shutdown_status) {
        log_sd_notify_failure(log_sink.as_ref(), "shutdown status", &error);
    }
    if let Err(error) = notifier.stopping() {
        log_sd_notify_failure(log_sink.as_ref(), "service shutdown", &error);
    }

    if let Some(log) = log_sink.as_ref() {
        let text = format!("rsyncd version {version} shutting down");
        let message = rsync_info!(text).with_role(Role::Daemon);
        log_message(log, &message);
    }

    drop(pid_guard);

    result
}
