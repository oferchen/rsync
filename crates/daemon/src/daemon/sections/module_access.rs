fn respond_with_module_list(
    stream: &mut TcpStream,
    limiter: &mut Option<BandwidthLimiter>,
    modules: &[ModuleRuntime],
    motd_lines: &[String],
    peer_ip: IpAddr,
    reverse_lookup: bool,
    messages: &LegacyMessageCache,
) -> io::Result<()> {
    for line in motd_lines {
        let payload = if line.is_empty() {
            "MOTD".to_string()
        } else {
            format!("MOTD {line}")
        };
        messages.write(stream, limiter, LegacyDaemonMessage::Other(payload.as_str()))?;
    }

    messages.write_ok(stream, limiter)?;

    let mut hostname_cache: Option<Option<String>> = None;
    for module in modules {
        if !module.listable {
            continue;
        }

        let peer_host = module_peer_hostname(module, &mut hostname_cache, peer_ip, reverse_lookup);
        if !module.permits(peer_ip, peer_host) {
            continue;
        }

        let mut line = module.name.clone();
        if let Some(comment) = &module.comment {
            if !comment.is_empty() {
                line.push('\t');
                line.push_str(comment);
            }
        }
        line.push('\n');
        write_limited(stream, limiter, line.as_bytes())?;
    }

    messages.write_exit(stream, limiter)?;
    stream.flush()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AuthenticationStatus {
    Granted,
    Denied,
}

fn perform_module_authentication(
    reader: &mut BufReader<TcpStream>,
    limiter: &mut Option<BandwidthLimiter>,
    module: &ModuleDefinition,
    peer_ip: IpAddr,
    messages: &LegacyMessageCache,
) -> io::Result<AuthenticationStatus> {
    let challenge = generate_auth_challenge(peer_ip);
    {
        let stream = reader.get_mut();
        messages.write(
            stream,
            limiter,
            LegacyDaemonMessage::AuthRequired {
                module: Some(&challenge),
            },
        )?;
        stream.flush()?;
    }

    let response = match read_trimmed_line(reader)? {
        Some(line) => line,
        None => {
            deny_module(reader.get_mut(), module, peer_ip, limiter, messages)?;
            return Ok(AuthenticationStatus::Denied);
        }
    };

    let mut segments = response.splitn(2, |ch: char| ch.is_ascii_whitespace());
    let username = segments.next().unwrap_or_default();
    let digest = segments
        .next()
        .map(|segment| segment.trim_start_matches(|ch: char| ch.is_ascii_whitespace()))
        .unwrap_or("");

    if username.is_empty() || digest.is_empty() {
        deny_module(reader.get_mut(), module, peer_ip, limiter, messages)?;
        return Ok(AuthenticationStatus::Denied);
    }

    if !module.auth_users.iter().any(|user| user == username) {
        deny_module(reader.get_mut(), module, peer_ip, limiter, messages)?;
        return Ok(AuthenticationStatus::Denied);
    }

    if !verify_secret_response(module, username, &challenge, digest)? {
        deny_module(reader.get_mut(), module, peer_ip, limiter, messages)?;
        return Ok(AuthenticationStatus::Denied);
    }

    Ok(AuthenticationStatus::Granted)
}

fn generate_auth_challenge(peer_ip: IpAddr) -> String {
    let mut input = [0u8; 32];
    let address_text = peer_ip.to_string();
    let address_bytes = address_text.as_bytes();
    let copy_len = address_bytes.len().min(16);
    input[..copy_len].copy_from_slice(&address_bytes[..copy_len]);

    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let seconds = (timestamp.as_secs() & u64::from(u32::MAX)) as u32;
    let micros = timestamp.subsec_micros();
    let pid = std::process::id();

    input[16..20].copy_from_slice(&seconds.to_le_bytes());
    input[20..24].copy_from_slice(&micros.to_le_bytes());
    input[24..28].copy_from_slice(&pid.to_le_bytes());

    let mut hasher = Md5::new();
    hasher.update(&input);
    let digest = hasher.finalize();
    STANDARD_NO_PAD.encode(digest)
}

fn verify_secret_response(
    module: &ModuleDefinition,
    username: &str,
    challenge: &str,
    response: &str,
) -> io::Result<bool> {
    let secrets_path = match &module.secrets_file {
        Some(path) => path,
        None => return Ok(false),
    };

    let contents = fs::read_to_string(secrets_path)?;

    for raw_line in contents.lines() {
        let line = raw_line.trim_end_matches('\r');
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        if let Some((user, secret)) = line.split_once(':') {
            if user == username
                && verify_daemon_auth_response(secret.as_bytes(), challenge, response)
            {
                return Ok(true);
            }
        }
    }

    Ok(false)
}

fn deny_module(
    stream: &mut TcpStream,
    module: &ModuleDefinition,
    peer_ip: IpAddr,
    limiter: &mut Option<BandwidthLimiter>,
    messages: &LegacyMessageCache,
) -> io::Result<()> {
    let module_display = sanitize_module_identifier(&module.name);
    let payload = ACCESS_DENIED_PAYLOAD
        .replace("{module}", module_display.as_ref())
        .replace("{addr}", &peer_ip.to_string());
    write_limited(stream, limiter, payload.as_bytes())?;
    write_limited(stream, limiter, b"\n")?;
    messages.write_exit(stream, limiter)?;
    stream.flush()
}

fn send_daemon_ok(
    stream: &mut TcpStream,
    limiter: &mut Option<BandwidthLimiter>,
    messages: &LegacyMessageCache,
) -> io::Result<()> {
    messages.write_ok(stream, limiter)?;
    stream.flush()
}

/// Reads client arguments sent after module approval.
///
/// After the daemon sends "@RSYNCD: OK", the client sends its command-line
/// arguments (e.g., "--server", "-r", "-a", "."). This mirrors upstream's
/// `read_args()` function in io.c:1292.
///
/// For protocol >= 30: arguments are null-byte terminated
/// For protocol < 30: arguments are newline terminated
/// An empty argument marks the end of the list.
fn read_client_arguments(
    reader: &mut BufReader<TcpStream>,
    protocol: Option<ProtocolVersion>,
) -> io::Result<Vec<String>> {
    let use_nulls = protocol.is_some_and(|p| p.as_u8() >= 30);
    let mut arguments = Vec::new();

    loop {
        if use_nulls {
            // Protocol 30+: read null-terminated arguments
            let mut buf = Vec::new();
            let bytes_read = reader.read_until(b'\0', &mut buf)?;

            if bytes_read == 0 {
                break; // EOF
            }

            // Remove the null terminator
            if buf.last() == Some(&b'\0') {
                buf.pop();
            }

            // Empty argument signals end
            if buf.is_empty() {
                break;
            }

            let arg = String::from_utf8_lossy(&buf).into_owned();
            arguments.push(arg);
        } else {
            // Protocol < 30: read newline-terminated arguments
            let line = match read_trimmed_line(reader)? {
                Some(line) => line,
                None => break, // EOF
            };

            // Empty line signals end
            if line.is_empty() {
                break;
            }

            arguments.push(line);
        }
    }

    Ok(arguments)
}

/// Performs protocol setup exchange after client arguments are received.
///
/// This mirrors upstream's `setup_protocol()` in compat.c:572. The exchange:
/// 1. Writes our protocol version to the client
/// 2. Reads the client's protocol version
/// 3. For protocol >= 30: exchanges compatibility flags
///
/// This must be called AFTER reading client arguments and BEFORE activating
/// multiplexing, to match upstream's daemon flow.
#[allow(dead_code)]
fn perform_protocol_setup<S: Read + Write>(
    stream: &mut S,
    our_protocol: ProtocolVersion,
) -> io::Result<(ProtocolVersion, protocol::CompatibilityFlags)> {
    // Write our protocol version (4-byte little-endian i32)
    let our_version_bytes = (our_protocol.as_u8() as i32).to_le_bytes();
    stream.write_all(&our_version_bytes)?;
    stream.flush()?;

    // Read client's protocol version (4-byte little-endian i32)
    let mut remote_bytes = [0u8; 4];
    stream.read_exact(&mut remote_bytes)?;
    let remote_version_i32 = i32::from_le_bytes(remote_bytes);

    if remote_version_i32 <= 0 || remote_version_i32 > 255 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("invalid remote protocol version: {remote_version_i32}"),
        ));
    }

    let remote_protocol = protocol::ProtocolVersion::try_from(remote_version_i32 as u8)
        .map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unsupported protocol version: {e}"),
            )
        })?;

    // Use the minimum of our version and the remote version
    let negotiated = if remote_protocol.as_u8() < our_protocol.as_u8() {
        remote_protocol
    } else {
        our_protocol
    };

    // For protocol >= 30, exchange compatibility flags
    let compat_flags = if negotiated.as_u8() >= 30 {
        // Server sends flags first
        let our_flags = protocol::CompatibilityFlags::INC_RECURSE
            | protocol::CompatibilityFlags::CHECKSUM_SEED_FIX
            | protocol::CompatibilityFlags::VARINT_FLIST_FLAGS;
        protocol::write_varint(stream, our_flags.bits() as i32)?;
        stream.flush()?;

        // Read client's flags
        let client_flags = protocol::CompatibilityFlags::read_from(stream)?;

        // Use the intersection of both flags (only features both sides support)
        our_flags & client_flags
    } else {
        protocol::CompatibilityFlags::EMPTY
    };

    Ok((negotiated, compat_flags))
}

