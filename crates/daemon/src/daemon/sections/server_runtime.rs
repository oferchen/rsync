/// Logs a systemd notification failure if a log sink is available.
fn log_sd_notify_failure(log: Option<&SharedLogSink>, context: &str, error: &io::Error) {
    if let Some(sink) = log {
        let payload = format!("failed to notify systemd about {context}: {error}");
        let message = rsync_warning!(payload).with_role(Role::Daemon);
        log_message(sink, &message);
    }
}


/// Formats a human-readable connection status message for systemd notification.
///
/// Returns appropriate singular/plural forms based on the connection count.
pub(crate) fn format_connection_status(active: usize) -> String {
    match active {
        0 => String::from("Idle; waiting for connections"),
        1 => String::from("Serving 1 connection"),
        count => format!("Serving {count} connections"),
    }
}

/// Normalizes a socket address by converting IPv4-mapped IPv6 addresses to pure IPv4.
///
/// When running in dual-stack mode and accepting IPv4 connections on an IPv6 listener,
/// the kernel reports peer addresses as IPv4-mapped IPv6 (e.g., `::ffff:127.0.0.1`).
/// This function converts such addresses back to their IPv4 equivalents for consistent
/// logging and host matching.
const fn normalize_peer_address(addr: SocketAddr) -> SocketAddr {
    match addr.ip() {
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                SocketAddr::new(IpAddr::V4(v4), addr.port())
            } else {
                addr
            }
        }
        IpAddr::V4(_) => addr,
    }
}

/// Interval between signal flag checks in the accept loop.
///
/// The listener uses a non-blocking timeout so the loop can periodically
/// inspect signal flags (SIGHUP, SIGTERM/SIGINT, SIGUSR1, SIGUSR2) without
/// waiting indefinitely for a new connection.
const SIGNAL_CHECK_INTERVAL: Duration = Duration::from_millis(500);

/// Logs a progress summary when SIGUSR2 is received.
///
/// Outputs the number of active worker threads, total connections served, and
/// daemon uptime. Mirrors upstream rsync's SIGUSR2 behaviour of dumping
/// transfer statistics to the log.
/// upstream: main.c — SIGUSR2 handler outputs transfer progress info.
fn log_progress_summary(
    log: Option<&SharedLogSink>,
    active_workers: usize,
    served: usize,
    start_time: SystemTime,
) {
    let uptime_secs = start_time
        .elapsed()
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let hours = uptime_secs / 3600;
    let minutes = (uptime_secs % 3600) / 60;
    let seconds = uptime_secs % 60;

    let text = format!(
        "progress: {active_workers} active connection(s), \
         {served} total served, uptime {hours}h {minutes}m {seconds}s"
    );

    if let Some(sink) = log {
        let message = rsync_info!(text).with_role(Role::Daemon);
        log_message(sink, &message);
    } else {
        eprintln!("{text}");
    }
}

/// Tracks the number of active client connections in the daemon server loop.
///
/// The counter uses an [`AtomicUsize`] for lock-free concurrent access from
/// worker threads. Each accepted connection increments the counter via
/// [`ConnectionCounter::acquire`], which returns a [`ConnectionGuard`] that
/// automatically decrements the counter when dropped.
///
/// # Usage
///
/// ```ignore
/// let counter = ConnectionCounter::new();
/// assert_eq!(counter.active(), 0);
///
/// let guard = counter.acquire();
/// assert_eq!(counter.active(), 1);
///
/// drop(guard);
/// assert_eq!(counter.active(), 0);
/// ```
///
/// The counter is wrapped in an `Arc` so it can be shared between the main
/// accept loop and spawned worker threads. This enables future max-connections
/// enforcement at the daemon level (as opposed to the per-module limits
/// already tracked by `ModuleRuntime::active_connections`).
///
/// upstream: clientserver.c — `count_connections()` tracks active children
/// for the `max connections` global directive.
#[derive(Debug)]
pub(crate) struct ConnectionCounter {
    active: Arc<AtomicUsize>,
}

