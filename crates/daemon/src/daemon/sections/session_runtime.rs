struct SessionParams<'a> {
    modules: &'a [ModuleRuntime],
    motd_lines: &'a [String],
    daemon_limit: Option<NonZeroU64>,
    daemon_burst: Option<NonZeroU64>,
    log_sink: Option<SharedLogSink>,
    reverse_lookup: bool,
    delegation: Option<SessionDelegation>,
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
        delegation,
    } = params;

    if let Some(delegation) = delegation.as_ref() {
        let delegated = stream
            .try_clone()
            .and_then(|clone| delegate_binary_session(clone, delegation, log_sink.as_ref()));
        if delegated.is_ok() {
            drop(stream);
            return Ok(());
        }

        if let Some(log) = log_sink.as_ref() {
            let text = format!(
                "failed to delegate session to '{}'; continuing with internal handler",
                Path::new(delegation.binary()).display()
            );
            let message = rsync_warning!(text).with_role(Role::Daemon);
            log_message(log, &message);
        }
    }

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

    while let Some(line) = read_trimmed_line(&mut reader)? {
        match parse_legacy_daemon_message(&line) {
            Ok(LegacyDaemonMessage::Version(version)) => {
                negotiated_protocol = Some(version);
                messages.write_ok(reader.get_mut(), &mut limiter)?;
                reader.get_mut().flush()?;
                continue;
            }
            Ok(LegacyDaemonMessage::Other(payload)) => {
                if let Some(option) = parse_daemon_option(payload) {
                    refused_options.push(option.to_string());
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

        request = Some(line);
        break;
    }

    let request = request.unwrap_or_default();

    advertise_capabilities(reader.get_mut(), modules, &messages)?;

    if request == "#list" {
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
        )?;
    }

    Ok(())
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
    let client_raw = u32::from_be_bytes(client_bytes);
    ProtocolVersion::from_peer_advertisement(client_raw).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "binary negotiation protocol identifier outside supported range",
        )
    })?;

    let server_bytes = u32::from(ProtocolVersion::NEWEST.as_u8()).to_be_bytes();
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

