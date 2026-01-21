/// Logs a systemd notification failure if a log sink is available.
fn log_sd_notify_failure(log: Option<&SharedLogSink>, context: &str, error: &io::Error) {
    if let Some(sink) = log {
        let payload = format!("failed to notify systemd about {context}: {error}");
        let message = rsync_warning!(payload).with_role(Role::Daemon);
        log_message(sink, &message);
    }
}

/// Holds temporary files for an auto-generated fallback config.
struct GeneratedFallbackConfig {
    config: NamedTempFile,
    _motd: Option<NamedTempFile>,
}

impl GeneratedFallbackConfig {
    /// Returns the path to the generated config file.
    fn config_path(&self) -> &Path {
        self.config.path()
    }
}

/// Formats a boolean value for rsync config files ("yes" or "no").
const fn format_bool(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

/// Converts a [`HostPattern`] to its config file string representation.
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

/// Joins host patterns into a space-separated string for config output.
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

/// Joins a list of strings with a delimiter, returning `None` if empty.
fn join_list_if_nonempty(items: &[String], delimiter: &str) -> Option<String> {
    if items.is_empty() {
        None
    } else {
        Some(items.join(delimiter))
    }
}

/// Renders auth users as a comma-separated list for config output.
///
/// Access level suffixes are included for non-default access levels:
/// - `:ro` for ReadOnly
/// - `:rw` for ReadWrite
/// - `:deny` for Deny
fn render_auth_users(users: &[AuthUser]) -> Option<String> {
    if users.is_empty() {
        return None;
    }

    let rendered: Vec<String> = users
        .iter()
        .map(|user| {
            match user.access_level {
                UserAccessLevel::Default => user.username.clone(),
                UserAccessLevel::ReadOnly => format!("{}:ro", user.username),
                UserAccessLevel::ReadWrite => format!("{}:rw", user.username),
                UserAccessLevel::Deny => format!("{}:deny", user.username),
            }
        })
        .collect();

    Some(rendered.join(","))
}

/// Renders refused options as a space-separated list for config output.
fn render_refused_options(options: &[String]) -> Option<String> {
    join_list_if_nonempty(options, " ")
}

/// Renders an optional u32 value as a string for config output.
fn render_optional_u32(value: Option<u32>) -> Option<String> {
    value.map(|id| id.to_string())
}

/// Renders an optional timeout value as a string for config output.
fn render_optional_timeout(value: Option<NonZeroU64>) -> Option<String> {
    value.map(|timeout| timeout.get().to_string())
}

/// Renders an optional bandwidth limit as a string for config output.
fn render_optional_bwlimit(limit: Option<NonZeroU64>) -> Option<String> {
    limit.map(|rate| rate.get().to_string())
}

/// Renders an optional chmod string for config output.
fn render_chmod(value: Option<&str>) -> Option<String> {
    value.map(str::to_string)
}

/// Generates a temporary fallback config file from inline module definitions.
///
/// Creates temporary files for the rsync config and MOTD, which are passed
/// to the delegated rsync process. Returns `None` if there are no modules
/// or MOTD lines to generate.
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

        if module.fake_super {
            writeln!(config, "    fake super = yes")?;
        }

        writeln!(config)?;
    }

    config.flush()?;

    Ok(Some(GeneratedFallbackConfig {
        config,
        _motd: motd_file,
    }))
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

        for (listener, local_addr) in listeners.into_iter().zip(bound_addresses.iter().copied()) {
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
                    Ok(message) => (*message).to_owned(),
                    Err(_) => "worker thread panicked".to_owned(),
                },
            };
            let error = io::Error::other(description);
            Err(stream_error(None, "serve legacy handshake", error))
        }
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

    // Tests for format_bool

    #[test]
    fn format_bool_returns_yes_for_true() {
        assert_eq!(format_bool(true), "yes");
    }

    #[test]
    fn format_bool_returns_no_for_false() {
        assert_eq!(format_bool(false), "no");
    }

    // Tests for join_list_if_nonempty

    #[test]
    fn join_list_if_nonempty_returns_none_for_empty() {
        let items: Vec<String> = vec![];
        assert!(join_list_if_nonempty(&items, ",").is_none());
    }

    #[test]
    fn join_list_if_nonempty_joins_with_delimiter() {
        let items = vec!["a".to_owned(), "b".to_owned(), "c".to_owned()];
        assert_eq!(join_list_if_nonempty(&items, ","), Some("a,b,c".to_owned()));
    }

    #[test]
    fn join_list_if_nonempty_single_item() {
        let items = vec!["single".to_owned()];
        assert_eq!(join_list_if_nonempty(&items, ","), Some("single".to_owned()));
    }

    // Tests for render_auth_users

    #[test]
    fn render_auth_users_empty_returns_none() {
        assert!(render_auth_users(&[]).is_none());
    }

    #[test]
    fn render_auth_users_comma_separated() {
        let users = vec![
            AuthUser::new("alice".to_owned()),
            AuthUser::new("bob".to_owned()),
        ];
        assert_eq!(render_auth_users(&users), Some("alice,bob".to_owned()));
    }

    #[test]
    fn render_auth_users_with_access_suffixes() {
        let users = vec![
            AuthUser::with_access("alice".to_owned(), UserAccessLevel::ReadWrite),
            AuthUser::with_access("bob".to_owned(), UserAccessLevel::ReadOnly),
            AuthUser::with_access("charlie".to_owned(), UserAccessLevel::Deny),
            AuthUser::new("dave".to_owned()),
        ];
        assert_eq!(
            render_auth_users(&users),
            Some("alice:rw,bob:ro,charlie:deny,dave".to_owned())
        );
    }

    // Tests for render_refused_options

    #[test]
    fn render_refused_options_empty_returns_none() {
        assert!(render_refused_options(&[]).is_none());
    }

    #[test]
    fn render_refused_options_space_separated() {
        let options = vec!["delete".to_owned(), "delete-after".to_owned()];
        assert_eq!(render_refused_options(&options), Some("delete delete-after".to_owned()));
    }

    // Tests for render_optional_u32

    #[test]
    fn render_optional_u32_none_returns_none() {
        assert!(render_optional_u32(None).is_none());
    }

    #[test]
    fn render_optional_u32_returns_string() {
        assert_eq!(render_optional_u32(Some(1000)), Some("1000".to_owned()));
    }

    // Tests for render_optional_timeout

    #[test]
    fn render_optional_timeout_none_returns_none() {
        assert!(render_optional_timeout(None).is_none());
    }

    #[test]
    fn render_optional_timeout_returns_string() {
        let timeout = NonZeroU64::new(300);
        assert_eq!(render_optional_timeout(timeout), Some("300".to_owned()));
    }

    // Tests for render_chmod

    #[test]
    fn render_chmod_none_returns_none() {
        assert!(render_chmod(None).is_none());
    }

    #[test]
    fn render_chmod_returns_string() {
        assert_eq!(render_chmod(Some("Dg+s,ug+w,Fo-w,+X")), Some("Dg+s,ug+w,Fo-w,+X".to_owned()));
    }

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

    // Tests for render_optional_bwlimit

    #[test]
    fn render_optional_bwlimit_none_returns_none() {
        assert!(render_optional_bwlimit(None).is_none());
    }

    #[test]
    fn render_optional_bwlimit_returns_string() {
        let limit = NonZeroU64::new(1_000_000);
        assert_eq!(render_optional_bwlimit(limit), Some("1000000".to_owned()));
    }

    // Tests for host_pattern_token

    #[test]
    fn host_pattern_token_any() {
        assert_eq!(host_pattern_token(&HostPattern::Any), "*");
    }

    #[test]
    fn host_pattern_token_ipv4() {
        let pattern = HostPattern::Ipv4 {
            network: Ipv4Addr::new(192, 168, 1, 0),
            prefix: 24,
        };
        assert_eq!(host_pattern_token(&pattern), "192.168.1.0/24");
    }

    #[test]
    fn host_pattern_token_ipv6() {
        let network: Ipv6Addr = "2001:db8::".parse().unwrap();
        let pattern = HostPattern::Ipv6 {
            network,
            prefix: 32,
        };
        assert_eq!(host_pattern_token(&pattern), "2001:db8::/32");
    }

    #[test]
    fn host_pattern_token_hostname_exact() {
        let pattern = HostPattern::Hostname(HostnamePattern {
            kind: HostnamePatternKind::Exact("example.com".to_owned()),
        });
        assert_eq!(host_pattern_token(&pattern), "example.com");
    }

    #[test]
    fn host_pattern_token_hostname_suffix() {
        let pattern = HostPattern::Hostname(HostnamePattern {
            kind: HostnamePatternKind::Suffix("example.com".to_owned()),
        });
        assert_eq!(host_pattern_token(&pattern), ".example.com");
    }

    #[test]
    fn host_pattern_token_hostname_wildcard() {
        let pattern = HostPattern::Hostname(HostnamePattern {
            kind: HostnamePatternKind::Wildcard("*.example.*".to_owned()),
        });
        assert_eq!(host_pattern_token(&pattern), "*.example.*");
    }

    // Tests for join_pattern_tokens

    #[test]
    fn join_pattern_tokens_empty_returns_none() {
        assert!(join_pattern_tokens(&[]).is_none());
    }

    #[test]
    fn join_pattern_tokens_single() {
        let patterns = vec![HostPattern::Any];
        assert_eq!(join_pattern_tokens(&patterns), Some("*".to_owned()));
    }

    #[test]
    fn join_pattern_tokens_multiple() {
        let patterns = vec![
            HostPattern::Any,
            HostPattern::Ipv4 {
                network: Ipv4Addr::new(10, 0, 0, 0),
                prefix: 8,
            },
        ];
        assert_eq!(join_pattern_tokens(&patterns), Some("* 10.0.0.0/8".to_owned()));
    }

    // Tests for GeneratedFallbackConfig

    #[test]
    fn generated_fallback_config_returns_none_when_empty() {
        let result = generate_fallback_config(&[], &[]).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn generated_fallback_config_creates_motd_file() {
        let motd_lines = vec!["Welcome".to_owned(), "to rsync".to_owned()];
        let result = generate_fallback_config(&[], &motd_lines).unwrap();
        assert!(result.is_some());
        let config = result.unwrap();
        let content = std::fs::read_to_string(config.config_path()).unwrap();
        assert!(content.contains("motd file"));
    }

    #[test]
    fn generated_fallback_config_creates_module_section() {
        let modules = vec![ModuleDefinition {
            name: "test".to_owned(),
            path: PathBuf::from("/tmp/test"),
            comment: Some("Test module".to_owned()),
            read_only: true,
            write_only: false,
            listable: true,
            use_chroot: false,
            numeric_ids: false,
            ..Default::default()
        }];
        let result = generate_fallback_config(&modules, &[]).unwrap();
        assert!(result.is_some());
        let config = result.unwrap();
        let content = std::fs::read_to_string(config.config_path()).unwrap();
        assert!(content.contains("[test]"));
        assert!(content.contains("path = /tmp/test"));
        assert!(content.contains("comment = Test module"));
        assert!(content.contains("read only = yes"));
        assert!(content.contains("write only = no"));
        assert!(content.contains("list = yes"));
    }

    #[test]
    fn generated_fallback_config_includes_auth_users() {
        let modules = vec![ModuleDefinition {
            name: "secure".to_owned(),
            path: PathBuf::from("/secure"),
            auth_users: vec![AuthUser::new("alice".to_owned()), AuthUser::new("bob".to_owned())],
            ..Default::default()
        }];
        let result = generate_fallback_config(&modules, &[]).unwrap();
        let config = result.unwrap();
        let content = std::fs::read_to_string(config.config_path()).unwrap();
        assert!(content.contains("auth users = alice,bob"));
    }

    #[test]
    fn generated_fallback_config_includes_hosts_allow() {
        let modules = vec![ModuleDefinition {
            name: "restricted".to_owned(),
            path: PathBuf::from("/restricted"),
            hosts_allow: vec![HostPattern::Ipv4 {
                network: Ipv4Addr::new(192, 168, 0, 0),
                prefix: 16,
            }],
            ..Default::default()
        }];
        let result = generate_fallback_config(&modules, &[]).unwrap();
        let config = result.unwrap();
        let content = std::fs::read_to_string(config.config_path()).unwrap();
        assert!(content.contains("hosts allow = 192.168.0.0/16"));
    }

    #[test]
    fn generated_fallback_config_includes_bandwidth_limit() {
        let modules = vec![ModuleDefinition {
            name: "limited".to_owned(),
            path: PathBuf::from("/limited"),
            bandwidth_limit: NonZeroU64::new(100000),
            ..Default::default()
        }];
        let result = generate_fallback_config(&modules, &[]).unwrap();
        let config = result.unwrap();
        let content = std::fs::read_to_string(config.config_path()).unwrap();
        assert!(content.contains("bwlimit = 100000"));
    }

    #[test]
    fn generated_fallback_config_includes_max_connections() {
        let modules = vec![ModuleDefinition {
            name: "limited".to_owned(),
            path: PathBuf::from("/limited"),
            max_connections: NonZeroU32::new(10),
            ..Default::default()
        }];
        let result = generate_fallback_config(&modules, &[]).unwrap();
        let config = result.unwrap();
        let content = std::fs::read_to_string(config.config_path()).unwrap();
        assert!(content.contains("max connections = 10"));
    }

    #[test]
    fn generated_fallback_config_includes_chmod_directives() {
        let modules = vec![ModuleDefinition {
            name: "chmod".to_owned(),
            path: PathBuf::from("/chmod"),
            incoming_chmod: Some("Dg+s,ug+w".to_owned()),
            outgoing_chmod: Some("Fo-w,+X".to_owned()),
            ..Default::default()
        }];
        let result = generate_fallback_config(&modules, &[]).unwrap();
        let config = result.unwrap();
        let content = std::fs::read_to_string(config.config_path()).unwrap();
        assert!(content.contains("incoming chmod = Dg+s,ug+w"));
        assert!(content.contains("outgoing chmod = Fo-w,+X"));
    }

    #[test]
    fn generated_fallback_config_includes_uid_gid() {
        let modules = vec![ModuleDefinition {
            name: "ids".to_owned(),
            path: PathBuf::from("/ids"),
            uid: Some(1000),
            gid: Some(1000),
            ..Default::default()
        }];
        let result = generate_fallback_config(&modules, &[]).unwrap();
        let config = result.unwrap();
        let content = std::fs::read_to_string(config.config_path()).unwrap();
        assert!(content.contains("uid = 1000"));
        assert!(content.contains("gid = 1000"));
    }

    #[test]
    fn generated_fallback_config_includes_timeout() {
        let modules = vec![ModuleDefinition {
            name: "timeout".to_owned(),
            path: PathBuf::from("/timeout"),
            timeout: NonZeroU64::new(600),
            ..Default::default()
        }];
        let result = generate_fallback_config(&modules, &[]).unwrap();
        let config = result.unwrap();
        let content = std::fs::read_to_string(config.config_path()).unwrap();
        assert!(content.contains("timeout = 600"));
    }

    #[test]
    fn generated_fallback_config_includes_refuse_options() {
        let modules = vec![ModuleDefinition {
            name: "restricted".to_owned(),
            path: PathBuf::from("/restricted"),
            refuse_options: vec!["delete".to_owned(), "delete-after".to_owned()],
            ..Default::default()
        }];
        let result = generate_fallback_config(&modules, &[]).unwrap();
        let config = result.unwrap();
        let content = std::fs::read_to_string(config.config_path()).unwrap();
        assert!(content.contains("refuse options = delete delete-after"));
    }

    #[test]
    fn generated_fallback_config_includes_fake_super_when_enabled() {
        let modules = vec![ModuleDefinition {
            name: "fakesuper".to_owned(),
            path: PathBuf::from("/fakesuper"),
            fake_super: true,
            ..Default::default()
        }];
        let result = generate_fallback_config(&modules, &[]).unwrap();
        let config = result.unwrap();
        let content = std::fs::read_to_string(config.config_path()).unwrap();
        assert!(content.contains("fake super = yes"));
    }

    #[test]
    fn generated_fallback_config_omits_fake_super_when_disabled() {
        let modules = vec![ModuleDefinition {
            name: "normal".to_owned(),
            path: PathBuf::from("/normal"),
            fake_super: false,
            ..Default::default()
        }];
        let result = generate_fallback_config(&modules, &[]).unwrap();
        let config = result.unwrap();
        let content = std::fs::read_to_string(config.config_path()).unwrap();
        assert!(!content.contains("fake super"));
    }
}

fn configure_stream(stream: &TcpStream) -> io::Result<()> {
    stream.set_read_timeout(Some(SOCKET_TIMEOUT))?;
    stream.set_write_timeout(Some(SOCKET_TIMEOUT))
}

