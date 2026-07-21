/// Immutable parameters shared across session handlers.
///
/// Carries the daemon-wide configuration (module table, MOTD, bandwidth
/// limits, log sink) that every connection handler needs. Passed by
/// reference from the accept loop to per-connection threads.
struct SessionParams<'a> {
    modules: &'a [ModuleRuntime],
    motd_lines: &'a [String],
    daemon_limit: Option<NonZeroU64>,
    daemon_burst: Option<NonZeroU64>,
    log_sink: Option<SharedLogSink>,
    reverse_lookup: bool,
    proxy_protocol: bool,
}

/// Parameters for the legacy `@RSYNCD:` session handler.
///
/// Extends [`SessionParams`] with the resolved peer hostname, which is
/// computed once in the top-level session handler and reused across the
/// greeting, module lookup, and authentication phases.
///
/// upstream: clientserver.c - the daemon resolves the peer hostname via
/// reverse DNS before entering the module request loop.
struct LegacySessionParams<'a> {
    modules: &'a [ModuleRuntime],
    motd_lines: &'a [String],
    daemon_limit: Option<NonZeroU64>,
    daemon_burst: Option<NonZeroU64>,
    log_sink: Option<SharedLogSink>,
    peer_host: Option<String>,
    reverse_lookup: bool,
}

/// Handles a single daemon connection from accept to completion.
///
/// Resolves the peer hostname (if reverse lookup is enabled), reads the
/// optional PROXY protocol header, and dispatches to the legacy `@RSYNCD:`
/// session handler. The function is the per-thread entry point called from
/// the accept loop with `catch_unwind` crash isolation.
///
/// upstream: clientserver.c - `start_daemon()` forks a child per connection;
/// each child calls `rsync_module()` which performs the full session lifecycle.
#[cfg_attr(feature = "tracing", instrument(skip(stream, params), fields(peer = %peer_addr), name = "session_handler"))]
fn handle_session(
    stream: DaemonStream,
    peer_addr: SocketAddr,
    params: SessionParams<'_>,
) -> io::Result<()> {
    let SessionParams {
        modules,
        motd_lines,
        daemon_limit,
        daemon_burst,
        log_sink,
        reverse_lookup,
        proxy_protocol,
    } = params;

    // rsync daemon protocol is ALWAYS the legacy @RSYNCD protocol.
    // Attempting to detect session style creates a deadlock: detect_session_style()
    // peeks at the socket waiting for client data, but the client is waiting for
    // the server to send the @RSYNCD greeting first!
    // Always use Legacy mode for daemon connections.
    let style = SessionStyle::Legacy;
    // The `@RSYNCD:` greeting exchange is deliberately left untimed, matching
    // upstream. Upstream keeps io_timeout at 0 (options.c:102) until a module
    // has been selected, only then arming lp_timeout(module_id)
    // (clientserver.c:1206); the handshake itself runs with no I/O deadline.
    // Arming one here tore down connections whose peer was momentarily
    // CPU-starved under a burst: the handshake read timed out, the worker
    // returned an error, and dropping the socket with the client's still-unread
    // request in the kernel buffer sent an RST that the client surfaced as
    // "Connection reset by peer". The per-module `timeout` directive still
    // governs the data phase via apply_module_timeout once the module is known.

    // upstream: clientserver.c:1298 - read PROXY protocol header before any
    // rsync protocol data when `proxy protocol = true` in the config.
    let mut stream = stream;
    let peer_addr = if proxy_protocol {
        match parse_proxy_header(&mut stream) {
            Ok(Some(proxied_addr)) => proxied_addr,
            Ok(None) => peer_addr,
            Err(error) => {
                if let Some(log) = log_sink.as_ref() {
                    let text =
                        format!("failed to read PROXY protocol header from {peer_addr}: {error}");
                    let message = rsync_warning!(text).with_role(Role::Daemon);
                    log_message(log, &message);
                }
                return Err(error);
            }
        }
    } else {
        peer_addr
    };

    // upstream: clientname.c `client_name` forward-confirms the reverse-DNS
    // name unconditionally; per-module `forward lookup` still governs the
    // access-control match in `module_peer_hostname`.
    let peer_host = if reverse_lookup {
        resolve_peer_hostname(peer_addr.ip(), true)
    } else {
        None
    };
    if let Some(log) = log_sink.as_ref() {
        log_connection(log, peer_host.as_deref(), peer_addr);
    }

    match style {
        SessionStyle::Binary => handle_binary_session(stream, daemon_limit, daemon_burst, log_sink),
        SessionStyle::Legacy => handle_legacy_session(
            stream,
            peer_addr,
            LegacySessionParams {
                modules,
                motd_lines,
                daemon_limit,
                daemon_burst,
                log_sink,
                peer_host,
                reverse_lookup,
            },
        ),
    }
}