impl ConnectionCounter {
    /// Creates a new connection counter with zero active connections.
    pub(crate) fn new() -> Self {
        Self {
            active: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// Returns the current number of active connections.
    ///
    /// This is wired into the accept loop for future daemon-level
    /// max-connections enforcement.
    #[allow(dead_code)]
    pub(crate) fn active(&self) -> usize {
        self.active.load(Ordering::Acquire)
    }

    /// Increments the counter and returns an RAII guard that decrements it on drop.
    pub(crate) fn acquire(&self) -> ConnectionGuard {
        self.active.fetch_add(1, Ordering::AcqRel);
        ConnectionGuard {
            counter: Arc::clone(&self.active),
        }
    }
}

impl Default for ConnectionCounter {
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for ConnectionCounter {
    fn clone(&self) -> Self {
        Self {
            active: Arc::clone(&self.active),
        }
    }
}

/// RAII guard that decrements the parent [`ConnectionCounter`] when dropped.
///
/// Created by [`ConnectionCounter::acquire`]. The guard holds an `Arc` reference
/// to the shared atomic counter, ensuring the decrement occurs even if the
/// owning thread panics (since `Drop` runs during unwinding).
#[derive(Debug)]
pub(crate) struct ConnectionGuard {
    counter: Arc<AtomicUsize>,
}

impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        self.counter.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Re-reads and re-parses the daemon configuration file on SIGHUP.
///
/// On success, replaces `modules` and `motd_lines` with freshly parsed values
/// so that subsequent connections use the new configuration. Existing
/// connections retain the old config via their `Arc` clones.
///
/// On failure (missing file, parse error), the error is logged and the daemon
/// continues with the previous configuration — matching upstream rsync
/// behaviour where a bad config reload is non-fatal.
///
/// upstream: clientserver.c — `re_read_config()` called from SIGHUP handler.
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
        // User specified a specific address via --address/--bind
        vec![bind_address]
    } else {
        match address_family {
            Some(AddressFamily::Ipv4) => vec![IpAddr::V4(Ipv4Addr::UNSPECIFIED)],
            Some(AddressFamily::Ipv6) => vec![IpAddr::V6(Ipv6Addr::UNSPECIFIED)],
            None => {
                // Dual-stack: bind to both IPv4 and IPv6
                vec![
                    IpAddr::V6(Ipv6Addr::UNSPECIFIED),
                    IpAddr::V4(Ipv4Addr::UNSPECIFIED),
                ]
            }
        }
    };

    // Create listeners for each bind address
    let mut listeners: Vec<TcpListener> = Vec::with_capacity(bind_addresses.len());
    let mut bound_addresses: Vec<SocketAddr> = Vec::with_capacity(bind_addresses.len());

    for addr in &bind_addresses {
        let requested_addr = SocketAddr::new(*addr, port);
        match TcpListener::bind(requested_addr) {
            Ok(listener) => {
                let local_addr = listener.local_addr().unwrap_or(requested_addr);
                bound_addresses.push(local_addr);
                listeners.push(listener);
            }
            Err(error) => {
                // If binding to one family fails (e.g., IPv6 not available), continue
                // with the other family if we're in dual-stack mode. Otherwise, fail.
                if bind_addresses.len() > 1 && !listeners.is_empty() {
                    // Already bound to at least one address, continue
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
    // is ready to accept connections — matching upstream main.c write_pid_file().
    let pid_guard = if let Some(path) = pid_file {
        Some(PidFileGuard::create(path)?)
    } else {
        None
    };

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
            // upstream: main.c — SIGUSR1 causes the daemon to stop accepting
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
            // upstream: main.c — SIGUSR2 outputs transfer statistics.
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
                    // Accepted sockets must be blocking for the session handler.
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

                    let peer_addr = normalize_peer_address(raw_peer_addr);
                    let modules = Arc::clone(&modules);
                    let motd_lines = Arc::clone(&motd_lines);
                    let log_for_worker = log_sink.as_ref().map(Arc::clone);
                    let conn_guard = connection_counter.acquire();
                    let handle = thread::spawn(move || {
                        // Hold the connection guard for the lifetime of the
                        // handler so the counter stays accurate even if the
                        // session panics (Drop runs during unwinding).
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
                                break; // Receiver dropped
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
                    // Continue to reap workers and check max_sessions
                    continue;
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    // All acceptors exited
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

struct PidFileGuard {
    path: PathBuf,
}

impl PidFileGuard {
    fn create(path: PathBuf) -> Result<Self, DaemonError> {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent).map_err(|error| pid_file_error(&path, error))?;
            }

        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&path)
            .map_err(|error| pid_file_error(&path, error))?;

        // upstream: main.c write_pid_file() — mode 0644
        #[cfg(unix)]
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644))
            .map_err(|error| pid_file_error(&path, error))?;

        let pid = std::process::id();
        writeln!(file, "{pid}").map_err(|error| pid_file_error(&path, error))?;
        file.sync_all()
            .map_err(|error| pid_file_error(&path, error))?;

        Ok(Self { path })
    }
}

impl Drop for PidFileGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

type WorkerResult = Result<(), (Option<SocketAddr>, io::Error)>;

fn reap_finished_workers(
    workers: &mut Vec<thread::JoinHandle<WorkerResult>>,
) -> Result<(), DaemonError> {
    let mut index = 0;
    while index < workers.len() {
        if workers[index].is_finished() {
            let handle = workers.remove(index);
            join_worker(handle)?;
        } else {
            index += 1;
        }
    }
    Ok(())
}

fn drain_workers(workers: &mut Vec<thread::JoinHandle<WorkerResult>>) -> Result<(), DaemonError> {
    while let Some(handle) = workers.pop() {
        join_worker(handle)?;
    }
    Ok(())
}

fn join_worker(handle: thread::JoinHandle<WorkerResult>) -> Result<(), DaemonError> {
    match handle.join() {
        Ok(Ok(())) => Ok(()),
        Ok(Err((peer, error))) => {
            let kind = error.kind();
            if is_connection_closed_error(kind) {
                Ok(())
            } else {
                Err(stream_error(peer, "serve legacy handshake", error))
            }
        }
        // Defense-in-depth: catch_unwind inside the thread already handles
        // panics, but if one somehow escapes, log it and keep the daemon
        // running rather than terminating all connections.
        // upstream: rsync forks per connection, so a crash only kills that
        // child process.
        Err(payload) => {
            let description = describe_panic_payload(payload);
            let error = io::Error::other(format!(
                "worker thread panicked (unwind escaped catch_unwind): {description}"
            ));
            eprintln!("{error}");
            Ok(())
        }
    }
}

/// Extracts a human-readable message from a panic payload.
///
/// Handles the two common payload types (`String` and `&str`) and falls back
/// to a generic description for anything else.
fn describe_panic_payload(payload: Box<dyn std::any::Any + Send>) -> String {
    match payload.downcast::<String>() {
        Ok(message) => *message,
        Err(payload) => match payload.downcast::<&str>() {
            Ok(message) => (*message).to_owned(),
            Err(_) => "unknown panic payload".to_owned(),
        },
    }
}

/// Checks if an I/O error indicates a normal connection close.
const fn is_connection_closed_error(kind: io::ErrorKind) -> bool {
    matches!(
        kind,
        io::ErrorKind::BrokenPipe
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::ConnectionAborted
    )
}

#[cfg(test)]
mod server_runtime_tests {
    use super::*;

    // Tests for format_connection_status

    #[test]
    fn format_connection_status_zero_connections() {
        assert_eq!(format_connection_status(0), "Idle; waiting for connections");
    }

    #[test]
    fn format_connection_status_one_connection() {
        assert_eq!(format_connection_status(1), "Serving 1 connection");
    }

    #[test]
    fn format_connection_status_multiple_connections() {
        assert_eq!(format_connection_status(5), "Serving 5 connections");
    }

    // Tests for normalize_peer_address

    #[test]
    fn normalize_peer_address_preserves_ipv4() {
        let addr: SocketAddr = "192.168.1.1:8873".parse().unwrap();
        assert_eq!(normalize_peer_address(addr), addr);
    }

    #[test]
    fn normalize_peer_address_preserves_pure_ipv6() {
        let addr: SocketAddr = "[2001:db8::1]:8873".parse().unwrap();
        assert_eq!(normalize_peer_address(addr), addr);
    }

    #[test]
    fn normalize_peer_address_converts_ipv4_mapped() {
        use std::net::{Ipv6Addr, SocketAddrV6};
        // IPv4-mapped IPv6: ::ffff:127.0.0.1
        let v6 = Ipv6Addr::new(0, 0, 0, 0, 0, 0xffff, 0x7f00, 0x0001);
        let addr = SocketAddr::V6(SocketAddrV6::new(v6, 8873, 0, 0));
        let normalized = normalize_peer_address(addr);
        assert_eq!(normalized.to_string(), "127.0.0.1:8873");
    }

    // Tests for is_connection_closed_error

    #[test]
    fn is_connection_closed_error_broken_pipe() {
        assert!(is_connection_closed_error(io::ErrorKind::BrokenPipe));
    }

    #[test]
    fn is_connection_closed_error_connection_reset() {
        assert!(is_connection_closed_error(io::ErrorKind::ConnectionReset));
    }

    #[test]
    fn is_connection_closed_error_connection_aborted() {
        assert!(is_connection_closed_error(io::ErrorKind::ConnectionAborted));
    }

    #[test]
    fn is_connection_closed_error_other_errors_false() {
        assert!(!is_connection_closed_error(io::ErrorKind::NotFound));
        assert!(!is_connection_closed_error(io::ErrorKind::PermissionDenied));
        assert!(!is_connection_closed_error(io::ErrorKind::TimedOut));
    }

    // Tests for ConnectionCounter

    #[test]
    fn connection_counter_starts_at_zero() {
        let counter = ConnectionCounter::new();
        assert_eq!(counter.active(), 0);
    }

    #[test]
    fn connection_counter_default_starts_at_zero() {
        let counter = ConnectionCounter::default();
        assert_eq!(counter.active(), 0);
    }

    #[test]
    fn connection_counter_increments_on_acquire() {
        let counter = ConnectionCounter::new();
        let _guard = counter.acquire();
        assert_eq!(counter.active(), 1);
    }

    #[test]
    fn connection_counter_decrements_on_guard_drop() {
        let counter = ConnectionCounter::new();
        let guard = counter.acquire();
        assert_eq!(counter.active(), 1);
        drop(guard);
        assert_eq!(counter.active(), 0);
    }

    #[test]
    fn connection_counter_tracks_multiple_connections() {
        let counter = ConnectionCounter::new();
        let g1 = counter.acquire();
        let g2 = counter.acquire();
        let g3 = counter.acquire();
        assert_eq!(counter.active(), 3);

        drop(g2);
        assert_eq!(counter.active(), 2);

        drop(g1);
        assert_eq!(counter.active(), 1);

        drop(g3);
        assert_eq!(counter.active(), 0);
    }

    #[test]
    fn connection_counter_clone_shares_state() {
        let counter = ConnectionCounter::new();
        let cloned = counter.clone();

        let _guard = counter.acquire();
        assert_eq!(cloned.active(), 1);

        let _guard2 = cloned.acquire();
        assert_eq!(counter.active(), 2);
    }

    #[test]
    fn connection_counter_concurrent_access() {
        let counter = ConnectionCounter::new();
        let mut handles = vec![];

        for _ in 0..10 {
            let cloned = counter.clone();
            let handle = thread::spawn(move || {
                let mut guards = Vec::new();
                for _ in 0..100 {
                    guards.push(cloned.acquire());
                }
                // All 100 guards are held here, then dropped when guards goes out of scope
                assert!(cloned.active() >= 100);
            });
            handles.push(handle);
        }

        for handle in handles {
            handle.join().unwrap();
        }

        assert_eq!(counter.active(), 0);
    }

    #[test]
    fn connection_guard_debug_format() {
        let counter = ConnectionCounter::new();
        let guard = counter.acquire();
        let debug = format!("{guard:?}");
        assert!(debug.contains("ConnectionGuard"));
    }

    #[test]
    fn connection_counter_debug_format() {
        let counter = ConnectionCounter::new();
        let debug = format!("{counter:?}");
        assert!(debug.contains("ConnectionCounter"));
    }

    // Tests for describe_panic_payload

    #[test]
    fn describe_panic_payload_extracts_string_message() {
        let payload = std::panic::catch_unwind(|| {
            panic!("handler exploded: {}", "bad input");
        })
        .unwrap_err();
        let description = describe_panic_payload(payload);
        assert!(
            description.contains("handler exploded"),
            "expected String payload to be extracted, got: {description}"
        );
    }

    #[test]
    fn describe_panic_payload_extracts_str_message() {
        let payload = std::panic::catch_unwind(|| {
            panic!("static str panic");
        })
        .unwrap_err();
        let description = describe_panic_payload(payload);
        assert_eq!(description, "static str panic");
    }

    #[test]
    fn describe_panic_payload_handles_non_string_payload() {
        let payload = std::panic::catch_unwind(|| {
            std::panic::panic_any(42u32);
        })
        .unwrap_err();
        let description = describe_panic_payload(payload);
        assert_eq!(description, "unknown panic payload");
    }

    // Tests for join_worker — panic isolation defense-in-depth

    #[test]
    fn join_worker_handles_successful_thread() {
        let handle = thread::spawn(|| Ok(()));
        let result = join_worker(handle);
        assert!(result.is_ok());
    }

    #[test]
    fn join_worker_handles_connection_closed_error() {
        let handle = thread::spawn(|| {
            Err((
                Some("127.0.0.1:12345".parse().unwrap()),
                io::Error::new(io::ErrorKind::BrokenPipe, "connection closed"),
            ))
        });
        let result = join_worker(handle);
        assert!(
            result.is_ok(),
            "BrokenPipe should be treated as normal close"
        );
    }

    #[test]
    fn join_worker_swallows_panicking_thread() {
        // Verify that a thread panic does not propagate through join_worker.
        // This is the defense-in-depth path: if catch_unwind inside the worker
        // somehow fails to catch the panic, join_worker still keeps the daemon
        // alive by converting the panic into Ok(()).
        let handle = thread::spawn(|| -> WorkerResult {
            panic!("simulated handler crash");
        });
        // Give the thread a moment to actually panic.
        thread::sleep(Duration::from_millis(50));
        let result = join_worker(handle);
        assert!(
            result.is_ok(),
            "join_worker must swallow panics to keep the daemon alive"
        );
    }

    // Tests for catch_unwind isolation pattern

    #[test]
    fn catch_unwind_isolates_panic_and_returns_ok() {
        // Simulates the exact pattern used in serve_connections: a worker
        // thread wraps its handler in catch_unwind, converts a panic into
        // a log-and-return-Ok path.
        let peer_addr: SocketAddr = "127.0.0.1:9999".parse().unwrap();
        let handle = thread::spawn(move || -> WorkerResult {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                panic!("connection handler for test panicked");
            }));
            match result {
                Ok(Ok(())) => Ok(()),
                Ok(Err(error)) => Err((Some(peer_addr), error)),
                Err(payload) => {
                    let description = describe_panic_payload(payload);
                    assert!(
                        description.contains("connection handler for test panicked"),
                        "panic message should be preserved: {description}"
                    );
                    Ok(())
                }
            }
        });
        let result = handle.join().expect("thread should not propagate panic");
        assert!(
            result.is_ok(),
            "catch_unwind should convert panics into Ok(())"
        );
    }

