/// Writes an error payload and exit sequence to the stream.
///
/// This helper consolidates the common pattern of sending an error message
/// followed by the daemon exit marker and flushing the stream.
#[allow(dead_code)]
fn send_error_response(
    stream: &mut TcpStream,
    limiter: &mut Option<BandwidthLimiter>,
    messages: &LegacyMessageCache,
    payload: &str,
) -> io::Result<()> {
    write_limited(stream, limiter, payload.as_bytes())?;
    write_limited(stream, limiter, b"\n")?;
    messages.write_exit(stream, limiter)?;
    stream.flush()
}

/// Context for module request handling containing common parameters.
#[allow(dead_code)]
struct ModuleRequestContext<'a> {
    limiter: &'a mut Option<BandwidthLimiter>,
    peer_ip: IpAddr,
    session_peer_host: Option<&'a str>,
    log_sink: Option<&'a SharedLogSink>,
    messages: &'a LegacyMessageCache,
}

/// Handles the case when a module is not found.
#[allow(dead_code)]
fn handle_unknown_module(
    stream: &mut TcpStream,
    ctx: &mut ModuleRequestContext<'_>,
    request: &str,
) -> io::Result<()> {
    let module_display = sanitize_module_identifier(request);
    let payload = UNKNOWN_MODULE_PAYLOAD.replace("{module}", module_display.as_ref());
    write_limited(stream, ctx.limiter, payload.as_bytes())?;
    write_limited(stream, ctx.limiter, b"\n")?;
    if let Some(log) = ctx.log_sink {
        log_unknown_module(log, ctx.session_peer_host, ctx.peer_ip, request);
    }
    ctx.messages.write_exit(stream, ctx.limiter)?;
    stream.flush()
}

/// Handles access denied when module doesn't permit the peer.
#[allow(dead_code)]
fn handle_access_denied(
    stream: &mut TcpStream,
    ctx: &mut ModuleRequestContext<'_>,
    module: &ModuleDefinition,
    module_peer_host: Option<&str>,
    request: &str,
) -> io::Result<()> {
    if let Some(log) = ctx.log_sink {
        log_module_denied(
            log,
            module_peer_host.or(ctx.session_peer_host),
            ctx.peer_ip,
            request,
        );
    }
    deny_module(stream, module, ctx.peer_ip, ctx.limiter, ctx.messages)
}

/// Result of attempting to acquire a module connection.
#[allow(dead_code)]
enum ConnectionAcquisitionResult<'a> {
    /// Successfully acquired the connection guard.
    Acquired(ModuleConnectionGuard<'a>),
    /// Connection limit reached, error response was sent.
    LimitReached,
    /// I/O error on lock file, error response was sent.
    IoError,
}

/// Attempts to acquire a module connection, sending error responses on failure.
#[allow(dead_code)]
fn try_acquire_module_connection<'a>(
    stream: &mut TcpStream,
    ctx: &mut ModuleRequestContext<'_>,
    module: &'a ModuleRuntime,
    module_peer_host: Option<&str>,
    request: &str,
) -> io::Result<ConnectionAcquisitionResult<'a>> {
    match module.try_acquire_connection() {
        Ok(guard) => Ok(ConnectionAcquisitionResult::Acquired(guard)),
        Err(ModuleConnectionError::Limit(limit)) => {
            let payload =
                MODULE_MAX_CONNECTIONS_PAYLOAD.replace("{limit}", &limit.get().to_string());
            send_error_response(stream, ctx.limiter, ctx.messages, &payload)?;
            if let Some(log) = ctx.log_sink {
                log_module_limit(
                    log,
                    module_peer_host.or(ctx.session_peer_host),
                    ctx.peer_ip,
                    request,
                    limit,
                );
            }
            Ok(ConnectionAcquisitionResult::LimitReached)
        }
        Err(ModuleConnectionError::Io(error)) => {
            send_error_response(stream, ctx.limiter, ctx.messages, MODULE_LOCK_ERROR_PAYLOAD)?;
            if let Some(log) = ctx.log_sink {
                log_module_lock_error(
                    log,
                    module_peer_host.or(ctx.session_peer_host),
                    ctx.peer_ip,
                    request,
                    &error,
                );
            }
            Ok(ConnectionAcquisitionResult::IoError)
        }
    }
}