/// Peeks at the first bytes from the client to determine the session style.
///
/// Currently unused because daemon connections always use the legacy protocol -
/// the server must send the `@RSYNCD:` greeting first, creating a deadlock if
/// we wait for client data to determine the style.
#[allow(dead_code)] // REASON: prepared for binary negotiation path; daemon always uses legacy
fn detect_session_style(stream: &TcpStream, fallback_available: bool) -> io::Result<SessionStyle> {
    stream.set_nonblocking(true)?;
    let mut peek_buf = [0u8; LEGACY_DAEMON_PREFIX_LEN];
    let decision = match stream.peek(&mut peek_buf) {
        Ok(0) => Ok(SessionStyle::Legacy),
        Ok(_) => {
            if peek_buf[0] == b'@' {
                Ok(SessionStyle::Legacy)
            } else {
                Ok(SessionStyle::Binary)
            }
        }
        Err(error) if error.kind() == io::ErrorKind::WouldBlock && fallback_available => {
            Ok(SessionStyle::Binary)
        }
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(SessionStyle::Legacy),
        Err(error) => Err(error),
    };
    let restore_result = stream.set_nonblocking(false);
    match (decision, restore_result) {
        (Ok(style), Ok(())) => Ok(style),
        (Ok(_), Err(error)) => Err(error),
        (Err(error), Ok(())) => Err(error),
        (Err(primary), Err(restore)) => Err(io::Error::new(
            primary.kind(),
            format!("{primary}; also failed to restore blocking mode: {restore}",),
        )),
    }
}

/// Discriminates between the two wire-level negotiation styles.
///
/// The legacy style uses line-oriented `@RSYNCD:` text messages for the
/// greeting and module selection phases. The binary style uses 4-byte
/// little-endian integers for the initial version exchange, as used by
/// the multiplex I/O layer in protocol versions 28+.
///
/// In daemon mode the protocol is always legacy - the server sends the
/// `@RSYNCD:` greeting first and the client responds in kind.
///
/// upstream: clientserver.c - daemon connections always use the legacy
/// `@RSYNCD:` greeting protocol.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SessionStyle {
    /// Line-oriented `@RSYNCD:` text protocol.
    Legacy,
    /// Binary 4-byte LE version exchange followed by multiplex frames.
    #[allow(dead_code)] // REASON: prepared for binary negotiation path
    Binary,
}

/// Writes `payload` to `stream`, respecting the optional bandwidth limiter.
///
/// When a limiter is active, the payload is split into recommended-size
/// chunks and each chunk is registered with the limiter before sending.
/// When no limiter is present, the payload is written in a single call.
fn write_limited(
    stream: &mut DaemonStream,
    limiter: &mut Option<BandwidthLimiter>,
    payload: &[u8],
) -> io::Result<()> {
    if let Some(limiter) = limiter {
        let mut remaining = payload;
        while !remaining.is_empty() {
            let chunk_len = limiter.recommended_read_size(remaining.len());
            stream.write_all(&remaining[..chunk_len])?;
            let _ = limiter.register(chunk_len);
            remaining = &remaining[chunk_len..];
        }
        Ok(())
    } else {
        stream.write_all(payload)
    }
}

