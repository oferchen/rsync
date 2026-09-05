/// Immutable parameters shared across session handlers.
///
/// Carries the daemon-wide configuration (module table, MOTD, bandwidth
/// limits, log sink) that every connection handler needs. Passed by
/// reference from the accept loop to per-connection threads.
struct SessionParams<'a> {
    modules: &'a [ModuleRuntime],
    motd_lines: &'a [String],
    daemon_limit: Option<NonZeroU64>,
    log_sink: Option<SharedLogSink>,
    reverse_lookup: bool,
    proxy_policy: ProxyProtocolPolicy,
    /// Daemon-wide `timeout`, bounding the pre-module handshake phase.
    ///
    /// upstream: clientserver.c:1441 arms the handshake deadline from
    /// `daemon_handshake_timeout(-1)`, i.e. `lp_timeout(-1)` - the GLOBAL
    /// value, read before any module is selected. `None` is upstream's
    /// `timeout <= 0`, which takes the built-in `DAEMON_HANDSHAKE_TIMEOUT`.
    daemon_timeout: Option<NonZeroU64>,
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
    log_sink: Option<SharedLogSink>,
    peer_host: Option<String>,
    reverse_lookup: bool,
    /// Daemon-wide `timeout`, bounding the pre-module handshake phase.
    ///
    /// upstream: clientserver.c:1441 arms the handshake deadline from
    /// `daemon_handshake_timeout(-1)`, i.e. `lp_timeout(-1)` - the GLOBAL
    /// value, read before any module is selected. `None` is upstream's
    /// `timeout <= 0`, which takes the built-in `DAEMON_HANDSHAKE_TIMEOUT`.
    daemon_timeout: Option<NonZeroU64>,
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
        log_sink,
        reverse_lookup,
        proxy_policy,
        daemon_timeout,
    } = params;

    // rsync daemon protocol is ALWAYS the legacy @RSYNCD protocol.
    // Attempting to detect session style creates a deadlock: detect_session_style()
    // peeks at the socket waiting for client data, but the client is waiting for
    // the server to send the @RSYNCD greeting first!
    // Always use Legacy mode for daemon connections.
    let style = SessionStyle::Legacy;
    // The `@RSYNCD:` greeting exchange is bounded by upstream's handshake
    // deadline - see `handshake_deadline` for the contract and its citations.
    //
    // ⚠ This block previously asserted the opposite ("deliberately left
    // untimed, matching upstream") on the strength of io_timeout staying 0
    // until a module is selected (options.c:102). That reading was accurate
    // against the 3.4.4 pin and is FALSE at 3.5.0: `grep -c daemon_handshake`
    // over io.c and clientserver.c gives 0 at 3.4.4 and 6 at each 3.5.0 file.
    // The io_timeout premise is still true and simply does not carry the
    // conclusion - 3.5.0's deadline is a SEPARATE mechanism from io_timeout.
    //
    // The regression that block recorded is real and is why the SHAPE matters:
    // arming a short per-read timeout here tore down connections whose peer was
    // momentarily CPU-starved, and dropping the socket with the client's unread
    // request still buffered sent an RST surfacing as "Connection reset by
    // peer". Upstream's mechanism does not do that - it is a 60s ABSOLUTE bound
    // on the whole phase, which a starved peer clears by orders of magnitude,
    // and on expiry it DIAGNOSES and exits RERR_TIMEOUT rather than dropping
    // the socket mutely. A per-read idle timeout would re-introduce exactly the
    // regression above AND be defeated by a client that trickles one byte at a
    // time, which is the shape the upstream cell probes.
    //
    // The per-module `timeout` directive still governs the data phase via
    // apply_module_timeout once the module is known.

    // upstream: clientserver.c:1443-1446 - `if (lp_proxy_protocol())` reads the
    // header before any rsync protocol data, but ONLY after the peer clears the
    // trusted-proxy gate:
    //
    //     if (lp_proxy_protocol()) {
    //             if (!proxy_peer_allowed(f_in) || !read_proxy_protocol_header(f_in))
    //                     return -1;
    //     }
    //
    // The gate is the security property, not a nicety. A PROXY header names
    // the address the daemon then treats as the peer - it feeds `hosts allow`
    // / `hosts deny`, `%h`, and every log line - so believing one from an
    // arbitrary direct connection lets that connection choose its own source
    // address. Upstream fail-closes: an unset trusted-proxy list rejects
    // everyone (access.c:302-303).
    let mut stream = stream;
    let peer_addr = match proxy_policy.decide(peer_addr.ip()) {
        ProxyHeaderDecision::NotRequired => peer_addr,
        ProxyHeaderDecision::Untrusted => {
            // upstream: clientserver.c:1394 - `rprintf(FLOG, "proxy protocol
            // rejected from untrusted peer %s (%s)\n", host, addr)`. The
            // wording is upstream's verbatim; the 3.5.0
            // `proxy-protocol-trusted-peer` cell greps the daemon log for it.
            if let Some(log) = log_sink.as_ref() {
                let host = peer_host_display(None, reverse_lookup);
                let text = format!(
                    "proxy protocol rejected from untrusted peer {host} ({})",
                    peer_addr.ip()
                );
                let message = rsync_warning!(text).with_role(Role::Daemon);
                log_message(log, &message);
            }
            return Ok(());
        }
        ProxyHeaderDecision::Trusted => match parse_proxy_header(&mut stream) {
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
        },
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
        log_connection(
            log,
            peer_host_display(peer_host.as_deref(), reverse_lookup),
            peer_addr,
        );
    }

    match style {
        SessionStyle::Binary => handle_binary_session(stream, daemon_limit, log_sink),
        SessionStyle::Legacy => {
            // upstream: clientserver.c - the per-connection child owns its own
            // `exit_cleanup()`, so a session-fatal refusal never reaches the
            // listening parent. Drop the code here for the same reason: it must
            // not tear down the accept loop.
            handle_legacy_session(
                stream,
                peer_addr,
                LegacySessionParams {
                    modules,
                    motd_lines,
                    daemon_limit,
                    log_sink,
                    peer_host,
                    reverse_lookup,
                    daemon_timeout,
                },
            )
            .map(|_| ())
        }
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

/// Maps the outcome of a single-session daemon run onto a process exit status.
///
/// The stdio and inetd entry points serve exactly one connection, so the process
/// plays the role of upstream's per-connection child: a session-fatal refusal
/// becomes the exit status, exactly as `exit_cleanup()` ends that child. An I/O
/// failure keeps the existing socket-IO code and `{context}: {error}` wording.
pub(crate) fn single_session_exit(
    outcome: io::Result<Option<ExitCode>>,
    context: &str,
) -> Result<(), DaemonError> {
    match outcome {
        Ok(None) => Ok(()),
        Ok(Some(exit)) => {
            let code = exit.as_i32();
            Err(DaemonError::new(
                code,
                rsync_error!(code, exit.description()).with_role(Role::Daemon),
            ))
        }
        Err(error) => Err(DaemonError::new(
            SOCKET_IO_EXIT_CODE,
            rsync_error!(SOCKET_IO_EXIT_CODE, format!("{context}: {error}"))
                .with_role(Role::Daemon),
        )),
    }
}

/// Takes the FSM's teardown edge as the session handler returns, forwarding
/// `exit_code` unchanged.
///
/// The edge is total, since a connection can be torn down from any lifecycle
/// state, so there is no error to propagate and none for a caller to discard.
/// Nothing reads the state once the handler returns, so what is worth
/// recording is the invariant rather than the value: a handler must not reach
/// a second teardown after already closing.
fn end_session(state: ConnectionState, exit_code: Option<ExitCode>) -> Option<ExitCode> {
    debug_assert!(
        !state.is_terminal(),
        "daemon session torn down twice, from {state:?}"
    );
    let _closed: ConnectionState = state.close();
    exit_code
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
///
/// Returns `Some(code)` when the session ended the way upstream's per-connection
/// child ends on `exit_cleanup()` (currently only compat.c:875's
/// `RERR_UNSUPPORTED` auth-digest refusal). Upstream forks per connection, so the
/// listening parent is unaffected; only the single-session entry points (stdio,
/// inetd) turn the code into a process exit status.
#[cfg_attr(feature = "tracing", instrument(skip(stream, params), fields(peer = %peer_addr), name = "legacy_session"))]
fn handle_legacy_session(
    stream: DaemonStream,
    peer_addr: SocketAddr,
    params: LegacySessionParams<'_>,
) -> io::Result<Option<ExitCode>> {
    let LegacySessionParams {
        modules,
        motd_lines,
        daemon_limit,
        log_sink,
        peer_host,
        reverse_lookup,
        daemon_timeout,
    } = params;
    let mut reader = BufReader::new(stream);
    let mut limiter = BandwidthLimitComponents::new(daemon_limit).into_limiter();
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

    // upstream: clientserver.c:160-172 exchange_protocols() - immediately after
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
    let mut client_digests: Option<String> = None;
    let mut session_exit_code: Option<ExitCode> = None;
    let mut early_input_data: Option<Vec<u8>> = None;

    // upstream: clientserver.c:1441 - `set_daemon_handshake_timeout(
    // daemon_handshake_timeout(-1))` is armed before ANY peer input is read,
    // from the DAEMON-WIDE `timeout` (module -1). No module has been selected
    // here, and a module's own value can only shorten a LATER phase, so the
    // global value is the only correct input to this arm.
    let deadline = HandshakeDeadline::armed(handshake_timeout(daemon_timeout));
    // A second descriptor onto the same socket, so the deadline can clamp
    // SO_RCVTIMEO without aliasing the reader's borrow. `None` for stdio, where
    // there is no socket to clamp and the deadline's own expiry arm is the
    // whole bound.
    let deadline_socket = reader
        .get_ref()
        .tcp_stream()
        .and_then(|stream| stream.try_clone().ok());

    // TCP_QUICKACK is one-shot; re-arm before each handshake read so every
    // round's ACK stays immediate across the multi-line greeting exchange.
    while let Some(line) = match read_trimmed_line(&mut DeadlineBufRead::new(
        &mut reader,
        deadline_socket.as_ref(),
        &deadline,
    )) {
        Ok(line) => line,
        // upstream: io.c:147-153 - the deadline is consulted at the wait, and an
        // elapsed one DIAGNOSES then exits RERR_TIMEOUT. The read error itself
        // is only the messenger: both the guard's own refusal and SO_RCVTIMEO
        // surface as TimedOut, so the deadline - not the errno - decides.
        Err(error) if deadline.expired() => {
            if let Some(log) = log_sink.as_ref() {
                let message = rsync_error!(30, handshake_timeout_message("rsyncd"))
                    .with_role(Role::Daemon);
                log_message(log, &message);
            }
            let _ = error;
            // FSM: -> Closing on the expired handshake deadline.
            return Ok(end_session(conn_state, Some(ExitCode::Timeout)));
        }
        Err(error) => return Err(error),
    } {
        // upstream: clientserver.c:180-213 exchange_protocols() (am_client == 0) -
        // the daemon validates the client's version greeting before proceeding,
        // refusing a line that is not a banner at all, or one that omits the
        // subprotocol value (protocol >= 30) or the digest name list
        // (protocol > 31). `reject_malformed_client_greeting` owns which of those
        // applies; the refusal is a fatal pre-OK @ERROR line, after which the
        // client returns and the socket closes.
        if negotiated_protocol.is_none()
            && let Some(error) = reject_malformed_client_greeting(&line)
        {
            write_limited(reader.get_mut(), &mut limiter, error.line().as_bytes())?;
            write_limited(reader.get_mut(), &mut limiter, b"\n")?;
            reader.get_mut().flush()?;
            // FSM: -> Closing after the fatal @ERROR refusal.
            return Ok(end_session(conn_state, None));
        }
        match parse_legacy_daemon_message(&line) {
            // upstream: clientserver.c:1534-1538 start_daemon() - the version
            // line is read exactly once, by exchange_protocols(); the very next
            // read_line_old() is the request line whatever it contains. A
            // second `@RSYNCD:` banner is therefore a module name, not a
            // re-greeting, and falls through to the unknown-module refusal.
            // Taking the Greeting -> ModuleSelect edge again instead would fail
            // the FSM's ordering check, and a session error is fatal to the
            // whole listener (workers.rs join_worker), so a peer could end the
            // daemon with two greeting lines.
            Ok(LegacyDaemonMessage::Version(version)) if negotiated_protocol.is_none() => {
                // upstream: clientserver.c:199-203 exchange_protocols() -
                // `daemon_auth_choices = strchr(buf + 9, ' ')` keeps the client's
                // digest name list for negotiate_daemon_auth(). The Version
                // variant carries only the version, so re-parse the raw line for
                // the rest of it. `None` here is upstream's NULL and `Some("")`
                // its non-NULL empty `strdup`; the two negotiate differently, so
                // the empty case must survive the round trip through `String`.
                client_digests =
                    parse_legacy_daemon_greeting_details(&line)
                        .ok()
                        .and_then(|greeting| {
                            greeting.advertised_digests().names().map(ToOwned::to_owned)
                        });
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
            // A banner arriving after the version exchange is the request line.
            Ok(LegacyDaemonMessage::Version(_)) => {}
            Ok(LegacyDaemonMessage::Other(payload)) => {
                if let Some(option) = parse_daemon_option(payload) {
                    refused_options.push(option.to_owned());
                    continue;
                }
            }
            Ok(LegacyDaemonMessage::Exit) => {
                // FSM: -> Closing on client-initiated exit.
                return Ok(end_session(conn_state, None));
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

        // upstream: clientserver.c:1407-1418 - the daemon checks if the first
        // non-@RSYNCD line is `#early_input=<len>`. If so, it reads <len> bytes
        // of raw data and then reads the next line as the module name.
        if let Some(data) = read_early_input(&line, &mut reader)? {
            early_input_data = Some(data);
            continue;
        }

        request = Some(line);
        break;
    }

    // upstream: clientserver.c:819, 1169 - `set_daemon_handshake_timeout(0)`
    // DISARMS the deadline once the peer-driven phase ends, so local setup,
    // operator hooks and the transfer itself are never measured against it.
    // oc's analogue is clearing the socket timeout the clamp left behind: the
    // deadline object goes out of scope here, but `SO_RCVTIMEO` would persist
    // on the fd and bound every later read.
    if let Some(socket) = deadline_socket.as_ref() {
        let _ = socket.set_read_timeout(None);
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
            log_list_request(
                log,
                peer_host_display(peer_host.as_deref(), reverse_lookup),
                peer_addr,
            );
        }
        respond_with_module_list(reader.get_mut(), &mut limiter, modules, messages)?;
    } else if request.starts_with('#') {
        // upstream: clientserver.c:1427-1431 - `if (*line == '#') { io_printf(
        // f_out, "@ERROR: Unknown command '%s'\n", line); return -1; }`. A
        // `#`-prefixed request that is neither `#list` (handled above) nor the
        // already-consumed `#early_input=` command is a command the daemon does
        // not recognize. It is rejected with the unknown-command error - keeping
        // the raw line including the leading `#` - which is distinct from the
        // unknown-module response reserved for a bad module name. The client
        // treats `@ERROR` as fatal and closes without reading further.
        let error = AtError::UnknownCommand(sanitize_module_identifier(&request).into_owned());
        send_error(reader.get_mut(), &mut limiter, &error)?;
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
            AdvertisedDigests::from(client_digests.as_deref()),
            &mut session_exit_code,
            early_input_data,
            conn_state,
        )?;
    }

    // FSM: -> Closing. Every branch above has finished with the client: the
    // module list was sent, the unknown command was refused, or the module
    // request ran to completion.
    Ok(end_session(conn_state, session_exit_code))
}

/// Checks whether `line` is an `#early_input=<len>` command and, if so, reads
/// the specified number of raw bytes from the stream.
///
/// Returns `Ok(Some(data))` when the early-input command was recognized and the
/// data was read successfully, `Ok(None)` when the line is not an early-input
/// command, or an I/O error if reading fails or the length is invalid.
///
/// upstream: clientserver.c:1407-1414 - `rsync_module()` reads early input data
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
    log_sink: Option<SharedLogSink>,
) -> io::Result<()> {
    handle_binary_session_internal(stream, daemon_limit, log_sink)
}

fn handle_binary_session_internal(
    mut stream: DaemonStream,
    daemon_limit: Option<NonZeroU64>,
    log_sink: Option<SharedLogSink>,
) -> io::Result<()> {
    let mut limiter = BandwidthLimitComponents::new(daemon_limit).into_limiter();

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
    // upstream: io.c:send_msg_int — SIVAL is little-endian
    let exit_code = u32::try_from(FEATURE_UNAVAILABLE_EXIT_CODE).unwrap_or_default();
    MessageFrame::new(MessageCode::ErrorExit, exit_code.to_le_bytes().to_vec())?
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
            log_sink: None,
            reverse_lookup: false,
            proxy_policy: ProxyProtocolPolicy::Disabled,
            daemon_timeout: None,
        };
        assert!(params.modules.is_empty());
        assert!(params.motd_lines.is_empty());
        assert!(params.daemon_limit.is_none());
        assert!(!params.reverse_lookup);
        assert!(matches!(params.proxy_policy, ProxyProtocolPolicy::Disabled));
    }

    #[test]
    fn session_params_with_limits() {
        let modules: Vec<ModuleRuntime> = vec![];
        let motd_lines: Vec<String> = vec![];
        let limit = NonZeroU64::new(1000);
        let params = SessionParams {
            modules: &modules,
            motd_lines: &motd_lines,
            daemon_limit: limit,
            log_sink: None,
            reverse_lookup: true,
            proxy_policy: ProxyProtocolPolicy::Disabled,
            daemon_timeout: None,
        };
        assert_eq!(params.daemon_limit, NonZeroU64::new(1000));
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
            log_sink: None,
            peer_host: None,
            reverse_lookup: false,
            daemon_timeout: None,
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
            log_sink: None,
            peer_host: Some("example.com".to_owned()),
            reverse_lookup: true,
            daemon_timeout: None,
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
        let state = state.close();
        assert!(state.is_terminal());
    }

    #[test]
    fn fsm_full_lifecycle_without_auth() {
        let mut state = ConnectionState::Greeting;
        state = state.transition(ConnectionState::ModuleSelect).unwrap();
        state = state.transition(ConnectionState::Transferring).unwrap();
        state = state.close();
        assert!(state.is_terminal());
    }

    #[test]
    fn fsm_full_lifecycle_with_auth() {
        let mut state = ConnectionState::Greeting;
        state = state.transition(ConnectionState::ModuleSelect).unwrap();
        state = state.transition(ConnectionState::Authenticating).unwrap();
        state = state.transition(ConnectionState::Transferring).unwrap();
        state = state.close();
        assert!(state.is_terminal());
    }

    #[test]
    fn fsm_early_close_from_module_select() {
        let mut state = ConnectionState::Greeting;
        state = state.transition(ConnectionState::ModuleSelect).unwrap();
        state = state.close();
        assert!(state.is_terminal());
    }

    #[test]
    fn fsm_auth_failure_transitions_to_closing() {
        let mut state = ConnectionState::Greeting;
        state = state.transition(ConnectionState::ModuleSelect).unwrap();
        state = state.transition(ConnectionState::Authenticating).unwrap();
        state = state.close();
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

    /// Teardown is not a progression, so the handler cannot reach `Closing`
    /// through `transition` at all - `end_session` owns the edge.
    #[test]
    fn fsm_closing_is_not_a_transition_target() {
        let state = ConnectionState::ModuleSelect;
        assert!(state.transition(ConnectionState::Closing).is_err());
    }

    /// `end_session` forwards the session exit code untouched: the teardown
    /// edge must not swallow the code the caller has to surface as the process
    /// exit status in the stdio and inetd modes.
    #[test]
    fn end_session_forwards_exit_code() {
        assert_eq!(end_session(ConnectionState::ModuleSelect, None), None);
        assert_eq!(
            end_session(
                ConnectionState::Authenticating,
                Some(UNSUPPORTED_AUTH_DIGEST_EXIT_CODE)
            ),
            Some(UNSUPPORTED_AUTH_DIGEST_EXIT_CODE)
        );
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
