fn log_sd_notify_failure(log: Option<&SharedLogSink>, context: &str, error: &io::Error) {
    if let Some(sink) = log {
        let payload = format!("failed to notify systemd about {context}: {error}");
        let message = rsync_warning!(payload).with_role(Role::Daemon);
        log_message(sink, &message);
    }
}

struct GeneratedFallbackConfig {
    config: NamedTempFile,
    _motd: Option<NamedTempFile>,
}

impl GeneratedFallbackConfig {
    fn config_path(&self) -> &Path {
        self.config.path()
    }
}

fn format_bool(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

fn host_pattern_token(pattern: &HostPattern) -> String {
    match pattern {
        HostPattern::Any => String::from("*"),
        HostPattern::Ipv4 { network, prefix } => format!("{network}/{prefix}"),
        HostPattern::Ipv6 { network, prefix } => format!("{network}/{prefix}"),
        HostPattern::Hostname(pattern) => match pattern.kind {
            HostnamePatternKind::Exact(ref exact) => exact.clone(),
            HostnamePatternKind::Suffix(ref suffix) => {
                let mut token = String::with_capacity(suffix.len() + 1);
                token.push('.');
                token.push_str(suffix);
                token
            }
            HostnamePatternKind::Wildcard(ref wildcard) => wildcard.clone(),
        },
    }
}

fn join_pattern_tokens(patterns: &[HostPattern]) -> Option<String> {
    if patterns.is_empty() {
        None
    } else {
        Some(
            patterns
                .iter()
                .map(host_pattern_token)
                .collect::<Vec<_>>()
                .join(" "),
        )
    }
}

fn render_auth_users(users: &[String]) -> Option<String> {
    if users.is_empty() {
        None
    } else {
        Some(users.join(","))
    }
}

fn render_refused_options(options: &[String]) -> Option<String> {
    if options.is_empty() {
        None
    } else {
        Some(options.join(" "))
    }
}

fn render_optional_u32(value: Option<u32>) -> Option<String> {
    value.map(|id| id.to_string())
}

fn render_optional_timeout(value: Option<NonZeroU64>) -> Option<String> {
    value.map(|timeout| timeout.get().to_string())
}

fn render_optional_bwlimit(limit: Option<NonZeroU64>) -> Option<String> {
    limit.map(|rate| rate.get().to_string())
}

fn render_chmod(value: Option<&str>) -> Option<String> {
    value.map(str::to_string)
}

fn generate_fallback_config(
    modules: &[ModuleDefinition],
    motd_lines: &[String],
) -> io::Result<Option<GeneratedFallbackConfig>> {
    if modules.is_empty() && motd_lines.is_empty() {
        return Ok(None);
    }

    let motd_file = if motd_lines.is_empty() {
        None
    } else {
        let mut file = NamedTempFile::new()?;
        for line in motd_lines {
            writeln!(file, "{line}")?;
        }
        Some(file)
    };

    let mut config = NamedTempFile::new()?;

    if let Some(motd) = motd_file.as_ref() {
        writeln!(config, "motd file = {}", motd.path().display())?;
    }

    for module in modules {
        writeln!(config, "[{}]", module.name)?;
        writeln!(config, "    path = {}", module.path.display())?;

        if let Some(comment) = &module.comment
            && !comment.is_empty() {
                writeln!(config, "    comment = {comment}")?;
            }

        if let Some(allowed) = join_pattern_tokens(&module.hosts_allow) {
            writeln!(config, "    hosts allow = {allowed}")?;
        }

        if let Some(denied) = join_pattern_tokens(&module.hosts_deny) {
            writeln!(config, "    hosts deny = {denied}")?;
        }

        if let Some(users) = render_auth_users(&module.auth_users) {
            writeln!(config, "    auth users = {users}")?;
        }

        if let Some(secrets) = module.secrets_file.as_ref() {
            writeln!(config, "    secrets file = {}", secrets.display())?;
        }

        if let Some(bwlimit) = render_optional_bwlimit(module.bandwidth_limit) {
            writeln!(config, "    bwlimit = {bwlimit}")?;
        }

        if let Some(options) = render_refused_options(&module.refuse_options) {
            writeln!(config, "    refuse options = {options}")?;
        }

        writeln!(config, "    read only = {}", format_bool(module.read_only))?;
        writeln!(config, "    write only = {}", format_bool(module.write_only))?;
        writeln!(config, "    list = {}", format_bool(module.listable))?;
        writeln!(config, "    use chroot = {}", format_bool(module.use_chroot))?;
        writeln!(config, "    numeric ids = {}", format_bool(module.numeric_ids))?;

        if let Some(uid) = render_optional_u32(module.uid) {
            writeln!(config, "    uid = {uid}")?;
        }

        if let Some(gid) = render_optional_u32(module.gid) {
            writeln!(config, "    gid = {gid}")?;
        }

        if let Some(timeout) = render_optional_timeout(module.timeout) {
            writeln!(config, "    timeout = {timeout}")?;
        }

        if let Some(max) = module.max_connections {
            writeln!(config, "    max connections = {}", max.get())?;
        }

        if let Some(chmod) = render_chmod(module.incoming_chmod.as_deref()) {
            writeln!(config, "    incoming chmod = {chmod}")?;
        }

        if let Some(chmod) = render_chmod(module.outgoing_chmod.as_deref()) {
            writeln!(config, "    outgoing chmod = {chmod}")?;
        }

        writeln!(config)?;
    }

    config.flush()?;

    Ok(Some(GeneratedFallbackConfig {
        config,
        _motd: motd_file,
    }))
}

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
fn normalize_peer_address(addr: SocketAddr) -> SocketAddr {
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

fn serve_connections(options: RuntimeOptions) -> Result<(), DaemonError> {
    let manifest = manifest();
    let version = manifest.rust_version();
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
        delegate_arguments,
        inline_modules,
        address_family,
        bind_address_overridden,
        ..
    } = options;

    let mut fallback_warning_message: Option<Message> = None;
    let mut delegate_arguments = delegate_arguments;
    let mut generated_config: Option<GeneratedFallbackConfig> = None;

    if let Some(reason) = fallback_disabled_reason() {
        fallback_warning_message = Some(rsync_warning!(reason).with_role(Role::Daemon));
    }

    if inline_modules {
        generated_config = generate_fallback_config(&modules, &motd_lines).map_err(|error| {
            DaemonError::new(
                FEATURE_UNAVAILABLE_EXIT_CODE,
                rsync_error!(
                    FEATURE_UNAVAILABLE_EXIT_CODE,
                    format!("failed to prepare fallback config: {error}")
                )
                .with_role(Role::Daemon),
            )
        })?;
        if let Some(config) = generated_config.as_ref() {
            delegate_arguments.push(OsString::from("--config"));
            delegate_arguments.push(config.config_path().as_os_str().to_owned());
        }
    }

    let delegation = if fallback_warning_message.is_none() {
        if let Some(binary) = configured_fallback_binary() {
            if fallback_binary_available(binary.as_os_str()) {
                Some(SessionDelegation::new(binary, delegate_arguments))
            } else {
                let warning_text = describe_missing_fallback_binary(
                    binary.as_os_str(),
                    &[DAEMON_FALLBACK_ENV, CLIENT_FALLBACK_ENV],
                );
                fallback_warning_message = Some(rsync_warning!(warning_text).with_role(Role::Daemon));
                None
            }
        } else {
            None
        }
    } else {
        None
    };

    let _generated_config_guard = generated_config;

    let pid_guard = if let Some(path) = pid_file {
        Some(PidFileGuard::create(path)?)
    } else {
        None
    };

    // Warning message removed - eprintln! crashes when stderr unavailable in daemon mode
    // If fallback warning needs to be logged, use proper logging framework instead
    let _ = fallback_warning_message;

    let log_sink = if let Some(path) = log_file {
        Some(open_log_sink(&path, Brand::Oc)?)
    } else {
        None
    };

    if let (Some(log), Some(message)) = (log_sink.as_ref(), fallback_warning_message.as_ref()) {
        log_message(log, message);
    }

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

    // For dual-stack (multiple listeners), use channels to receive accepted connections
    // from listener threads. For single listener, use direct accept for simplicity.
    use std::sync::mpsc;

    if listeners.len() == 1 {
        // Single listener - use simple blocking accept loop
        let listener = listeners.remove(0);
        let local_addr = bound_addresses[0];

        loop {
            reap_finished_workers(&mut workers)?;

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
                    let peer_addr = normalize_peer_address(raw_peer_addr);
                    let modules = Arc::clone(&modules);
                    let motd_lines = Arc::clone(&motd_lines);
                    let log_for_worker = log_sink.as_ref().map(Arc::clone);
                    let delegation_clone = delegation.clone();
                    let handle = thread::spawn(move || {
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
                                log_sink: log_for_worker,
                                reverse_lookup,
                                delegation: delegation_clone,
                            },
                        )
                        .map_err(|error| (Some(peer_addr), error))
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
        // Multiple listeners (dual-stack) - spawn acceptor threads and use channel
        let (tx, rx) = mpsc::channel::<Result<(TcpStream, SocketAddr), (SocketAddr, io::Error)>>();
        let shutdown = Arc::new(AtomicBool::new(false));

        let mut acceptor_handles: Vec<thread::JoinHandle<()>> = Vec::with_capacity(listeners.len());

        for (listener, local_addr) in listeners.into_iter().zip(bound_addresses.iter().cloned()) {
            let tx = tx.clone();
            let shutdown = Arc::clone(&shutdown);

            let handle = thread::spawn(move || {
                while !shutdown.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((stream, peer_addr)) => {
                            if tx.send(Ok((stream, peer_addr))).is_err() {
                                break; // Receiver dropped
                            }
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

            let current_active = workers.len();
            if current_active != active_connections {
                let status = format_connection_status(current_active);
                if let Err(error) = notifier.status(&status) {
                    log_sd_notify_failure(log_sink.as_ref(), "connection status update", &error);
                }
                active_connections = current_active;
            }

            // Use recv_timeout to allow periodic worker reaping and shutdown checks
            match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(Ok((stream, raw_peer_addr))) => {
                    let peer_addr = normalize_peer_address(raw_peer_addr);
                    let modules = Arc::clone(&modules);
                    let motd_lines = Arc::clone(&motd_lines);
                    let log_for_worker = log_sink.as_ref().map(Arc::clone);
                    let delegation_clone = delegation.clone();
                    let handle = thread::spawn(move || {
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
                                log_sink: log_for_worker,
                                reverse_lookup,
                                delegation: delegation_clone,
                            },
                        )
                        .map_err(|error| (Some(peer_addr), error))
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
        Err(panic) => {
            let description = match panic.downcast::<String>() {
                Ok(message) => *message,
                Err(payload) => match payload.downcast::<&str>() {
                    Ok(message) => (*message).to_string(),
                    Err(_) => "worker thread panicked".to_string(),
                },
            };
            let error = io::Error::other(description);
            Err(stream_error(None, "serve legacy handshake", error))
        }
    }
}

fn is_connection_closed_error(kind: io::ErrorKind) -> bool {
    matches!(
        kind,
        io::ErrorKind::BrokenPipe
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::ConnectionAborted
    )
}

fn configure_stream(stream: &TcpStream) -> io::Result<()> {
    stream.set_read_timeout(Some(SOCKET_TIMEOUT))?;
    stream.set_write_timeout(Some(SOCKET_TIMEOUT))
}