/// Runs the legacy `@RSYNCD:` session protocol for a single connection.
///
/// Sends the greeting with the protocol version and supported digest list,
/// reads the client's version response and module request, then dispatches
/// to either `#list` handling or module-specific access control and transfer.
///
/// upstream: clientserver.c - the daemon greeting/response sequence is:
/// 1. Server sends `@RSYNCD: 32.0 sha512 sha256 sha1 md5 md4\n`
/// 2. Client responds with `@RSYNCD: 32.0 sha512 sha256 sha1 md5 md4\n`
/// 3. Client sends module name (or `#list`)
#[cfg_attr(feature = "tracing", instrument(skip(stream, params), fields(peer = %peer_addr), name = "legacy_session"))]
fn handle_legacy_session(
    stream: DaemonStream,
    peer_addr: SocketAddr,
    params: LegacySessionParams<'_>,
) -> io::Result<()> {
    let LegacySessionParams {
        modules,
        motd_lines,
        daemon_limit,
        daemon_burst,
        log_sink,
        peer_host,
        reverse_lookup,
    } = params;
    let mut reader = BufReader::new(stream);
    let mut limiter = BandwidthLimitComponents::new(daemon_limit, daemon_burst).into_limiter();
    // DIS-4.a R3: borrow the process-wide cache instead of rebuilding the
    // `@RSYNCD: OK\n` / `@RSYNCD: EXIT\n` boxes per accepted connection.
    let messages = LegacyMessageCache::shared();

    // FSM: connection starts in Greeting - the server is about to send the
    // @RSYNCD: greeting and wait for the client's version response.
    let mut conn_state = ConnectionState::Greeting;

    // DIS-4.a R2: write the cached newest-protocol greeting bytes directly,
    // skipping the per-accept `format!`/`push_str` chain.
    // upstream: clientserver.c:455 output_daemon_greeting
    write_limited(
        reader.get_mut(),
        &mut limiter,
        cached_legacy_daemon_greeting(),
    )?;

    // upstream: clientserver.c:158-170 exchange_protocols() - immediately after
    // the greeting the daemon dumps the MOTD file verbatim and appends a single
    // trailing newline (write_sbuf(f_out, "\n")), before reading the client's
    // version/module request. Emitting it here (rather than only in the module
    // listing) mirrors upstream: the MOTD precedes every response, including an
    // @ERROR refusal for an unknown module.
    if !motd_lines.is_empty() {
        for line in motd_lines {
            write_limited(reader.get_mut(), &mut limiter, line.as_bytes())?;
            write_limited(reader.get_mut(), &mut limiter, b"\n")?;
        }
        write_limited(reader.get_mut(), &mut limiter, b"\n")?;
    }

    let mut request = None;
    let mut refused_options = Vec::new();
    let mut negotiated_protocol = None;
    let mut early_input_data: Option<Vec<u8>> = None;

    // TCP_QUICKACK is one-shot; re-arm before each handshake read so every
    // round's ACK stays immediate across the multi-line greeting exchange.
    fast_io::rearm_tcp_quickack(reader.get_ref().tcp_stream());
    while let Some(line) = read_trimmed_line(&mut reader)? {
        fast_io::rearm_tcp_quickack(reader.get_ref().tcp_stream());
        // upstream: clientserver.c:180-211 exchange_protocols() (am_client == 0) -
        // before proceeding the daemon validates the client's version greeting,
        // refusing one that omits the subprotocol value (protocol >= 30) or the
        // digest name list (protocol > 31). The refusal is a fatal pre-OK
        // @ERROR line, after which the client returns and the socket closes.
        if negotiated_protocol.is_none() {
            if let Some(payload) = reject_malformed_client_greeting(&line) {
                write_limited(reader.get_mut(), &mut limiter, payload.as_bytes())?;
                write_limited(reader.get_mut(), &mut limiter, b"\n")?;
                reader.get_mut().flush()?;
                // FSM: -> Closing after the fatal @ERROR refusal.
                let _ = conn_state.transition(ConnectionState::Closing);
                return Ok(());
            }
        }
        match parse_legacy_daemon_message(&line) {
            Ok(LegacyDaemonMessage::Version(version)) => {
                // Record the negotiated protocol version but do NOT send @RSYNCD: OK here.
                // The OK is only sent after the module is selected and approved, not after
                // the version exchange. Sending OK here causes the client to misinterpret
                // subsequent protocol messages.
                negotiated_protocol = Some(version);
                // FSM: Greeting -> ModuleSelect - version exchange complete,
                // now waiting for the client to request a module name.
                conn_state = conn_state
                    .transition(ConnectionState::ModuleSelect)
                    .map_err(transition_error)?;
                continue;
            }
            Ok(LegacyDaemonMessage::Other(payload)) => {
                if let Some(option) = parse_daemon_option(payload) {
                    refused_options.push(option.to_owned());
                    continue;
                }
            }
            Ok(LegacyDaemonMessage::Exit) => {
                // FSM: -> Closing on client-initiated exit.
                let _ = conn_state.transition(ConnectionState::Closing);
                return Ok(());
            }
            Ok(
                LegacyDaemonMessage::Ok
                | LegacyDaemonMessage::Capabilities { .. }
                | LegacyDaemonMessage::AuthRequired { .. }
                | LegacyDaemonMessage::AuthChallenge { .. },
            ) => {
                request = Some(line);
                break;
            }
            Err(_) => {}
        }

        // upstream: clientserver.c:1357-1368 - the daemon checks if the first
        // non-@RSYNCD line is `#early_input=<len>`. If so, it reads <len> bytes
        // of raw data and then reads the next line as the module name.
        if let Some(data) = read_early_input(&line, &mut reader)? {
            early_input_data = Some(data);
            continue;
        }

        request = Some(line);
        break;
    }

    let request = request.unwrap_or_default();

    if request.is_empty() || request == "#list" {
        // upstream: clientserver.c:1420 - `if (!*line || strcmp(line,
        // "#list") == 0) { send_listing(); }` - both an empty module
        // name (the client connected with `rsync rsync://host/`) and an
        // explicit `#list` request fall through to the module listing.
        // The #list handler does NOT send @RSYNCD: CAP before the
        // listing; capabilities are only sent after module selection
        // during the transfer handshake.
        if let Some(log) = log_sink.as_ref() {
            log_list_request(log, peer_host.as_deref(), peer_addr);
        }
        respond_with_module_list(reader.get_mut(), &mut limiter, modules, messages)?;
        // FSM: -> Closing after sending the module list and EXIT.
        _ = conn_state
            .transition(ConnectionState::Closing)
            .map_err(transition_error)?;
    } else if request.starts_with('#') {
        // upstream: clientserver.c:1427-1431 - `if (*line == '#') { io_printf(
        // f_out, "@ERROR: Unknown command '%s'\n", line); return -1; }`. A
        // `#`-prefixed request that is neither `#list` (handled above) nor the
        // already-consumed `#early_input=` command is a command the daemon does
        // not recognize. It is rejected with the unknown-command error - keeping
        // the raw line including the leading `#` - which is distinct from the
        // unknown-module response reserved for a bad module name. The client
        // treats `@ERROR` as fatal and closes without reading further.
        let command_display = sanitize_module_identifier(&request);
        let payload = UNKNOWN_COMMAND_PAYLOAD.replace("{command}", command_display.as_ref());
        send_error(reader.get_mut(), &mut limiter, &payload)?;
        // FSM: -> Closing after rejecting the unknown command.
        _ = conn_state
            .transition(ConnectionState::Closing)
            .map_err(transition_error)?;
    } else {
        respond_with_module_request(
            &mut reader,
            &mut limiter,
            modules,
            &request,
            peer_addr.ip(),
            peer_host.as_deref(),
            &refused_options,
            log_sink.as_ref(),
            reverse_lookup,
            messages,
            negotiated_protocol,
            early_input_data,
            conn_state,
        )?;
    }

    Ok(())
}