/// Applies the module-specific bandwidth directives to the active limiter.
///
/// The helper mirrors upstream rsync's precedence rules: a module `bwlimit`
/// directive overrides the daemon-wide limit with the strictest rate while
/// honouring explicitly configured bursts. When a module omits the directive
/// the limiter remains in the state established by the daemon scope, ensuring
/// clients observe inherited throttling exactly as the C implementation does.
/// The function returns the [`LimiterChange`] reported by
/// [`apply_effective_limit`], allowing callers and tests to verify whether the
/// limiter configuration changed as a result of the module overrides.
pub(crate) fn apply_module_bandwidth_limit(
    limiter: &mut Option<BandwidthLimiter>,
    module_limit: Option<NonZeroU64>,
    module_limit_specified: bool,
    module_limit_configured: bool,
    module_burst: Option<NonZeroU64>,
    module_burst_specified: bool,
) -> LimiterChange {
    if module_limit_configured && module_limit.is_none() {
        let burst_only_override =
            module_burst_specified && module_burst.is_some() && limiter.is_some();
        if !burst_only_override {
            return if limiter.take().is_some() {
                LimiterChange::Disabled
            } else {
                LimiterChange::Unchanged
            };
        }
    }

    let limit_specified =
        module_limit_specified || (module_limit_configured && module_limit.is_some());
    let burst_specified =
        module_burst_specified && (module_limit_configured || module_limit_specified);

    BandwidthLimitComponents::new_with_flags(
        module_limit,
        module_burst,
        limit_specified,
        burst_specified,
    )
    .apply_to_limiter(limiter)
}

