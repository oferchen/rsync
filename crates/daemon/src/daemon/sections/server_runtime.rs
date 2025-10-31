fn log_sd_notify_failure(log: Option<&SharedLogSink>, context: &str, error: &io::Error) {
    if let Some(sink) = log {
        let payload = format!("failed to notify systemd about {}: {}", context, error);
        let message = rsync_warning!(payload).with_role(Role::Daemon);
        log_message(sink, &message);
    }
}

fn format_connection_status(active: usize) -> String {
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

    let delegation = configured_fallback_binary().and_then(|binary| {
        if inline_modules {
            None
        } else {
            Some(SessionDelegation::new(binary, delegate_arguments))
        }
    });

    let pid_guard = if let Some(path) = pid_file {
        Some(PidFileGuard::create(path)?)
    } else {
        None
    };

    let log_sink = if let Some(path) = log_file {
        Some(open_log_sink(&path)?)
    } else {
        None
    };

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
    let ready_status = format!("Listening on {}", local_addr);
    if let Err(error) = notifier.ready(Some(&ready_status)) {
        log_sd_notify_failure(log_sink.as_ref(), "service readiness", &error);
    }

    if let Some(log) = log_sink.as_ref() {
        let text = format!(
            "rsyncd version {} starting, listening on port {}",
            version,
            local_addr.port()
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
        let text = format!("rsyncd version {} shutting down", version);
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
        writeln!(file, "{}", std::process::id()).map_err(|error| pid_file_error(&path, error))?;
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