/// Command prefix for the early-input protocol message.
///
/// upstream: clientserver.c - `#define EARLY_INPUT_CMD "#early_input="`
const EARLY_INPUT_CMD: &str = "#early_input=";

/// Maximum early-input data size in bytes.
///
/// upstream: rsync.h - `BIGPATHBUFLEN` is `MAXPATHLEN + 1024` (typically 5120).
const EARLY_INPUT_MAX_SIZE: usize = 5120;

/// Checks whether `line` is an `#early_input=<len>` command and, if so, reads
/// the specified number of raw bytes from the stream.
///
/// Returns `Ok(Some(data))` when the early-input command was recognized and the
/// data was read successfully, `Ok(None)` when the line is not an early-input
/// command, or an I/O error if reading fails or the length is invalid.
///
/// upstream: clientserver.c:1357-1364 - `rsync_module()` reads early input data
/// and stores it for later delivery to the pre-xfer exec script.
fn read_early_input(line: &str, reader: &mut impl Read) -> io::Result<Option<Vec<u8>>> {
    let len_str = match line.strip_prefix(EARLY_INPUT_CMD) {
        Some(rest) => rest,
        None => return Ok(None),
    };

    let data_len: usize = len_str.parse().map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("invalid early_input length: {len_str}"),
        )
    })?;

    if data_len == 0 || data_len > EARLY_INPUT_MAX_SIZE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("early_input length {data_len} out of range (1..={EARLY_INPUT_MAX_SIZE})"),
        ));
    }

    let mut buf = vec![0u8; data_len];
    reader.read_exact(&mut buf)?;

    Ok(Some(buf))
}