#[allow(clippy::too_many_arguments)]
fn respond_with_module_request(
    reader: &mut BufReader<TcpStream>,
    limiter: &mut Option<BandwidthLimiter>,
    modules: &[ModuleRuntime],
    request: &str,
    peer_ip: IpAddr,
    session_peer_host: Option<&str>,
    options: &[String],
    log_sink: Option<&SharedLogSink>,
    reverse_lookup: bool,
    messages: &LegacyMessageCache,
    negotiated_protocol: Option<ProtocolVersion>,
) -> io::Result<()> {
    if let Some(module) = modules.iter().find(|module| module.name == request) {
        let change = apply_module_bandwidth_limit(
            limiter,
            module.bandwidth_limit(),
            module.bandwidth_limit_specified(),
            module.bandwidth_limit_configured(),
            module.bandwidth_burst(),
            module.bandwidth_burst_specified(),
        );

        let mut hostname_cache: Option<Option<String>> = None;
        let module_peer_host =
            module_peer_hostname(module, &mut hostname_cache, peer_ip, reverse_lookup);

        if change != LimiterChange::Unchanged {
            if let Some(log) = log_sink {
                log_module_bandwidth_change(
                    log,
                    module_peer_host.or(session_peer_host),
                    peer_ip,
                    request,
                    limiter.as_ref(),
                    change,
                );
            }
        }
        if module.permits(peer_ip, module_peer_host) {
            let _connection_guard = match module.try_acquire_connection() {
                Ok(guard) => guard,
                Err(ModuleConnectionError::Limit(limit)) => {
                    let payload =
                        MODULE_MAX_CONNECTIONS_PAYLOAD.replace("{limit}", &limit.get().to_string());
                    let stream = reader.get_mut();
                    write_limited(stream, limiter, payload.as_bytes())?;
                    write_limited(stream, limiter, b"\n")?;
                    messages.write_exit(stream, limiter)?;
                    stream.flush()?;
                    if let Some(log) = log_sink {
                        log_module_limit(
                            log,
                            module_peer_host.or(session_peer_host),
                            peer_ip,
                            request,
                            limit,
                        );
                    }
                    return Ok(());
                }
                Err(ModuleConnectionError::Io(error)) => {
                    let stream = reader.get_mut();
                    write_limited(stream, limiter, MODULE_LOCK_ERROR_PAYLOAD.as_bytes())?;
                    write_limited(stream, limiter, b"\n")?;
                    messages.write_exit(stream, limiter)?;
                    stream.flush()?;
                    if let Some(log) = log_sink {
                        log_module_lock_error(
                            log,
                            module_peer_host.or(session_peer_host),
                            peer_ip,
                            request,
                            &error,
                        );
                    }
                    return Ok(());
                }
            };

            if let Some(log) = log_sink {
                log_module_request(
                    log,
                    module_peer_host.or(session_peer_host),
                    peer_ip,
                    request,
                );
            }

            if let Some(refused) = refused_option(module, options) {
                let payload = format!("@ERROR: The server is configured to refuse {refused}");
                let stream = reader.get_mut();
                write_limited(stream, limiter, payload.as_bytes())?;
                write_limited(stream, limiter, b"\n")?;
                messages.write_exit(stream, limiter)?;
                stream.flush()?;
                if let Some(log) = log_sink {
                    log_module_refused_option(
                        log,
                        module_peer_host.or(session_peer_host),
                        peer_ip,
                        request,
                        refused,
                    );
                }
                return Ok(());
            }

            apply_module_timeout(reader.get_mut(), module)?;
            let mut acknowledged = false;
            if module.requires_authentication() {
                match perform_module_authentication(reader, limiter, module, peer_ip, messages)? {
                    AuthenticationStatus::Denied => {
                        if let Some(log) = log_sink {
                            log_module_auth_failure(
                                log,
                                module_peer_host.or(session_peer_host),
                                peer_ip,
                                request,
                            );
                        }
                        return Ok(());
                    }
                    AuthenticationStatus::Granted => {
                        if let Some(log) = log_sink {
                            log_module_auth_success(
                                log,
                                module_peer_host.or(session_peer_host),
                                peer_ip,
                                request,
                            );
                        }
                        send_daemon_ok(reader.get_mut(), limiter, messages)?;
                        acknowledged = true;
                    }
                }
            }

            if !acknowledged {
                send_daemon_ok(reader.get_mut(), limiter, messages)?;
            }

            // Read client arguments sent after "@RSYNCD: OK"
            // This mirrors upstream's read_args() in io.c:1292
            let client_args = match read_client_arguments(reader, negotiated_protocol) {
                Ok(args) => args,
                Err(err) => {
                    let payload = format!("@ERROR: failed to read client arguments: {err}");
                    let stream = reader.get_mut();
                    write_limited(stream, limiter, payload.as_bytes())?;
                    write_limited(stream, limiter, b"\n")?;
                    messages.write_exit(stream, limiter)?;
                    stream.flush()?;
                    return Ok(());
                }
            };

            // Log received arguments for debugging
            if let Some(log) = log_sink {
                let args_str = client_args.join(" ");
                let text = format!(
                    "module '{}' from {} ({}): client args: {}",
                    request,
                    module_peer_host.or(session_peer_host).unwrap_or("unknown"),
                    peer_ip,
                    args_str
                );
                let message = rsync_info!(text).with_role(Role::Daemon);
                log_message(log, &message);
            }

            // DAEMON WIRING: Wire core::server for daemon file transfers
            // Determine role based on client arguments (mirrors upstream daemon.c)
            // The --sender flag indicates that the SERVER should act as sender (Generator)
            // When absent, the SERVER should act as receiver (Receiver)
            // This is counterintuitive: --sender means "I (the server) am the sender"
            let server_is_sender = client_args.iter().any(|arg| arg == "--sender");
            let role = if server_is_sender {
                // Server is sending to client (client is receiving from us)
                ServerRole::Generator
            } else {
                // Server is receiving from client (client is sending to us)
                ServerRole::Receiver
            };

            // Build ServerConfig with module path as the target directory
            // Parse client arguments to extract flags and additional paths
            let config = match ServerConfig::from_flag_string_and_args(
                role,
                client_args.join(" "), // Use client-supplied arguments
                vec![OsString::from(&module.path)],
            ) {
                Ok(cfg) => cfg,
                Err(err) => {
                    let payload = format!("@ERROR: failed to configure server: {err}");
                    let stream = reader.get_mut();
                    write_limited(stream, limiter, payload.as_bytes())?;
                    write_limited(stream, limiter, b"\n")?;
                    messages.write_exit(stream, limiter)?;
                    stream.flush()?;
                    return Ok(());
                }
            };

            // Validate that the module path exists and is accessible
            if !Path::new(&module.path).exists() {
                let payload = format!(
                    "@ERROR: module '{}' path does not exist: {}",
                    sanitize_module_identifier(request),
                    module.path.display()
                );
                let stream = reader.get_mut();
                write_limited(stream, limiter, payload.as_bytes())?;
                write_limited(stream, limiter, b"\n")?;
                messages.write_exit(stream, limiter)?;
                stream.flush()?;
                if let Some(log) = log_sink {
                    let text = format!(
                        "module '{}' path validation failed for {} ({}): path does not exist: {}",
                        request,
                        module_peer_host.or(session_peer_host).unwrap_or("unknown"),
                        peer_ip,
                        module.path.display()
                    );
                    let message = rsync_error!(1, text).with_role(Role::Daemon);
                    log_message(log, &message);
                }
                return Ok(());
            }

            // For daemon success path, we do NOT perform protocol setup here!
            // Upstream does setup_protocol INSIDE start_server() (main.c:1245)
            // only calling it early for error reporting (clientserver.c:1136)
            // We pass the negotiated protocol to run_server_with_handshake,
            // which will perform setup_protocol internally
            let final_protocol = negotiated_protocol.unwrap_or(ProtocolVersion::V30);

            // Extract any buffered data from the BufReader before proceeding
            // The BufReader may have read ahead during the negotiation phase
            let buffered_data = reader.buffer().to_vec();

            // Get mutable reference to stream to set TCP_NODELAY and exchange compat flags
            let stream = reader.get_mut();

            // CRITICAL: Set TCP_NODELAY to disable Nagle's algorithm
            // This prevents kernel buffering from reordering small writes
            stream.set_nodelay(true)?;

            // NOTE: Compat flags exchange moved to setup_protocol() to ensure correct timing
            // relative to OUTPUT multiplex activation. The compat flags must be sent BEFORE
            // multiplex is activated, but AFTER the stream is set up properly in run_server_with_handshake.

            // Log the negotiated protocol
            if let Some(log) = log_sink {
                let text = format!(
                    "module '{}' from {} ({}): protocol {}, role: {:?}",
                    request,
                    module_peer_host.or(session_peer_host).unwrap_or("unknown"),
                    peer_ip,
                    final_protocol.as_u8(),
                    role
                );
                let message = rsync_info!(text).with_role(Role::Daemon);
                log_message(log, &message);
            }

            // Now clone the stream for concurrent read/write
            let read_stream = match stream.try_clone() {
                Ok(s) => s,
                Err(err) => {
                    let payload = format!("@ERROR: failed to clone stream: {err}");
                    let stream_mut = reader.get_mut();
                    write_limited(stream_mut, limiter, payload.as_bytes())?;
                    write_limited(stream_mut, limiter, b"\n")?;
                    messages.write_exit(stream_mut, limiter)?;
                    stream_mut.flush()?;
                    return Ok(());
                }
            };

            // Clone write stream - this creates another handle to the SAME socket
            // Upstream rsync uses dup() which is equivalent to try_clone()
            // The key is: both handles point to the same kernel socket, so no buffering issues
            let write_stream = match stream.try_clone() {
                Ok(s) => s,
                Err(err) => {
                    return Err(io::Error::other(
                        format!("failed to clone write stream: {err}"),
                    ));
                }
            };

            // Create HandshakeResult from the negotiated protocol version
            // Let setup_protocol() handle compat exchange with client capabilities
            let handshake = HandshakeResult {
                protocol: final_protocol,
                buffered: buffered_data,
                compat_exchanged: false,  // Let setup_protocol parse client_args and send compat flags
                client_args: Some(client_args.clone()),  // Pass client args for capability parsing
                io_timeout: module.timeout.map(|t| t.get()),  // Pass configured I/O timeout
            };

            // Enable protocol tracing for debugging
            // Trace files will be written to /tmp/rsync-trace/ directory
            use protocol::debug_trace::{TraceConfig, TracingReader, TracingWriter};

            let trace_config = TraceConfig::enabled("daemon");
            let mut traced_read_stream = TracingReader::new(read_stream, trace_config.clone());
            let mut traced_write_stream = TracingWriter::new(write_stream, trace_config);

            // Run the server transfer - handles protocol setup and multiplex internally
            let result = run_server_with_handshake(config, handshake, &mut traced_read_stream, &mut traced_write_stream);
            match result {
                Ok(_server_stats) => {
                    if let Some(log) = log_sink {
                        let text = format!(
                            "transfer to {} ({}): module={} status=success",
                            module_peer_host.or(session_peer_host).unwrap_or("unknown"),
                            peer_ip,
                            request
                        );
                        let message = rsync_info!(text).with_role(Role::Daemon);
                        log_message(log, &message);
                    }
                }
                Err(err) => {
                    if let Some(log) = log_sink {
                        let text = format!(
                            "transfer failed to {} ({}): module={} error={}",
                            module_peer_host.or(session_peer_host).unwrap_or("unknown"),
                            peer_ip,
                            request,
                            err
                        );
                        let message = rsync_error!(1, text).with_role(Role::Daemon);
                        log_message(log, &message);
                    }
                }
            }

            // Note: Connection guard (_connection_guard) drops here, releasing the slot
            return Ok(());
        } else {
            if let Some(log) = log_sink {
                log_module_denied(
                    log,
                    module_peer_host.or(session_peer_host),
                    peer_ip,
                    request,
                );
            }
            deny_module(reader.get_mut(), module, peer_ip, limiter, messages)?;
            return Ok(());
        }
    } else {
        let module_display = sanitize_module_identifier(request);
        let payload = UNKNOWN_MODULE_PAYLOAD.replace("{module}", module_display.as_ref());
        let stream = reader.get_mut();
        write_limited(stream, limiter, payload.as_bytes())?;
        write_limited(stream, limiter, b"\n")?;
        if let Some(log) = log_sink {
            log_unknown_module(log, session_peer_host, peer_ip, request);
        }
    }

    let stream = reader.get_mut();
    messages.write_exit(stream, limiter)?;
    stream.flush()
}