/// Checks for refused options and sends error response if found.
///
/// Returns `Ok(Some(refused))` if a refused option was found and error was sent,
/// `Ok(None)` if no refused options were found, or an I/O error.
#[allow(dead_code)]
fn check_refused_options(
    stream: &mut TcpStream,
    ctx: &mut ModuleRequestContext<'_>,
    module: &ModuleRuntime,
    options: &[String],
    module_peer_host: Option<&str>,
    request: &str,
) -> io::Result<Option<()>> {
    if let Some(refused) = refused_option(module, options) {
        let payload = format!("@ERROR: The server is configured to refuse {refused}");
        send_error_response(stream, ctx.limiter, ctx.messages, &payload)?;
        if let Some(log) = ctx.log_sink {
            log_module_refused_option(
                log,
                module_peer_host.or(ctx.session_peer_host),
                ctx.peer_ip,
                request,
                refused,
            );
        }
        Ok(Some(()))
    } else {
        Ok(None)
    }
}

/// Handles module authentication if required.
///
/// Returns `Ok(true)` if authentication succeeded (or wasn't required),
/// `Ok(false)` if authentication was denied, or an I/O error.
#[allow(dead_code)]
fn handle_module_authentication(
    reader: &mut BufReader<TcpStream>,
    ctx: &mut ModuleRequestContext<'_>,
    module: &ModuleRuntime,
    module_peer_host: Option<&str>,
    request: &str,
) -> io::Result<bool> {
    apply_module_timeout(reader.get_mut(), module)?;

    if !module.requires_authentication() {
        send_daemon_ok(reader.get_mut(), ctx.limiter, ctx.messages)?;
        return Ok(true);
    }

    match perform_module_authentication(reader, ctx.limiter, module, ctx.peer_ip, ctx.messages)? {
        AuthenticationStatus::Denied => {
            if let Some(log) = ctx.log_sink {
                log_module_auth_failure(
                    log,
                    module_peer_host.or(ctx.session_peer_host),
                    ctx.peer_ip,
                    request,
                );
            }
            Ok(false)
        }
        AuthenticationStatus::Granted => {
            if let Some(log) = ctx.log_sink {
                log_module_auth_success(
                    log,
                    module_peer_host.or(ctx.session_peer_host),
                    ctx.peer_ip,
                    request,
                );
            }
            send_daemon_ok(reader.get_mut(), ctx.limiter, ctx.messages)?;
            Ok(true)
        }
    }
}

/// Determines the server role from client arguments.
///
/// The `--sender` flag indicates the server should act as sender (Generator).
/// When absent, the server acts as receiver (Receiver).
#[allow(dead_code)]
fn determine_server_role(client_args: &[String]) -> ServerRole {
    if client_args.iter().any(|arg| arg == "--sender") {
        ServerRole::Generator
    } else {
        ServerRole::Receiver
    }
}

/// Extracts the short flags argument from client arguments.
///
/// Client args are like: `["--server", "--sender", "-vvre.iLsfxCIvu", ".", "testmod/"]`
#[allow(dead_code)]
fn extract_flag_string(client_args: &[String]) -> String {
    client_args
        .iter()
        .find(|arg| arg.starts_with('-') && !arg.starts_with("--"))
        .cloned()
        .unwrap_or_default()
}

/// Builds server configuration from client arguments.
#[allow(dead_code)]
fn build_server_config(
    client_args: &[String],
    module_path: &Path,
) -> Result<ServerConfig, String> {
    let role = determine_server_role(client_args);
    let flag_string = extract_flag_string(client_args);
    ServerConfig::from_flag_string_and_args(
        role,
        flag_string,
        vec![OsString::from(module_path)],
    )
}