fn handle_binary_session(
    stream: DaemonStream,
    daemon_limit: Option<NonZeroU64>,
    daemon_burst: Option<NonZeroU64>,
    log_sink: Option<SharedLogSink>,
) -> io::Result<()> {
    handle_binary_session_internal(stream, daemon_limit, daemon_burst, log_sink)
}

fn handle_binary_session_internal(
    mut stream: DaemonStream,
    daemon_limit: Option<NonZeroU64>,
    daemon_burst: Option<NonZeroU64>,
    log_sink: Option<SharedLogSink>,
) -> io::Result<()> {
    let mut limiter = BandwidthLimitComponents::new(daemon_limit, daemon_burst).into_limiter();

    let mut client_bytes = [0u8; 4];
    stream.read_exact(&mut client_bytes)?;
    // upstream: io.c read_int() uses IVAL which is little-endian
    let client_raw = u32::from_le_bytes(client_bytes);
    ProtocolVersion::from_peer_advertisement(client_raw).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "binary negotiation protocol identifier outside supported range",
        )
    })?;

    // upstream: io.c write_int() uses SIVAL which is little-endian
    let server_bytes = u32::from(ProtocolVersion::NEWEST.as_u8()).to_le_bytes();
    stream.write_all(&server_bytes)?;
    stream.flush()?;

    let mut frames = Vec::new();
    MessageFrame::new(
        MessageCode::Error,
        HANDSHAKE_ERROR_PAYLOAD.as_bytes().to_vec(),
    )?
    .encode_into_writer(&mut frames)?;
    let exit_code = u32::try_from(FEATURE_UNAVAILABLE_EXIT_CODE).unwrap_or_default();
    MessageFrame::new(MessageCode::ErrorExit, exit_code.to_be_bytes().to_vec())?
        .encode_into_writer(&mut frames)?;
    write_limited(&mut stream, &mut limiter, &frames)?;
    stream.flush()?;

    if let Some(log) = log_sink.as_ref() {
        let message =
            rsync_info!("binary negotiation forwarded error frames").with_role(Role::Daemon);
        log_message(log, &message);
    }

    Ok(())
}

#[cfg(test)]
mod session_runtime_tests {
    use super::*;

    #[test]
    fn session_style_eq_legacy() {
        assert_eq!(SessionStyle::Legacy, SessionStyle::Legacy);
    }

    #[test]
    fn session_style_eq_binary() {
        assert_eq!(SessionStyle::Binary, SessionStyle::Binary);
    }

    #[test]
    fn session_style_ne() {
        assert_ne!(SessionStyle::Legacy, SessionStyle::Binary);
    }

    #[test]
    fn session_style_clone() {
        let style = SessionStyle::Legacy;
        let cloned = style;
        assert_eq!(style, cloned);
    }

    #[test]
    fn session_style_debug() {
        let style = SessionStyle::Legacy;
        let debug = format!("{style:?}");
        assert!(debug.contains("Legacy"));
    }

    #[test]
    fn session_params_fields() {
        let modules: Vec<ModuleRuntime> = vec![];
        let motd_lines: Vec<String> = vec![];
        let params = SessionParams {
            modules: &modules,
            motd_lines: &motd_lines,
            daemon_limit: None,
            daemon_burst: None,
            log_sink: None,
            reverse_lookup: false,
            proxy_protocol: false,
        };
        assert!(params.modules.is_empty());
        assert!(params.motd_lines.is_empty());
        assert!(params.daemon_limit.is_none());
        assert!(!params.reverse_lookup);
        assert!(!params.proxy_protocol);
    }

