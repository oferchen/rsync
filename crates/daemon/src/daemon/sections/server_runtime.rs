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

        if let Some(comment) = &module.comment {
            if !comment.is_empty() {
                writeln!(config, "    comment = {comment}")?;
            }
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

fn serve_connections(options: RuntimeOptions) -> Result<(), DaemonError> {
    let manifest = manifest();
    let version = manifest.rust_version();
    let RuntimeOptions {
        brand,
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
        ..
    } = options;

    let mut fallback_warning_message: Option<Message> = None;
    let mut delegate_arguments = delegate_arguments;
    let mut generated_config: Option<GeneratedFallbackConfig> = None;

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

    // Daemon mode never delegates - always use internal Rust implementation
    let delegation = if let Some(binary) = configured_fallback_binary_for_daemon() {
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
    };

    let _generated_config_guard = generated_config;

    let pid_guard = if let Some(path) = pid_file {
        Some(PidFileGuard::create(path)?)
    } else {
        None
    };

    if let Some(message) = fallback_warning_message.as_ref() {
        eprintln!("{}", message.clone().with_brand(brand));
    }

    let log_sink = if let Some(path) = log_file {
        Some(open_log_sink(&path, brand)?)
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
    let requested_addr = SocketAddr::new(bind_address, port);
    let listener =
        TcpListener::bind(requested_addr).map_err(|error| bind_error(requested_addr, error))?;
    let local_addr = listener.local_addr().unwrap_or(requested_addr);

    let notifier = systemd::ServiceNotifier::new();
    let ready_status = format!("Listening on {local_addr}");
    if let Err(error) = notifier.ready(Some(&ready_status)) {
        log_sd_notify_failure(log_sink.as_ref(), "service readiness", &error);
    }

    if let Some(log) = log_sink.as_ref() {
        let port = local_addr.port();
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
            Ok((stream, peer_addr)) => {
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

        if let Some(limit) = max_sessions {
            if served >= limit {
                if let Err(error) = notifier.status("Draining worker threads") {
                    log_sd_notify_failure(log_sink.as_ref(), "connection status update", &error);
                }
                break;
            }
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
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent).map_err(|error| pid_file_error(&path, error))?;
            }
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