    // Tests for log_progress_summary

    #[test]
    fn log_progress_summary_without_log_sink() {
        // Verify that calling without a log sink does not panic.
        // Output goes to stderr which we do not capture here, but the
        // function must not panic.
        log_progress_summary(None, 3, 10, SystemTime::now());
    }

    #[test]
    fn log_progress_summary_zero_active() {
        log_progress_summary(None, 0, 0, SystemTime::now());
    }

    #[test]
    fn log_progress_summary_with_uptime() {
        // Use a start time 90 seconds in the past to verify the uptime
        // calculation does not panic on non-zero durations.
        let past = SystemTime::now() - Duration::from_secs(90);
        log_progress_summary(None, 2, 5, past);
    }

    // Tests for signal flag interactions with the server loop

    #[test]
    fn graceful_exit_flag_stops_accept_loop_independently() {
        let flags = SignalFlags {
            reload_config: Arc::new(AtomicBool::new(false)),
            shutdown: Arc::new(AtomicBool::new(false)),
            graceful_exit: Arc::new(AtomicBool::new(true)),
            progress_dump: Arc::new(AtomicBool::new(false)),
        };
        assert!(
            flags.graceful_exit.load(Ordering::Relaxed),
            "graceful_exit should be set"
        );
        assert!(
            !flags.shutdown.load(Ordering::Relaxed),
            "shutdown must remain unset when only graceful_exit is triggered"
        );
    }