/// Validates that the module path exists.
#[allow(dead_code)]
fn validate_module_path(module_path: &Path) -> bool {
    module_path.exists()
}

/// Executes the file transfer and logs the result.
#[allow(dead_code, clippy::too_many_arguments)]
fn execute_transfer(
    config: ServerConfig,
    handshake: HandshakeResult,
    read_stream: &mut TcpStream,
    write_stream: &mut TcpStream,
    log_sink: Option<&SharedLogSink>,
    peer_host: Option<&str>,
    peer_ip: IpAddr,
    request: &str,
) {
    let result = run_server_with_handshake(config, handshake, read_stream, write_stream);
    match result {
        Ok(_server_stats) => {
            if let Some(log) = log_sink {
                let text = format!(
                    "transfer to {} ({}): module={} status=success",
                    peer_host.unwrap_or("unknown"),
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
                    peer_host.unwrap_or("unknown"),
                    peer_ip,
                    request,
                    err
                );
                let message = rsync_error!(1, text).with_role(Role::Daemon);
                log_message(log, &message);
            }
        }
    }
}

/// Sends the list of available modules to a client.
///
/// This responds to a module listing request by sending the MOTD (message of the
/// day) lines followed by the names and comments of modules the peer is allowed
/// to access. Only modules marked as listable and that permit the peer's IP
/// address are included in the response.
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
            "MOTD".to_owned()
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
        if let Some(comment) = &module.comment
            && !comment.is_empty() {
                line.push('\t');
                line.push_str(comment);
            }
        line.push('\n');
        write_limited(stream, limiter, line.as_bytes())?;
    }

    messages.write_exit(stream, limiter)?;
    stream.flush()
}

/// Result of a module authentication attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AuthenticationStatus {
    /// Authentication was successful.
    Granted,
    /// Authentication was denied (bad credentials or missing response).
    Denied,
}

/// Performs challenge-response authentication for a protected module.
///
/// This implements the rsync daemon authentication protocol:
/// 1. Sends a base64-encoded MD5 challenge to the client
/// 2. Reads the client's response containing username and digest
/// 3. Verifies the digest against the module's secrets file
///
/// Returns `Granted` if authentication succeeded, `Denied` otherwise.
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
        .map_or("", |segment| segment.trim_start_matches(|ch: char| ch.is_ascii_whitespace()));

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

/// Generates a unique authentication challenge string.
///
/// The challenge is created by combining the peer IP address, current timestamp,
/// and process ID, then hashing with MD5 and encoding as base64. This produces
/// a unique, time-sensitive challenge for each authentication attempt.
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

/// Verifies a client's authentication response against the secrets file.
///
/// Reads the module's secrets file line by line, looking for a matching
/// username entry. For matching usernames, computes the expected digest
/// using the stored secret and challenge, then compares with the client's
/// response.
///
/// Returns `true` if the username exists and the digest matches, `false` otherwise.
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

        if let Some((user, secret)) = line.split_once(':')
            && user == username
                && verify_daemon_auth_response(secret.as_bytes(), challenge, response)
            {
                return Ok(true);
            }
    }

    Ok(false)
}

/// Sends an access denied response to the client and closes the session.
///
/// This writes the "@ERROR: access denied" message with the module name
/// and peer address, then sends the daemon exit marker.
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

