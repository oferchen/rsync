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
    pre_bound_listener: Option<TcpListener>,
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
    let tcp_fastopen_mode = options.tcp_fastopen();
    let RuntimeOptions {
        bind_address,
        port,
        max_sessions,
        max_connections,
        modules,
        motd_lines,
        bandwidth_limit,
        bandwidth_burst,
        log_file,
        pid_file,
        reverse_lookup,
        lock_file,
        address_family,
        dual_stack,
        bind_address_overridden,
        config_path,
        syslog_facility,
        syslog_tag,
        daemon_uid,
        daemon_gid,
        daemon_chroot,
        proxy_protocol,
        ..
    } = options;

    let log_sink = if let Some(path) = log_file {
        Some(open_log_sink(&path, Brand::Oc)?)
    } else {
        None
    };

    // Apply Linux-only defense-in-depth startup hardenings before the
    // listener binds or any pre-xfer-exec hook is spawned. PR_SET_NO_NEW_PRIVS
    // is a one-way bit and must run before bind/fork so it propagates to
    // every per-connection worker; the LSM-detection log is a one-shot
    // audit line tied to the same startup transition.
    apply_startup_hardening(log_sink.as_ref());

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

    // LSM-CAP.5: verify required Linux capabilities are present before binding
    // the listener. A module configured with `uid = root` cannot honour
    // ownership-changing transfers (`--chown`, `--owner`, `--group`) without
    // CAP_CHOWN; exiting here with an explicit operator-facing message is
    // better than failing per-transfer once the daemon is already serving.
    // On non-Linux targets this is a no-op.
    if let Err(reason) = preflight_required_capabilities(&modules) {
        return Err(DaemonError::new(
            FEATURE_UNAVAILABLE_EXIT_CODE,
            rsync_error!(
                FEATURE_UNAVAILABLE_EXIT_CODE,
                format!("oc-rsyncd: error: {reason}")
            )
            .with_role(Role::Daemon),
        ));
    }

    // Determine bind addresses based on address_family, dual_stack, and the
    // `OC_RSYNC_DAEMON_ADDRESS_FAMILY` runtime override (used by CI/test
    // fixtures to force a specific family without rebuilding the CLI).
    //
    // Default (no flag, no env, no explicit bind address): dual-stack with
    // IPv6 first, IPv4 second. Mirrors upstream's `default_af_hint = 0`
    // (AF_UNSPEC) which lets `getaddrinfo(NULL, port, AI_PASSIVE, ...)`
    // return every available family - on glibc that is `::` then `0.0.0.0`.
    // `bind_listeners_per_family` walks the list in order, logs a warning
    // for any per-family bind failure, and only fails the daemon when zero
    // sockets bound. GitHub Actions Linux runners that have IPv6 partially
    // configured (where `bind(2)` to `[::]:port` returns `EADDRNOTAVAIL`)
    // cleanly fall back to the IPv4 listener instead of exiting 10 with a
    // silent dual-stack misconfiguration.
    //
    // upstream: socket.c:402-499 (`open_socket_in`) walks every
    // getaddrinfo result, binds one socket per family, and only returns
    // NULL when zero sockets bound.
    let env_family = read_address_family_env_override();
    let bind_addresses: Vec<IpAddr> = if bind_address_overridden {
        vec![bind_address]
    } else if let Some(env) = env_family {
        match env {
            AddressFamilyOverride::Ipv4 => vec![IpAddr::V4(Ipv4Addr::UNSPECIFIED)],
            AddressFamilyOverride::Ipv6 => vec![IpAddr::V6(Ipv6Addr::UNSPECIFIED)],
            AddressFamilyOverride::Both => vec![
                IpAddr::V6(Ipv6Addr::UNSPECIFIED),
                IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            ],
        }
    } else if dual_stack {
        vec![
            IpAddr::V6(Ipv6Addr::UNSPECIFIED),
            IpAddr::V4(Ipv4Addr::UNSPECIFIED),
        ]
    } else {
        match address_family {
            Some(AddressFamily::Ipv4) => vec![IpAddr::V4(Ipv4Addr::UNSPECIFIED)],
            Some(AddressFamily::Ipv6) => vec![IpAddr::V6(Ipv6Addr::UNSPECIFIED)],
            None => vec![
                IpAddr::V6(Ipv6Addr::UNSPECIFIED),
                IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            ],
        }
    };

    // When a pre-bound listener is injected (test infrastructure), use it
    // directly - skipping the bind step eliminates the TOCTOU race between
    // port allocation and daemon bind. `listeners` is later drained via
    // `listeners.remove(0)`; `bound_addresses` is only read by index.
    let mut listeners: Vec<TcpListener>;
    let bound_addresses: Vec<SocketAddr>;

    if let Some(listener) = pre_bound_listener {
        let local_addr = listener
            .local_addr()
            .unwrap_or_else(|_| SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port));
        bound_addresses = vec![local_addr];
        listeners = vec![listener];
    } else {
        let backlog = listen_backlog.map_or(DEFAULT_LISTEN_BACKLOG, |v| v as i32);

        // Per-family bind failure handling mirrors upstream rsync's
        // `socket.c::open_socket_in` (rsync-3.4.1, lines 428-498): the loop
        // attempts every getaddrinfo result, emits a per-family diagnostic
        // via warn_per_family_bind_failure, and only fails the daemon when
        // zero sockets bound. A dual-stack startup on a kernel where one
        // family is unavailable (e.g., GitHub Actions runners with IPv6
        // partially configured but unroutable) succeeds as long as the
        // other family binds.
        match bind_listeners_per_family(
            &bind_addresses,
            port,
            backlog,
            tcp_fastopen_mode,
            log_sink.as_ref(),
        ) {
            Ok((bound_listeners, bound_local_addrs)) => {
                listeners = bound_listeners;
                bound_addresses = bound_local_addrs;
            }
            Err(error) => {
                let requested_addr = SocketAddr::new(bind_addresses[0], port);
                return Err(bind_error(requested_addr, error));
            }
        }
    }

    // LSM-CAP.2: CAP_NET_BIND_SERVICE is no longer needed once the listener
    // has bound. Drop it from effective, permitted, and bounding sets so a
    // compromised worker cannot rebind another privileged port. No-op on
    // non-Linux targets and on builds that never held the capability.
    drop_cap_net_bind_service(log_sink.as_ref());

    // Surface a one-shot warning when the operator asked for TFO
    // unconditionally (`--tcp-fastopen=on`) but the running platform does
    // not implement server-side TFO. `auto` mode stays silent because
    // unsupported platforms are part of the expected fallback path.
    if tcp_fastopen_mode.is_strict() && !fast_io::tcp_fastopen_listener_supported() {
        warn_tcp_fastopen_unsupported(log_sink.as_ref());
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

    // Apply daemon-level chroot and drop daemon-level privileges after binding
    // (which may require root for ports < 1024), daemonizing, and writing the
    // PID file. Order matches upstream: chroot first (while still root), then
    // setgid, then setuid. Any failure is fatal so the daemon never continues
    // running as root after a partial privilege drop.
    // upstream: clientserver.c:1301-1339 start_accept_loop() applies
    // lp_daemon_chroot() then lp_daemon_gid()/lp_daemon_uid() before the accept
    // loop services any client.
    if daemon_chroot.is_some() || daemon_uid.is_some() || daemon_gid.is_some() {
        let fallback_sink = open_privilege_fallback_sink();
        let sink = log_sink.as_ref().unwrap_or(&fallback_sink);

        if let Some(chroot_path) = daemon_chroot.as_deref() {
            apply_chroot(chroot_path, sink).map_err(|error| {
                DaemonError::new(
                    FEATURE_UNAVAILABLE_EXIT_CODE,
                    rsync_error!(
                        FEATURE_UNAVAILABLE_EXIT_CODE,
                        format!(
                            "daemon chroot to '{}' failed: {error}",
                            chroot_path.display()
                        )
                    )
                    .with_role(Role::Daemon),
                )
            })?;
        }

        if daemon_uid.is_some() || daemon_gid.is_some() {
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
        max_connections: max_connections.map(NonZeroUsize::get),
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
        #[cfg(feature = "daemon-tls")]
        tls_acceptor: None,
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