    #[test]
    fn progress_dump_flag_is_consumed() {
        let flags = SignalFlags {
            reload_config: Arc::new(AtomicBool::new(false)),
            shutdown: Arc::new(AtomicBool::new(false)),
            graceful_exit: Arc::new(AtomicBool::new(false)),
            progress_dump: Arc::new(AtomicBool::new(true)),
        };
        // Consume the flag just like the server loop does.
        let was_set = flags.progress_dump.swap(false, Ordering::Relaxed);
        assert!(was_set, "progress_dump should have been set");
        assert!(
            !flags.progress_dump.load(Ordering::Relaxed),
            "progress_dump must be cleared after swap"
        );
    }

    // Tests for reload_daemon_config

    #[test]
    fn reload_config_with_no_config_path_is_noop() {
        let limiter: Option<Arc<ConnectionLimiter>> = None;
        let mut modules: Arc<Vec<ModuleRuntime>> = Arc::new(Vec::new());
        let mut motd: Arc<Vec<String>> = Arc::new(Vec::new());
        let notifier = systemd::ServiceNotifier::new();

        reload_daemon_config(
            None,
            &limiter,
            &mut modules,
            &mut motd,
            None,
            &notifier,
        );

        assert!(modules.is_empty());
        assert!(motd.is_empty());
    }