pub(crate) fn open_log_sink(path: &Path, brand: Brand) -> Result<SharedLogSink, DaemonError> {
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|error| log_file_error(path, error))?;
    Ok(Arc::new(Mutex::new(MessageSink::with_brand(file, brand))))
}

fn log_file_error(path: &Path, error: io::Error) -> DaemonError {
    DaemonError::new(
        FEATURE_UNAVAILABLE_EXIT_CODE,
        rsync_error!(
            FEATURE_UNAVAILABLE_EXIT_CODE,
            format!("failed to open log file '{}': {}", path.display(), error)
        )
        .with_role(Role::Daemon),
    )
}

fn pid_file_error(path: &Path, error: io::Error) -> DaemonError {
    DaemonError::new(
        FEATURE_UNAVAILABLE_EXIT_CODE,
        rsync_error!(
            FEATURE_UNAVAILABLE_EXIT_CODE,
            format!("failed to write pid file '{}': {}", path.display(), error)
        )
        .with_role(Role::Daemon),
    )
}

fn lock_file_error(path: &Path, error: io::Error) -> DaemonError {
    DaemonError::new(
        FEATURE_UNAVAILABLE_EXIT_CODE,
        rsync_error!(
            FEATURE_UNAVAILABLE_EXIT_CODE,
            format!("failed to open lock file '{}': {}", path.display(), error)
        )
        .with_role(Role::Daemon),
    )
}