    #[test]
    fn session_params_with_limits() {
        let modules: Vec<ModuleRuntime> = vec![];
        let motd_lines: Vec<String> = vec![];
        let limit = NonZeroU64::new(1000);
        let burst = NonZeroU64::new(2000);
        let params = SessionParams {
            modules: &modules,
            motd_lines: &motd_lines,
            daemon_limit: limit,
            daemon_burst: burst,
            log_sink: None,
            reverse_lookup: true,
            proxy_protocol: false,
        };
        assert_eq!(params.daemon_limit, NonZeroU64::new(1000));
        assert_eq!(params.daemon_burst, NonZeroU64::new(2000));
        assert!(params.reverse_lookup);
    }

    #[test]
    fn legacy_session_params_fields() {
        let modules: Vec<ModuleRuntime> = vec![];
        let motd_lines: Vec<String> = vec![];
        let params = LegacySessionParams {
            modules: &modules,
            motd_lines: &motd_lines,
            daemon_limit: None,
            daemon_burst: None,
            log_sink: None,
            peer_host: None,
            reverse_lookup: false,
        };
        assert!(params.modules.is_empty());
        assert!(params.peer_host.is_none());
    }

    #[test]
    fn legacy_session_params_with_host() {
        let modules: Vec<ModuleRuntime> = vec![];
        let motd_lines: Vec<String> = vec![];
        let params = LegacySessionParams {
            modules: &modules,
            motd_lines: &motd_lines,
            daemon_limit: None,
            daemon_burst: None,
            log_sink: None,
            peer_host: Some("example.com".to_owned()),
            reverse_lookup: true,
        };
        assert_eq!(params.peer_host.as_deref(), Some("example.com"));
        assert!(params.reverse_lookup);
    }

    #[test]
    fn read_early_input_parses_valid_command() {
        let data = b"hello world";
        let mut cursor = io::Cursor::new(data.to_vec());
        let result = read_early_input("#early_input=11", &mut cursor).unwrap();
        assert_eq!(result, Some(b"hello world".to_vec()));
    }

    #[test]
    fn read_early_input_returns_none_for_non_command() {
        let mut cursor = io::Cursor::new(Vec::new());
        let result = read_early_input("mymodule", &mut cursor).unwrap();
        assert_eq!(result, None);
    }

    #[test]
    fn read_early_input_returns_none_for_empty_line() {
        let mut cursor = io::Cursor::new(Vec::new());
        let result = read_early_input("", &mut cursor).unwrap();
        assert_eq!(result, None);
    }