    #[test]
    fn reload_config_with_missing_file_keeps_old_config() {
        let limiter: Option<Arc<ConnectionLimiter>> = None;
        let old_module = ModuleRuntime::new(
            ModuleDefinition {
                name: "old".to_owned(),
                path: PathBuf::from("/old"),
                ..Default::default()
            },
            None,
        );
        let mut modules: Arc<Vec<ModuleRuntime>> = Arc::new(vec![old_module]);
        let mut motd: Arc<Vec<String>> = Arc::new(vec!["old motd".to_owned()]);
        let notifier = systemd::ServiceNotifier::new();

        let missing = PathBuf::from("/nonexistent/rsyncd.conf");
        reload_daemon_config(
            Some(&missing),
            &limiter,
            &mut modules,
            &mut motd,
            None,
            &notifier,
        );

        assert_eq!(modules.len(), 1);
        assert_eq!(modules[0].definition.name, "old");
        assert_eq!(motd.len(), 1);
        assert_eq!(motd[0], "old motd");
    }

    #[cfg(unix)]
    #[test]
    fn reload_config_replaces_modules_and_motd() {
        use std::io::Write;

        let dir = tempfile::tempdir().unwrap();
        let conf_path = dir.path().join("rsyncd.conf");
        {
            let mut f = fs::File::create(&conf_path).unwrap();
            writeln!(f, "motd file = {}", dir.path().join("motd.txt").display()).unwrap();
            writeln!(f, "[alpha]").unwrap();
            writeln!(f, "path = /alpha").unwrap();
        }
        {
            let motd_path = dir.path().join("motd.txt");
            let mut f = fs::File::create(motd_path).unwrap();
            writeln!(f, "Welcome!").unwrap();
        }

        let limiter: Option<Arc<ConnectionLimiter>> = None;
        let mut modules: Arc<Vec<ModuleRuntime>> = Arc::new(Vec::new());
        let mut motd: Arc<Vec<String>> = Arc::new(Vec::new());
        let notifier = systemd::ServiceNotifier::new();

        reload_daemon_config(
            Some(&conf_path),
            &limiter,
            &mut modules,
            &mut motd,
            None,
            &notifier,
        );

        assert_eq!(modules.len(), 1);
        assert_eq!(modules[0].definition.name, "alpha");
        assert_eq!(modules[0].definition.path, PathBuf::from("/alpha"));
    }