fn log_message(log: &SharedLogSink, message: &Message) {
    if let Ok(mut sink) = log.lock() {
        if sink.write(message).is_ok() {
            let _ = sink.flush();
        }
    }
}

fn format_host(host: Option<&str>, fallback: IpAddr) -> String {
    host.map(str::to_string)
        .unwrap_or_else(|| fallback.to_string())
}

/// Returns a sanitised view of a module identifier suitable for diagnostics.
///
/// Module names originate from user input (daemon operands) or configuration
/// files. When composing diagnostics the value must not embed control
/// characters, otherwise adversarial requests could smuggle terminal control
/// sequences or split log lines. The helper replaces ASCII control characters
/// with a visible `'?'` marker while borrowing clean identifiers to avoid
/// unnecessary allocations.
pub(crate) fn sanitize_module_identifier(input: &str) -> Cow<'_, str> {
    if input.chars().all(|ch| !ch.is_control()) {
        return Cow::Borrowed(input);
    }

    let mut sanitized = String::with_capacity(input.len());
    for ch in input.chars() {
        if ch.is_control() {
            sanitized.push('?');
        } else {
            sanitized.push(ch);
        }
    }

    Cow::Owned(sanitized)
}

pub(crate) fn format_bandwidth_rate(value: NonZeroU64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = KIB * 1024;
    const GIB: u64 = MIB * 1024;
    const TIB: u64 = GIB * 1024;
    const PIB: u64 = TIB * 1024;

    let bytes = value.get();
    if bytes.is_multiple_of(PIB) {
        let rate = bytes / PIB;
        format!("{rate} PiB/s")
    } else if bytes.is_multiple_of(TIB) {
        let rate = bytes / TIB;
        format!("{rate} TiB/s")
    } else if bytes.is_multiple_of(GIB) {
        let rate = bytes / GIB;
        format!("{rate} GiB/s")
    } else if bytes.is_multiple_of(MIB) {
        let rate = bytes / MIB;
        format!("{rate} MiB/s")
    } else if bytes.is_multiple_of(KIB) {
        let rate = bytes / KIB;
        format!("{rate} KiB/s")
    } else {
        format!("{bytes} bytes/s")
    }
}

