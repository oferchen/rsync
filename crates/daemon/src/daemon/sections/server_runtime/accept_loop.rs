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
fn serve_connections(
    options: RuntimeOptions,
    external_signal_flags: Option<platform::signal::SignalFlags>,
) -> Result<(), DaemonError> {
    // Use externally injected signal flags (from the Windows Service dispatcher)
    // when available, otherwise register platform signal handlers so SIGPIPE is
    // ignored and SIGHUP/SIGTERM/SIGINT flags are captured from the start.
    // upstream: main.c SIGACT(SIGPIPE, SIG_IGN) and rsync_panic_handler setup.
    let signal_flags = match external_signal_flags {
        Some(flags) => SignalFlags::from(flags),
        None => register_signal_handlers().map_err(|error| {
            DaemonError::new(
                FEATURE_UNAVAILABLE_EXIT_CODE,
                rsync_error!(
                    FEATURE_UNAVAILABLE_EXIT_CODE,
                    format!("failed to register signal handlers: {error}")
                )
                .with_role(Role::Daemon),
            )
        })?,
    };

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

    let modules: Arc<Vec<ModuleRuntime>> = Arc::new(
        modules
            .into_iter()
            .map(|definition| ModuleRuntime::new(definition, connection_limiter.clone()))
            .collect(),
    );
    let motd_lines = Arc::new(motd_lines);

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

    let mut state = AcceptLoopState {
        signal_flags: &signal_flags,
        workers: Vec::new(),
        served: 0,
        active_connections: 0,
        connection_counter: ConnectionCounter::new(),
        start_time: SystemTime::now(),
        max_sessions: max_sessions.map(NonZeroUsize::get),
        config_path: &config_path,
        connection_limiter: &connection_limiter,
        modules,
        motd_lines,
        log_sink: &log_sink,
        notifier: &notifier,
        client_socket_options,
        bandwidth_limit,
        bandwidth_burst,
        reverse_lookup,
        proxy_protocol,
    };

    if listeners.len() == 1 {
        let listener = listeners.remove(0);
        let local_addr = bound_addresses[0];
        run_single_listener_loop(listener, local_addr, &mut state)?;
    } else {
        run_dual_stack_loop(listeners, &bound_addresses, &mut state)?;
    }

    let result = drain_workers(&mut state.workers);

    let shutdown_status = match state.served {
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
