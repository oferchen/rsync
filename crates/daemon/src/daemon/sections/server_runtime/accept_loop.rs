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
    let acceptor_threads = options.acceptor_threads();
    let socket_options_str = options.socket_options().map(str::to_string);
    let tcp_fastopen_mode = options.tcp_fastopen();

    // Capture the QUIC listener inputs before `options` is destructured. The
    // UDP/QUIC listener binds the same resolved address set as TCP but only
    // when QUIC is configured; unconfigured (the default, even under
    // `--all-features`) leaves this `None` and no UDP socket is opened.
    #[cfg(all(unix, feature = "quic"))]
    let quic_bind = options
        .quic_listener_enabled()
        .then(|| (options.effective_quic_port(), options.resolve_quic_identity()));

    let RuntimeOptions {
        bind_address,
        port,
        max_sessions,
        max_connections,
        modules,
        motd_lines,
        bandwidth_limit,
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

    let (modules, connection_limiter) = build_module_runtimes_with_lock_file(modules, lock_file)?;
    let modules: Arc<Vec<ModuleRuntime>> = Arc::new(modules);
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

    // Determine bind addresses from address_family, dual_stack, the explicit
    // `address =` override, and the `OC_RSYNC_DAEMON_ADDRESS_FAMILY` runtime
    // override. `resolve_bind_addresses` is the single source of truth for
    // this IP-assignment policy (see its rustdoc for the precedence and the
    // upstream `open_socket_in` citation), shared so any future datagram
    // listener binds the identical ordered set.
    //
    // `bind_listeners_per_family` then walks the returned list in order, logs
    // a warning for any per-family bind failure, and only fails the daemon
    // when zero sockets bound. GitHub Actions Linux runners that have IPv6
    // partially configured (where `bind(2)` to `[::]:port` returns
    // `EADDRNOTAVAIL`) cleanly fall back to the IPv4 listener instead of
    // exiting 10 with a silent dual-stack misconfiguration.
    let bind_addresses = resolve_bind_addresses(
        bind_address,
        bind_address_overridden,
        address_family,
        dual_stack,
        read_address_family_env_override(),
    );

    // upstream: socket.c:set_socket_options() - the `socket options =` /
    // `--sockopts` string is parsed once up front so it can be applied to
    // each listener socket before bind(2) (socket.c:449-452 - after
    // SO_REUSEADDR, before bind), and later to each accepted client
    // connection before the session handler runs.
    let parsed_socket_options: Vec<SocketOption> = if let Some(ref opts_str) = socket_options_str {
        parse_socket_options(opts_str, log_sink.as_ref()).map_err(|msg| {
            DaemonError::new(
                FEATURE_UNAVAILABLE_EXIT_CODE,
                rsync_error!(
                    FEATURE_UNAVAILABLE_EXIT_CODE,
                    format!("invalid socket options: {msg}")
                )
                .with_role(Role::Daemon),
            )
        })?
    } else {
        Vec::new()
    };

    // When a pre-bound listener is injected (test infrastructure), use it
    // directly - skipping the bind step eliminates the TOCTOU race between
    // port allocation and daemon bind. `listeners` is later moved into the
    // accept engine; `bound_addresses` is only read by index.
    let listeners: Vec<TcpListener>;
    let bound_addresses: Vec<SocketAddr>;

    if let Some(listener) = pre_bound_listener {
        let local_addr = listener
            .local_addr()
            .unwrap_or_else(|_| SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port));
        // The listener is already bound (and, for a real `TcpListener`,
        // already listening) by the test harness that injected it, so
        // sockopts can only be applied post-hoc here. This path is
        // test-infrastructure-only; the real startup path below applies
        // sockopts pre-bind.
        apply_socket_options_to_listener(&listener, &parsed_socket_options, log_sink.as_ref());
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
            acceptor_threads,
            &parsed_socket_options,
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

    // QUIC/UDP listener (oc extension): bind it alongside the TCP listeners,
    // over the identical `resolve_bind_addresses` set, while CAP_NET_BIND_SERVICE
    // is still held (the default port 873 is privileged). The bound acceptors
    // are held for the whole daemon lifetime; the accept()->session handoff
    // lands under QUIC task #55, so for now they simply keep the UDP sockets
    // reserved next to the TCP ones.
    #[cfg(all(unix, feature = "quic"))]
    let _quic_acceptors: Vec<QuicAcceptor> = if let Some((quic_port, quic_identity)) = &quic_bind {
        match bind_quic_listeners_per_family(
            &bind_addresses,
            *quic_port,
            quic_identity,
            log_sink.as_ref(),
        ) {
            Ok(acceptors) => {
                if let Some(log) = log_sink.as_ref() {
                    let addrs: Vec<String> = acceptors
                        .iter()
                        .filter_map(|a| a.local_addr().ok())
                        .map(|a| a.to_string())
                        .collect();
                    let text = format!("QUIC listener bound on {}", addrs.join(" and "));
                    let message = rsync_info!(text).with_role(Role::Daemon);
                    log_message(log, &message);
                }
                acceptors
            }
            Err(error) => {
                let requested_addr = SocketAddr::new(bind_addresses[0], *quic_port);
                return Err(bind_error(requested_addr, error));
            }
        }
    } else {
        Vec::new()
    };

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

    // Retained for each accepted client connection - upstream: clientserver.c
    // applies set_socket_options() to the accepted fd before the session
    // handler runs, independent of the listener-side application above.
    let client_socket_options: Arc<Vec<SocketOption>> = Arc::new(parsed_socket_options);

    // Detach from terminal if --detach is active (Unix default).
    // Must happen after binding so startup errors reach stderr, and before
    // PID file creation so the file records the child's PID.
    // upstream: clientserver.c:1568-1571 -- become_daemon() called before accept loop.
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
    // upstream: clientserver.c:1337-1389 start_accept_loop() applies
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
            let daemon_gids: Vec<u32> = daemon_gid.into_iter().collect();
            drop_privileges(daemon_uid, &daemon_gids, sink).map_err(|error| {
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
        reverse_lookup,
        proxy_protocol,
    };

    // Select the accept engine once from the bound listener topology, then run
    // the shared accept loop. The engine hides the readiness mechanism
    // (non-blocking accept vs acceptor-thread fan-in) behind a uniform poll.
    let mut engine = build_accept_engine(listeners, &bound_addresses, &state)?;
    run_accept_loop(engine.as_mut(), &mut state)?;

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