    #[test]
    fn read_early_input_rejects_zero_length() {
        let mut cursor = io::Cursor::new(Vec::new());
        let result = read_early_input("#early_input=0", &mut cursor);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("out of range"));
    }

    #[test]
    fn read_early_input_rejects_exceeding_max_size() {
        let mut cursor = io::Cursor::new(Vec::new());
        let too_large = EARLY_INPUT_MAX_SIZE + 1;
        let line = format!("#early_input={too_large}");
        let result = read_early_input(&line, &mut cursor);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("out of range"));
    }

    #[test]
    fn read_early_input_rejects_non_numeric_length() {
        let mut cursor = io::Cursor::new(Vec::new());
        let result = read_early_input("#early_input=abc", &mut cursor);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("invalid early_input length"));
    }

    #[test]
    fn read_early_input_reads_binary_data() {
        let data: Vec<u8> = (0..=255u8).collect();
        let mut cursor = io::Cursor::new(data.clone());
        let line = format!("#early_input={}", data.len());
        let result = read_early_input(&line, &mut cursor).unwrap();
        assert_eq!(result, Some(data));
    }

    #[test]
    fn read_early_input_at_max_size() {
        let data = vec![0xABu8; EARLY_INPUT_MAX_SIZE];
        let mut cursor = io::Cursor::new(data.clone());
        let line = format!("#early_input={EARLY_INPUT_MAX_SIZE}");
        let result = read_early_input(&line, &mut cursor).unwrap();
        assert_eq!(result, Some(data));
    }

    #[test]
    fn read_early_input_roundtrip_with_send_format() {
        let payload = b"authentication-token-xyz";
        let header = format!("{EARLY_INPUT_CMD}{}\n", payload.len());
        let mut wire = header.into_bytes();
        wire.extend_from_slice(payload);

        // The daemon reads lines; `#early_input=24` would be the trimmed line.
        let line = format!("{EARLY_INPUT_CMD}{}", payload.len());
        let mut cursor = io::Cursor::new(payload.to_vec());
        let result = read_early_input(&line, &mut cursor).unwrap();
        assert_eq!(result.unwrap(), payload);
    }

    #[test]
    fn read_early_input_returns_error_on_short_stream() {
        // Only 3 bytes available but header says 10
        let data = vec![1u8, 2, 3];
        let mut cursor = io::Cursor::new(data);
        let result = read_early_input("#early_input=10", &mut cursor);
        assert!(result.is_err());
    }

    #[test]
    fn early_input_cmd_constant_matches_upstream() {
        assert_eq!(EARLY_INPUT_CMD, "#early_input=");
    }

    #[test]
    fn early_input_max_size_is_5k() {
        assert_eq!(EARLY_INPUT_MAX_SIZE, 5120);
    }

    #[test]
    fn fsm_greeting_to_module_select() {
        let state = ConnectionState::Greeting;
        let state = state.transition(ConnectionState::ModuleSelect).unwrap();
        assert_eq!(state, ConnectionState::ModuleSelect);
    }

    #[test]
    fn fsm_module_select_to_closing_on_list() {
        let state = ConnectionState::ModuleSelect;
        let state = state.transition(ConnectionState::Closing).unwrap();
        assert!(state.is_terminal());
    }

    #[test]
    fn fsm_full_lifecycle_without_auth() {
        let mut state = ConnectionState::Greeting;
        state = state.transition(ConnectionState::ModuleSelect).unwrap();
        state = state.transition(ConnectionState::Transferring).unwrap();
        state = state.transition(ConnectionState::Closing).unwrap();
        assert!(state.is_terminal());
    }

    #[test]
    fn fsm_full_lifecycle_with_auth() {
        let mut state = ConnectionState::Greeting;
        state = state.transition(ConnectionState::ModuleSelect).unwrap();
        state = state.transition(ConnectionState::Authenticating).unwrap();
        state = state.transition(ConnectionState::Transferring).unwrap();
        state = state.transition(ConnectionState::Closing).unwrap();
        assert!(state.is_terminal());
    }

    #[test]
    fn fsm_early_close_from_module_select() {
        let mut state = ConnectionState::Greeting;
        state = state.transition(ConnectionState::ModuleSelect).unwrap();
        state = state.transition(ConnectionState::Closing).unwrap();
        assert!(state.is_terminal());
    }

    #[test]
    fn fsm_auth_failure_transitions_to_closing() {
        let mut state = ConnectionState::Greeting;
        state = state.transition(ConnectionState::ModuleSelect).unwrap();
        state = state.transition(ConnectionState::Authenticating).unwrap();
        state = state.transition(ConnectionState::Closing).unwrap();
        assert!(state.is_terminal());
    }

    #[test]
    fn fsm_skip_auth_to_transfer() {
        let mut state = ConnectionState::Greeting;
        state = state.transition(ConnectionState::ModuleSelect).unwrap();
        // When no auth required, skip Authenticating and go to Transferring.
        state = state.transition(ConnectionState::Transferring).unwrap();
        assert_eq!(state, ConnectionState::Transferring);
    }

    #[test]
    fn fsm_invalid_greeting_to_transferring() {
        let state = ConnectionState::Greeting;
        let result = state.transition(ConnectionState::Transferring);
        assert!(result.is_err());
    }

    #[test]
    fn fsm_no_double_close() {
        let state = ConnectionState::Closing;
        let result = state.transition(ConnectionState::Closing);
        assert!(result.is_err());
    }

    #[test]
    fn transition_error_produces_invalid_data() {
        let err = InvalidTransition {
            from: ConnectionState::Greeting,
            to: ConnectionState::Transferring,
        };
        let io_err = transition_error(err);
        assert_eq!(io_err.kind(), io::ErrorKind::InvalidData);
        assert!(io_err.to_string().contains("Greeting"));
        assert!(io_err.to_string().contains("Transferring"));
    }
}