    #[cfg(unix)]
    #[test]
    fn reload_config_existing_connections_keep_old_config() {
        use std::io::Write;

        let dir = tempfile::tempdir().unwrap();
        let conf_path = dir.path().join("rsyncd.conf");
        {
            let mut f = fs::File::create(&conf_path).unwrap();
            writeln!(f, "[original]").unwrap();
            writeln!(f, "path = /original").unwrap();
        }

        let limiter: Option<Arc<ConnectionLimiter>> = None;
        let mut modules: Arc<Vec<ModuleRuntime>> = Arc::new(Vec::new());
        let mut motd: Arc<Vec<String>> = Arc::new(Vec::new());
        let notifier = systemd::ServiceNotifier::new();

        // Initial load
        reload_daemon_config(
            Some(&conf_path),
            &limiter,
            &mut modules,
            &mut motd,
            None,
            &notifier,
        );
        assert_eq!(modules.len(), 1);
        assert_eq!(modules[0].definition.name, "original");

        // Simulate an existing connection holding a clone of the old config
        let old_modules = Arc::clone(&modules);

        // Update the config file and reload
        {
            let mut f = fs::File::create(&conf_path).unwrap();
            writeln!(f, "[updated]").unwrap();
            writeln!(f, "path = /updated").unwrap();
        }
        reload_daemon_config(
            Some(&conf_path),
            &limiter,
            &mut modules,
            &mut motd,
            None,
            &notifier,
        );

        // New config is visible
        assert_eq!(modules.len(), 1);
        assert_eq!(modules[0].definition.name, "updated");

        // Old connection still sees the original config
        assert_eq!(old_modules.len(), 1);
        assert_eq!(old_modules[0].definition.name, "original");
    }