/// Sends the "@RSYNCD: OK" acknowledgment to the client.
///
/// This confirms that the module request was accepted and the client
/// may proceed with sending its arguments.
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
fn read_client_arguments<R: BufRead>(
    reader: &mut R,
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

/// Handles a client's module access request.
///
/// This is the main entry point for processing a module request. It performs:
/// 1. Module lookup and access permission verification
/// 2. Bandwidth limit application from module configuration
/// 3. Connection acquisition with max-connections enforcement
/// 4. Refused options checking
/// 5. Authentication (if the module requires it)
/// 6. Protocol setup and transfer execution
///
/// Returns an I/O error if the connection fails, otherwise `Ok(())`.
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

        if change != LimiterChange::Unchanged
            && let Some(log) = log_sink {
                log_module_bandwidth_change(
                    log,
                    module_peer_host.or(session_peer_host),
                    peer_ip,
                    request,
                    limiter.as_ref(),
                    change,
                );
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
            // Extract the short flags argument (starts with '-' but not '--')
            // Client args are like: ["--server", "--sender", "-vvre.iLsfxCIvu", ".", "testmod/"]
            let flag_string = client_args
                .iter()
                .find(|arg| arg.starts_with('-') && !arg.starts_with("--"))
                .cloned()
                .unwrap_or_default();
            let config = match ServerConfig::from_flag_string_and_args(
                role,
                flag_string,
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
            let mut read_stream = match stream.try_clone() {
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
            let mut write_stream = match stream.try_clone() {
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
                client_args: Some(client_args),  // Pass client args for capability parsing
                io_timeout: module.timeout.map(|t| t.get()),  // Pass configured I/O timeout
                negotiated_algorithms: None,  // Will be populated by setup_protocol()
                compat_flags: None,  // Will be populated by setup_protocol()
                checksum_seed: 0,  // Will be populated by setup_protocol()
            };

            // Run the server transfer - handles protocol setup and multiplex internally
            let result = run_server_with_handshake(config, handshake, &mut read_stream, &mut write_stream);
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

/// Opens or creates a log file and wraps it in a shared message sink.
///
/// The log file is opened in append mode, creating it if it doesn't exist.
/// Returns a thread-safe [`SharedLogSink`] for concurrent logging.
pub(crate) fn open_log_sink(path: &Path, brand: Brand) -> Result<SharedLogSink, DaemonError> {
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|error| log_file_error(path, error))?;
    Ok(Arc::new(Mutex::new(MessageSink::with_brand(file, brand))))
}

/// Creates a [`DaemonError`] for log file open failures.
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

/// Creates a [`DaemonError`] for PID file write failures.
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

/// Creates a [`DaemonError`] for lock file open failures.
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

/// Writes a message to the shared log sink with proper locking.
fn log_message(log: &SharedLogSink, message: &Message) {
    if let Ok(mut sink) = log.lock()
        && sink.write(message).is_ok() {
            let _ = sink.flush();
        }
}

/// Formats a host for logging, using the IP address as fallback.
fn format_host(host: Option<&str>, fallback: IpAddr) -> String {
    host.map_or_else(|| fallback.to_string(), str::to_string)
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

/// Formats a bandwidth rate in human-readable units (bytes/s, KiB/s, etc.).
///
/// Chooses the largest unit that divides evenly into the rate, falling back
/// to raw bytes/s for values that don't align to a power-of-1024 boundary.
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

#[cfg(test)]
mod module_access_tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn generate_auth_challenge_includes_ip_and_timestamp() {
        let peer_ip = "192.168.1.1".parse::<IpAddr>().unwrap();
        let challenge = generate_auth_challenge(peer_ip);

        // Challenge should be base64-encoded MD5 hash (22 characters without padding)
        assert_eq!(challenge.len(), 22);
        assert!(challenge.chars().all(|c| c.is_alphanumeric() || c == '+' || c == '/'));
    }

    #[test]
    fn generate_auth_challenge_produces_different_values() {
        let peer_ip = "10.0.0.1".parse::<IpAddr>().unwrap();
        let challenge1 = generate_auth_challenge(peer_ip);

        // Small delay to ensure different timestamp
        std::thread::sleep(std::time::Duration::from_millis(10));
        let challenge2 = generate_auth_challenge(peer_ip);

        // Challenges should differ due to timestamp
        assert_ne!(challenge1, challenge2);
    }

    #[test]
    fn sanitize_module_identifier_preserves_clean_input() {
        let clean = "my_module-123";
        let result = sanitize_module_identifier(clean);
        assert_eq!(result, clean);
        assert!(matches!(result, Cow::Borrowed(_)));
    }

    #[test]
    fn sanitize_module_identifier_replaces_control_characters() {
        let dirty = "module\nwith\tcontrols\r";
        let result = sanitize_module_identifier(dirty);
        assert_eq!(result, "module?with?controls?");
        assert!(matches!(result, Cow::Owned(_)));
    }

    #[test]
    fn sanitize_module_identifier_handles_mixed_content() {
        let mixed = "mod\x00ule_\x1bname";
        let result = sanitize_module_identifier(mixed);
        assert_eq!(result, "mod?ule_?name");
    }

    #[test]
    fn read_client_arguments_protocol_30_null_terminated() {
        let input = b"--server\0--sender\0-r\0.\0\0";
        let mut reader = BufReader::new(Cursor::new(input));

        let args = read_client_arguments(&mut reader, Some(ProtocolVersion::V30))
            .expect("should read arguments");

        assert_eq!(args, vec!["--server", "--sender", "-r", "."]);
    }

    #[test]
    fn read_client_arguments_protocol_30_stops_at_empty() {
        let input = b"--server\0\0more\0data\0";
        let mut reader = BufReader::new(Cursor::new(input));

        let args = read_client_arguments(&mut reader, Some(ProtocolVersion::V30))
            .expect("should read arguments");

        assert_eq!(args, vec!["--server"]);
    }

    #[test]
    fn read_client_arguments_protocol_29_newline_terminated() {
        let input = b"--server\n--sender\n-r\n.\n\n";
        let mut reader = BufReader::new(Cursor::new(input));

        let args = read_client_arguments(&mut reader, Some(ProtocolVersion::V29))
            .expect("should read arguments");

        assert_eq!(args, vec!["--server", "--sender", "-r", "."]);
    }

    #[test]
    fn read_client_arguments_protocol_29_stops_at_empty_line() {
        let input = b"--server\n\nmore\n";
        let mut reader = BufReader::new(Cursor::new(input));

        let args = read_client_arguments(&mut reader, Some(ProtocolVersion::V29))
            .expect("should read arguments");

        assert_eq!(args, vec!["--server"]);
    }

    #[test]
    fn read_client_arguments_handles_eof() {
        let input = b"--server\0--sender\0";
        let mut reader = BufReader::new(Cursor::new(input));

        let args = read_client_arguments(&mut reader, Some(ProtocolVersion::V30))
            .expect("should read arguments");

        assert_eq!(args, vec!["--server", "--sender"]);
    }

    #[test]
    fn read_client_arguments_empty_input() {
        let input = b"";
        let mut reader = BufReader::new(Cursor::new(input));

        let args = read_client_arguments(&mut reader, Some(ProtocolVersion::V30))
            .expect("should read arguments");

        assert!(args.is_empty());
    }

    // Note: perform_protocol_setup tests are challenging to write as unit tests
    // because the function requires bidirectional I/O (writing our version,
    // then reading client's version, then potentially exchanging compat flags).
    // Cursor<Vec<u8>> doesn't work well for bidirectional I/O patterns.
    // These behaviors are better tested via integration tests with actual TCP streams.

    #[test]
    fn perform_protocol_setup_rejects_invalid_version_zero() {
        let client_version = 0i32.to_le_bytes();
        // Need buffer with room for both write and read
        let mut buf = vec![0u8; 1024];
        buf[..4].copy_from_slice(&client_version);
        let mut stream = Cursor::new(buf);

        let result = perform_protocol_setup(&mut stream, ProtocolVersion::V30);
        assert!(result.is_err());
        let err = result.unwrap_err();
        // The error might be UnexpectedEof or InvalidData depending on exact stream state
        assert!(
            err.kind() == io::ErrorKind::InvalidData || err.kind() == io::ErrorKind::UnexpectedEof,
            "Expected InvalidData or UnexpectedEof, got {:?}",
            err.kind()
        );
    }

    #[test]
    fn perform_protocol_setup_rejects_invalid_version_negative() {
        let client_version = (-1i32).to_le_bytes();
        let mut buf = vec![0u8; 1024];
        buf[..4].copy_from_slice(&client_version);
        let mut stream = Cursor::new(buf);

        let result = perform_protocol_setup(&mut stream, ProtocolVersion::V30);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.kind() == io::ErrorKind::InvalidData || err.kind() == io::ErrorKind::UnexpectedEof,
            "Expected InvalidData or UnexpectedEof, got {:?}",
            err.kind()
        );
    }

    #[test]
    fn perform_protocol_setup_rejects_version_too_high() {
        let client_version = 256i32.to_le_bytes();
        let mut buf = vec![0u8; 1024];
        buf[..4].copy_from_slice(&client_version);
        let mut stream = Cursor::new(buf);

        let result = perform_protocol_setup(&mut stream, ProtocolVersion::V30);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.kind() == io::ErrorKind::InvalidData || err.kind() == io::ErrorKind::UnexpectedEof,
            "Expected InvalidData or UnexpectedEof, got {:?}",
            err.kind()
        );
    }

    #[test]
    fn apply_module_bandwidth_limit_disables_when_module_configured_none() {
        let mut limiter = Some(BandwidthLimiter::new(
            NonZeroU64::new(1024).unwrap(),
        ));

        let change = apply_module_bandwidth_limit(
            &mut limiter,
            None,
            false,
            true,  // module_limit_configured
            None,
            false,
        );

        assert_eq!(change, LimiterChange::Disabled);
        assert!(limiter.is_none());
    }

    #[test]
    fn apply_module_bandwidth_limit_preserves_when_not_configured() {
        let mut limiter = Some(BandwidthLimiter::new(
            NonZeroU64::new(1024).unwrap(),
        ));

        let change = apply_module_bandwidth_limit(
            &mut limiter,
            None,
            false,
            false,  // module_limit_configured
            None,
            false,
        );

        assert_eq!(change, LimiterChange::Unchanged);
        assert!(limiter.is_some());
    }

    #[test]
    fn apply_module_bandwidth_limit_enables_when_none_existed() {
        let mut limiter = None;

        let change = apply_module_bandwidth_limit(
            &mut limiter,
            NonZeroU64::new(2048),
            true,  // module_limit_specified
            true,  // module_limit_configured
            None,
            false,
        );

        assert_eq!(change, LimiterChange::Enabled);
        assert!(limiter.is_some());
        assert_eq!(limiter.as_ref().unwrap().limit_bytes().get(), 2048);
    }

    #[test]
    fn apply_module_bandwidth_limit_lowers_existing_limit() {
        let mut limiter = Some(BandwidthLimiter::new(
            NonZeroU64::new(2048).unwrap(),
        ));

        let change = apply_module_bandwidth_limit(
            &mut limiter,
            NonZeroU64::new(1024),
            true,
            true,
            None,
            false,
        );

        // Lowering the limit results in Updated
        assert_eq!(change, LimiterChange::Updated);
        assert_eq!(limiter.as_ref().unwrap().limit_bytes().get(), 1024);
    }

    #[test]
    fn apply_module_bandwidth_limit_unchanged_when_limit_higher() {
        let mut limiter = Some(BandwidthLimiter::new(
            NonZeroU64::new(1024).unwrap(),
        ));

        let change = apply_module_bandwidth_limit(
            &mut limiter,
            NonZeroU64::new(2048),
            true,
            true,
            None,
            false,
        );

        // Higher limit doesn't raise existing limit (cap function), so Unchanged
        assert_eq!(change, LimiterChange::Unchanged);
        assert_eq!(limiter.as_ref().unwrap().limit_bytes().get(), 1024);
    }

    #[test]
    fn apply_module_bandwidth_limit_burst_only_override() {
        let mut limiter = Some(BandwidthLimiter::new(
            NonZeroU64::new(1024).unwrap(),
        ));

        let change = apply_module_bandwidth_limit(
            &mut limiter,
            None,
            false,
            true,  // module_limit_configured
            NonZeroU64::new(4096),
            true,  // module_burst_specified
        );

        // Should update with burst
        assert_eq!(change, LimiterChange::Updated);
        assert!(limiter.is_some());
        assert_eq!(limiter.as_ref().unwrap().burst_bytes().unwrap().get(), 4096);
    }

    #[test]
    fn format_bandwidth_rate_displays_bytes() {
        let rate = NonZeroU64::new(512).unwrap();
        assert_eq!(format_bandwidth_rate(rate), "512 bytes/s");
    }

    #[test]
    fn format_bandwidth_rate_displays_kib() {
        let rate = NonZeroU64::new(2048).unwrap();
        assert_eq!(format_bandwidth_rate(rate), "2 KiB/s");
    }

    #[test]
    fn format_bandwidth_rate_displays_mib() {
        let rate = NonZeroU64::new(5 * 1024 * 1024).unwrap();
        assert_eq!(format_bandwidth_rate(rate), "5 MiB/s");
    }

    #[test]
    fn format_bandwidth_rate_displays_gib() {
        let rate = NonZeroU64::new(3 * 1024 * 1024 * 1024).unwrap();
        assert_eq!(format_bandwidth_rate(rate), "3 GiB/s");
    }

    #[test]
    fn format_bandwidth_rate_prefers_largest_unit() {
        let rate = NonZeroU64::new(1024).unwrap();
        assert_eq!(format_bandwidth_rate(rate), "1 KiB/s");

        let rate = NonZeroU64::new(1025).unwrap();
        assert_eq!(format_bandwidth_rate(rate), "1025 bytes/s");
    }

    // Tests for determine_server_role

    #[test]
    fn determine_server_role_returns_generator_with_sender_flag() {
        let args = vec![
            "--server".to_owned(),
            "--sender".to_owned(),
            "-vvre.iLsfxCIvu".to_owned(),
            ".".to_owned(),
            "testmod/".to_owned(),
        ];
        assert_eq!(determine_server_role(&args), ServerRole::Generator);
    }

    #[test]
    fn determine_server_role_returns_receiver_without_sender_flag() {
        let args = vec![
            "--server".to_owned(),
            "-vvre.iLsfxCIvu".to_owned(),
            ".".to_owned(),
            "testmod/".to_owned(),
        ];
        assert_eq!(determine_server_role(&args), ServerRole::Receiver);
    }

    #[test]
    fn determine_server_role_returns_receiver_for_empty_args() {
        let args: Vec<String> = vec![];
        assert_eq!(determine_server_role(&args), ServerRole::Receiver);
    }

    #[test]
    fn determine_server_role_ignores_sender_in_middle_of_other_arg() {
        // The "--sender" must be an exact match, not a substring
        let args = vec![
            "--server".to_owned(),
            "--not-a-sender".to_owned(),
            ".".to_owned(),
        ];
        assert_eq!(determine_server_role(&args), ServerRole::Receiver);
    }

    // Tests for extract_flag_string

    #[test]
    fn extract_flag_string_finds_short_flags() {
        let args = vec![
            "--server".to_owned(),
            "--sender".to_owned(),
            "-vvre.iLsfxCIvu".to_owned(),
            ".".to_owned(),
            "testmod/".to_owned(),
        ];
        assert_eq!(extract_flag_string(&args), "-vvre.iLsfxCIvu");
    }

    #[test]
    fn extract_flag_string_returns_empty_when_no_short_flags() {
        let args = vec![
            "--server".to_owned(),
            "--sender".to_owned(),
            ".".to_owned(),
            "testmod/".to_owned(),
        ];
        assert_eq!(extract_flag_string(&args), "");
    }

    #[test]
    fn extract_flag_string_returns_first_short_flag_group() {
        let args = vec![
            "--server".to_owned(),
            "-abc".to_owned(),
            "-xyz".to_owned(),
        ];
        // Should return first match
        assert_eq!(extract_flag_string(&args), "-abc");
    }

    #[test]
    fn extract_flag_string_handles_empty_args() {
        let args: Vec<String> = vec![];
        assert_eq!(extract_flag_string(&args), "");
    }

    #[test]
    fn extract_flag_string_ignores_double_dash_flags() {
        let args = vec![
            "--server".to_owned(),
            "--verbose".to_owned(),
        ];
        assert_eq!(extract_flag_string(&args), "");
    }

    // Tests for validate_module_path

    #[test]
    fn validate_module_path_returns_true_for_existing_path() {
        // Use temp_dir which always exists
        let path = std::env::temp_dir();
        assert!(validate_module_path(&path));
    }

    #[test]
    fn validate_module_path_returns_false_for_nonexistent_path() {
        let path = std::path::Path::new("/this/path/does/not/exist/at/all/12345");
        assert!(!validate_module_path(path));
    }

    #[test]
    fn validate_module_path_works_with_file_path() {
        // Create a temp file and check it
        let temp_dir = std::env::temp_dir();
        let test_file = temp_dir.join("test_validate_module_path.txt");
        std::fs::write(&test_file, "test").unwrap();
        assert!(validate_module_path(&test_file));
        std::fs::remove_file(&test_file).unwrap();
    }

    // Tests for error file functions

    #[test]
    fn log_file_error_creates_daemon_error_with_correct_code() {
        let path = std::path::Path::new("/tmp/test.log");
        let io_err = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "test error");
        let err = log_file_error(path, io_err);
        assert_eq!(err.exit_code(), FEATURE_UNAVAILABLE_EXIT_CODE);
    }

    #[test]
    fn log_file_error_message_contains_path() {
        let path = std::path::Path::new("/var/log/rsyncd.log");
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "not found");
        let err = log_file_error(path, io_err);
        let message = format!("{:?}", err.message());
        assert!(message.contains("/var/log/rsyncd.log"));
    }

    #[test]
    fn pid_file_error_creates_daemon_error_with_correct_code() {
        let path = std::path::Path::new("/var/run/rsyncd.pid");
        let io_err = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "test error");
        let err = pid_file_error(path, io_err);
        assert_eq!(err.exit_code(), FEATURE_UNAVAILABLE_EXIT_CODE);
    }

    #[test]
    fn pid_file_error_message_contains_path() {
        let path = std::path::Path::new("/var/run/rsyncd.pid");
        let io_err = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "access denied");
        let err = pid_file_error(path, io_err);
        let message = format!("{:?}", err.message());
        assert!(message.contains("/var/run/rsyncd.pid"));
    }

    #[test]
    fn lock_file_error_creates_daemon_error_with_correct_code() {
        let path = std::path::Path::new("/var/lock/rsyncd.lock");
        let io_err = std::io::Error::new(std::io::ErrorKind::AlreadyExists, "locked");
        let err = lock_file_error(path, io_err);
        assert_eq!(err.exit_code(), FEATURE_UNAVAILABLE_EXIT_CODE);
    }

    #[test]
    fn lock_file_error_message_contains_path() {
        let path = std::path::Path::new("/var/lock/rsyncd.lock");
        let io_err = std::io::Error::new(std::io::ErrorKind::AlreadyExists, "file locked");
        let err = lock_file_error(path, io_err);
        let message = format!("{:?}", err.message());
        assert!(message.contains("/var/lock/rsyncd.lock"));
    }

    // Tests for format_host

    #[test]
    fn format_host_returns_hostname_when_present() {
        use std::net::IpAddr;
        let host = Some("example.com");
        let fallback: IpAddr = "192.168.1.1".parse().unwrap();
        assert_eq!(format_host(host, fallback), "example.com");
    }

    #[test]
    fn format_host_returns_ip_when_hostname_missing() {
        use std::net::IpAddr;
        let host: Option<&str> = None;
        let fallback: IpAddr = "10.0.0.1".parse().unwrap();
        assert_eq!(format_host(host, fallback), "10.0.0.1");
    }

    #[test]
    fn format_host_returns_ipv6_when_hostname_missing() {
        use std::net::IpAddr;
        let host: Option<&str> = None;
        let fallback: IpAddr = "::1".parse().unwrap();
        assert_eq!(format_host(host, fallback), "::1");
    }
}

