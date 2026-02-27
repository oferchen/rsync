struct SessionParams<'a> {
    modules: &'a [ModuleRuntime],
    motd_lines: &'a [String],
    daemon_limit: Option<NonZeroU64>,
    daemon_burst: Option<NonZeroU64>,
    log_sink: Option<SharedLogSink>,
    reverse_lookup: bool,
}

struct LegacySessionParams<'a> {
    modules: &'a [ModuleRuntime],
    motd_lines: &'a [String],
    daemon_limit: Option<NonZeroU64>,
    daemon_burst: Option<NonZeroU64>,
    log_sink: Option<SharedLogSink>,
    peer_host: Option<String>,
    reverse_lookup: bool,
}

#[cfg_attr(feature = "tracing", instrument(skip(stream, params), fields(peer = %peer_addr), name = "session_handler"))]
fn handle_session(
    stream: TcpStream,
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
    } = params;

    // rsync daemon protocol is ALWAYS the legacy @RSYNCD protocol.
    // Attempting to detect session style creates a deadlock: detect_session_style()
    // peeks at the socket waiting for client data, but the client is waiting for
    // the server to send the @RSYNCD greeting first!
    // Always use Legacy mode for daemon connections.
    let style = SessionStyle::Legacy;
    configure_stream(&stream)?;

    let peer_host = if reverse_lookup {
        resolve_peer_hostname(peer_addr.ip())
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

#[allow(dead_code)]
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SessionStyle {
    Legacy,
    #[allow(dead_code)]
    Binary,
}

fn write_limited(
    stream: &mut TcpStream,
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

#[cfg_attr(feature = "tracing", instrument(skip(stream, params), fields(peer = %peer_addr), name = "legacy_session"))]
fn handle_legacy_session(
    stream: TcpStream,
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
    let messages = LegacyMessageCache::new();

    let greeting = legacy_daemon_greeting();
    write_limited(reader.get_mut(), &mut limiter, greeting.as_bytes())?;
    reader.get_mut().flush()?;

    let mut request = None;
    let mut refused_options = Vec::new();
    let mut negotiated_protocol = None;
    let mut early_input_data: Option<Vec<u8>> = None;

    while let Some(line) = read_trimmed_line(&mut reader)? {
        match parse_legacy_daemon_message(&line) {
            Ok(LegacyDaemonMessage::Version(version)) => {
                // Record the negotiated protocol version but do NOT send @RSYNCD: OK here.
                // The OK is only sent after the module is selected and approved, not after
                // the version exchange. Sending OK here causes the client to misinterpret
                // subsequent protocol messages.
                negotiated_protocol = Some(version);
                continue;
            }
            Ok(LegacyDaemonMessage::Other(payload)) => {
                if let Some(option) = parse_daemon_option(payload) {
                    refused_options.push(option.to_owned());
                    continue;
                }
            }
            Ok(LegacyDaemonMessage::Exit) => return Ok(()),
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

        // upstream: clientserver.c:1357-1368 — the daemon checks if the first
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

    if request == "#list" {
        advertise_capabilities(reader.get_mut(), modules, &messages)?;
        if let Some(log) = log_sink.as_ref() {
            log_list_request(log, peer_host.as_deref(), peer_addr);
        }
        respond_with_module_list(
            reader.get_mut(),
            &mut limiter,
            modules,
            motd_lines,
            peer_addr.ip(),
            reverse_lookup,
            &messages,
        )?;
    } else if request.is_empty() {
        write_limited(
            reader.get_mut(),
            &mut limiter,
            HANDSHAKE_ERROR_PAYLOAD.as_bytes(),
        )?;
        write_limited(reader.get_mut(), &mut limiter, b"\n")?;
        messages.write_exit(reader.get_mut(), &mut limiter)?;
        reader.get_mut().flush()?;
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
            &messages,
            negotiated_protocol,
            early_input_data,
        )?;
    }

    Ok(())
}

/// Command prefix for the early-input protocol message.
///
/// upstream: clientserver.c — `#define EARLY_INPUT_CMD "#early_input="`
const EARLY_INPUT_CMD: &str = "#early_input=";

/// Maximum early-input data size in bytes.
///
/// upstream: rsync.h — `BIGPATHBUFLEN` is `MAXPATHLEN + 1024` (typically 5120).
const EARLY_INPUT_MAX_SIZE: usize = 5120;

/// Checks whether `line` is an `#early_input=<len>` command and, if so, reads
/// the specified number of raw bytes from the stream.
///
/// Returns `Ok(Some(data))` when the early-input command was recognized and the
/// data was read successfully, `Ok(None)` when the line is not an early-input
/// command, or an I/O error if reading fails or the length is invalid.
///
/// upstream: clientserver.c:1357-1364 — `rsync_module()` reads early input data
/// and stores it for later delivery to the pre-xfer exec script.
fn read_early_input(
    line: &str,
    reader: &mut impl Read,
) -> io::Result<Option<Vec<u8>>> {
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
    stream: TcpStream,
    daemon_limit: Option<NonZeroU64>,
    daemon_burst: Option<NonZeroU64>,
    log_sink: Option<SharedLogSink>,
) -> io::Result<()> {
    handle_binary_session_internal(stream, daemon_limit, daemon_burst, log_sink)
}

fn handle_binary_session_internal(
    mut stream: TcpStream,
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

    // Tests for SessionStyle

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

    // Tests for SessionParams

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
        };
        assert!(params.modules.is_empty());
        assert!(params.motd_lines.is_empty());
        assert!(params.daemon_limit.is_none());
        assert!(!params.reverse_lookup);
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
        };
        assert_eq!(params.daemon_limit, NonZeroU64::new(1000));
        assert_eq!(params.daemon_burst, NonZeroU64::new(2000));
        assert!(params.reverse_lookup);
    }

    // Tests for LegacySessionParams

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

    // Tests for read_early_input

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
}