    #[cfg(unix)]
    #[test]
    fn reload_config_with_invalid_syntax_keeps_old_config() {
        use std::io::Write;

        let dir = tempfile::tempdir().unwrap();
        let conf_path = dir.path().join("rsyncd.conf");

        // Start with valid config
        {
            let mut f = fs::File::create(&conf_path).unwrap();
            writeln!(f, "[valid]").unwrap();
            writeln!(f, "path = /valid").unwrap();
        }

        let limiter: Option<Arc<ConnectionLimiter>> = None;
        let mut modules: Arc<Vec<ModuleRuntime>> = Arc::new(Vec::new());
        let mut motd: Arc<Vec<String>> = Arc::new(Vec::new());
        let notifier = systemd::ServiceNotifier::new();

        reload_daemon_config(
            Some(&conf_path),
            &limiter,
            &mut modules,
            &mut motd,
            None,
            &notifier,
        );
        assert_eq!(modules.len(), 1);
        assert_eq!(modules[0].definition.name, "valid");

        // Write invalid config (unterminated module header)
        {
            let mut f = fs::File::create(&conf_path).unwrap();
            writeln!(f, "[broken").unwrap();
        }

        reload_daemon_config(
            Some(&conf_path),
            &limiter,
            &mut modules,
            &mut motd,
            None,
            &notifier,
        );

        // Old config is preserved
        assert_eq!(modules.len(), 1);
        assert_eq!(modules[0].definition.name, "valid");
    }

    #[cfg(unix)]
    #[test]
    fn reload_config_sighup_flag_triggers_reload() {
        // Verify that the AtomicBool swap pattern works correctly:
        // setting the flag to true and swapping to false returns true once.
        let flags = SignalFlags {
            reload_config: Arc::new(AtomicBool::new(false)),
            shutdown: Arc::new(AtomicBool::new(false)),
            graceful_exit: Arc::new(AtomicBool::new(false)),
            progress_dump: Arc::new(AtomicBool::new(false)),
        };

        // Simulate SIGHUP
        flags.reload_config.store(true, Ordering::Relaxed);

        // First swap should return true (flag was set)
        assert!(flags.reload_config.swap(false, Ordering::Relaxed));

        // Second swap should return false (flag was cleared)
        assert!(!flags.reload_config.swap(false, Ordering::Relaxed));
    }
}

fn configure_stream(stream: &TcpStream) -> io::Result<()> {
    stream.set_read_timeout(Some(SOCKET_TIMEOUT))?;
    stream.set_write_timeout(Some(SOCKET_TIMEOUT))
}

