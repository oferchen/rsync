// Module request handling - context, error responses, and main entry point.
//
// When the client sends a module name (anything other than `#list`), the
// daemon looks up the module definition, checks host-based access control,
// acquires a connection slot, authenticates the user (if required), reads
// client arguments, and delegates to the transfer engine.
//
// upstream: `clientserver.c` - `rsync_module()` is the dispatcher that handles
// module lookup, access control, authentication, and transfer setup.

/// Maps an [`InvalidTransition`] to an [`io::Error`] for protocol-level reporting.
fn transition_error(err: InvalidTransition) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, err.to_string())
}

/// Context for module request handling, passed to helper functions.
///
/// Bundles the connection state (reader, bandwidth limiter, peer address)
/// with the module request metadata so helper functions receive a single
/// context parameter instead of many individual arguments.
struct ModuleRequestContext<'a> {
    reader: &'a mut BufReader<DaemonStream>,
    limiter: &'a mut Option<BandwidthLimiter>,
    peer_ip: IpAddr,
    session_peer_host: Option<&'a str>,
    module_peer_host: Option<&'a str>,
    /// Whether a reverse lookup was in scope for this session (global default
    /// OR the module override), which selects between upstream's two
    /// no-name sentinels. upstream: clientserver.c:768 `lp_reverse_lookup(i)`.
    reverse_lookup: bool,
    request: &'a str,
    log_sink: Option<&'a SharedLogSink>,
    messages: &'a LegacyMessageCache,
    /// Early-input data sent by the client before the module name.
    ///
    /// upstream: clientserver.c:583-584 - the daemon writes `early_input` to
    /// the pre-xfer exec script's stdin.
    early_input_data: Option<Vec<u8>>,
    /// Digest name list the client advertised in its `@RSYNCD:` greeting.
    ///
    /// upstream: clientserver.c:199-211 - `exchange_protocols()` stores it in the
    /// `daemon_auth_choices` global, which `negotiate_daemon_auth()` consumes at
    /// authentication time (compat.c:857). An advertised-but-empty list is a
    /// different state from no list at all, so the distinction is carried in the
    /// type rather than in an `Option<&str>`.
    client_digests: AdvertisedDigests<'a>,
    /// Set when the session must end the way upstream's forked child would
    /// `exit_cleanup()`, carrying that exit code.
    ///
    /// The listening parent is never torn down by it: only the single-session
    /// modes (stdio, inetd), where the process *is* the connection, surface it as
    /// the process exit status.
    session_exit_code: &'a mut Option<ExitCode>,
    /// Typed FSM state tracking the connection lifecycle phase.
    ///
    /// Every phase transition goes through `ConnectionState::transition()`,
    /// which rejects invalid progressions. The field is the single source of
    /// truth for which protocol phase the connection is in.
    conn_state: ConnectionState,
}

impl<'a> ModuleRequestContext<'a> {
    /// Returns the peer host string that every log escape, daemon log line and
    /// `RSYNC_HOST_NAME` renders, in upstream's shape: the resolved name, else
    /// `UNKNOWN` when a lookup was attempted, else `UNDETERMINED`.
    ///
    /// The module-level name wins over the session-level one because upstream
    /// re-resolves per module when the session left the host undetermined
    /// (clientserver.c:768-769).
    fn host_display(&self) -> &str {
        peer_host_display(
            self.module_peer_host.or(self.session_peer_host),
            self.reverse_lookup,
        )
    }
}

/// Which refusal wrote the error, selecting how long the daemon holds the
/// connection open before closing it.
///
/// The pause is load-bearing, not cosmetic. Closing a socket that still holds
/// unread peer bytes makes the kernel discard the pending send queue and answer
/// with RST, so the error just written never arrives: the client reports
/// `Connection reset by peer` and the refusal looks like it never happened.
/// Whether the peer's next write landed before the close decides it, which is
/// why the symptom is intermittent.
///
/// Two upstream sites stack, and each arm below names the ones that apply:
/// - `cleanup.c:263-264` - every server exiting non-zero sleeps 100 ms before
///   `close_all()`.
/// - `clientserver.c:1266` - a post-`@RSYNCD: OK` error waits a further 400 ms
///   before `exit_cleanup(RERR_UNSUPPORTED)`, because the client must also
///   drain the multiplexed frames.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RefusalPhase {
    /// Refused before `@RSYNCD: OK`; the client is still reading raw text and
    /// reaches upstream's exit linger by returning -1 out of `rsync_module()`.
    PreHandshake,
    /// Refused after `@RSYNCD: OK`; upstream stacks
    /// [`Self::POST_HANDSHAKE_EXTRA`] on top of the server-exit wait.
    PostHandshake,
}

impl RefusalPhase {
    /// upstream: `cleanup.c:263-264` - `if (am_server && exit_code) msleep(100)`.
    const SERVER_EXIT: Duration = Duration::from_millis(100);
    /// upstream: `clientserver.c:1266` - `msleep(400)` before `exit_cleanup()`.
    const POST_HANDSHAKE_EXTRA: Duration = Duration::from_millis(400);

    const fn linger(self) -> Duration {
        match self {
            Self::PreHandshake => Self::SERVER_EXIT,
            Self::PostHandshake => Self::SERVER_EXIT.saturating_add(Self::POST_HANDSHAKE_EXTRA),
        }
    }
}

/// Holds a refused connection open long enough for the peer to read the error
/// the daemon just wrote. See [`RefusalPhase`] for why the wait exists and
/// which upstream sites set each duration.
fn linger_before_close(phase: RefusalPhase) {
    thread::sleep(phase.linger());
}

/// Sends a fatal `@ERROR:` refusal line to the client and closes the session.
///
/// Writes exactly `<payload>\n` and nothing more. Upstream never follows an
/// `@ERROR:` line with `@RSYNCD: EXIT`: the client treats `@ERROR` as fatal
/// and returns immediately without reading further
/// (upstream: clientserver.c:424-428 - `strncmp(line, "@ERROR", 6) == 0` ->
/// `return -1`), so the server just emits the line and lets the socket close.
/// Only the successful module-listing path emits `@RSYNCD: EXIT`.
///
/// This is the pre-handshake form: the daemon has not yet emitted
/// `@RSYNCD: OK` so the client still reads raw `@RSYNCD:`-style text.
/// After OK is sent the client switches its input to the multiplex stream
/// and any further raw bytes are mis-parsed as multiplex frame headers
/// (e.g. the 'T' of "The server..." surfaces as `unexpected tag 77`).
/// Use [`send_multiplexed_error_and_exit`] once OK has been written.
fn send_error(
    stream: &mut DaemonStream,
    limiter: &mut Option<BandwidthLimiter>,
    error: &AtError,
) -> io::Result<()> {
    write_limited(stream, limiter, error.line().as_bytes())?;
    write_limited(stream, limiter, b"\n")?;
    stream.flush()?;
    linger_before_close(RefusalPhase::PreHandshake);
    Ok(())
}

/// Sends a fatal error to the client through the multiplex stream and
/// terminates the session.
///
/// upstream: clientserver.c:1175-1186 - once `io_start_multiplex_out()`
/// has run (which happens around the `@RSYNCD: OK` exchange) the daemon
/// emits errors via `rwrite(FERROR, ...)`, encoded as a `MSG_ERROR_XFER`
/// frame, followed by `MSG_ERROR_EXIT` to synchronize the error exit.
/// Raw `@ERROR: ...\n` text written here would be decoded by the client's
/// `read_a_msg()` as a multiplex frame header and surface as
/// "unexpected tag 77" (the byte 'T' from "The server is configured ..."
/// minus `MPLEX_BASE = 7`).
fn send_multiplexed_error_and_exit(
    stream: &mut DaemonStream,
    limiter: &mut Option<BandwidthLimiter>,
    payload: &str,
    exit_code: i32,
) -> io::Result<()> {
    let mut frame_bytes = payload.as_bytes().to_vec();
    frame_bytes.push(b'\n');
    let mut buffer = Vec::new();
    MessageFrame::new(MessageCode::ErrorXfer, frame_bytes)?.encode_into_writer(&mut buffer)?;
    // upstream: io.c:1060 send_msg_int() - little-endian 4-byte exit code.
    MessageFrame::new(MessageCode::ErrorExit, exit_code.to_le_bytes().to_vec())?
        .encode_into_writer(&mut buffer)?;
    write_limited(stream, limiter, &buffer)?;
    stream.flush()?;
    linger_before_close(RefusalPhase::PostHandshake);
    Ok(())
}

/// Sends an access denied response to the client and closes the session.
///
/// When the module has `list = false`, the daemon hides the module's existence
/// by sending `@ERROR: Unknown module` instead of the real access denied
/// message. This prevents unauthenticated clients from probing which hidden
/// modules exist.
///
/// upstream: clientserver.c:729-735 - when `!lp_list(i)`, sends
/// `@ERROR: Unknown module '%s'`; otherwise sends
/// `@ERROR: access denied to %s from %s (%s)`.
fn deny_module(
    stream: &mut DaemonStream,
    module: &ModuleDefinition,
    peer_ip: IpAddr,
    host: &str,
    limiter: &mut Option<BandwidthLimiter>,
) -> io::Result<()> {
    let module_display = sanitize_module_identifier(&module.name);
    let error = if !module.listable {
        // upstream: clientserver.c:730 - hide module existence for non-listable modules.
        AtError::UnknownModule(module_display.into_owned())
    } else {
        AtError::AccessDenied {
            module: module_display.into_owned(),
            host: host.to_owned(),
            addr: peer_ip.to_string(),
        }
    };
    send_error(stream, limiter, &error)
}

/// Sends the "@RSYNCD: OK" acknowledgment to the client.
///
/// This confirms that the module request was accepted and the client
/// may proceed with sending its arguments.
fn send_daemon_ok(
    stream: &mut DaemonStream,
    limiter: &mut Option<BandwidthLimiter>,
    messages: &LegacyMessageCache,
) -> io::Result<()> {
    messages.write_ok(stream, limiter)?;
    stream.flush()
}

/// Handles max connections exceeded for a module.
///
/// Sends an error message indicating the connection limit was reached and logs
/// the event. `limit` is the configured number rendered verbatim, so a module
/// disabled with a negative `max connections` keeps its minus sign, matching
/// upstream's `%d` in clientserver.c:746-757.
fn handle_max_connections_exceeded(
    ctx: &mut ModuleRequestContext<'_>,
    module: &ModuleRuntime,
    limit: i32,
) -> io::Result<()> {
    send_error(
        ctx.reader.get_mut(),
        ctx.limiter,
        &AtError::MaxConnections {
            limit: i64::from(limit),
        },
    )?;
    if let Some(log) = ctx.log_sink {
        let current = module
            .active_connections
            .load(std::sync::atomic::Ordering::Acquire);
        log_module_limit(
            log,
            ctx.host_display(),
            ctx.peer_ip,
            ctx.request,
            limit,
            current,
        );
    }
    Ok(())
}

/// Handles lock file errors when acquiring a module connection.
///
/// Sends an error message and logs the lock failure.
fn handle_lock_error(ctx: &mut ModuleRequestContext<'_>, error: &io::Error) -> io::Result<()> {
    send_error(ctx.reader.get_mut(), ctx.limiter, &AtError::ModuleLockError)?;
    if let Some(log) = ctx.log_sink {
        log_module_lock_error(log, ctx.host_display(), ctx.peer_ip, ctx.request, error);
    }
    Ok(())
}

/// Handles refused options for a module (pre-handshake path).
///
/// Sends an error message indicating the option is refused and logs the event.
/// Used when refused-options are detected from `OPTION` directives sent before
/// `@RSYNCD: OK`; at this point the client still reads raw text.
fn handle_refused_option(ctx: &mut ModuleRequestContext<'_>, refused: &str) -> io::Result<()> {
    let error = AtError::message(refused_option_message(refused));
    send_error(ctx.reader.get_mut(), ctx.limiter, &error)?;
    if let Some(log) = ctx.log_sink {
        log_module_refused_option(log, refused);
    }
    Ok(())
}

/// Handles refused options for a module after `@RSYNCD: OK` has been emitted.
///
/// upstream: clientserver.c:1146-1186 - once the daemon has acknowledged the
/// module, errors detected after `read_args()` (including refused options
/// matched against the real argv in `parse_arguments()`) must be delivered
/// through the multiplexed stream. Upstream funnels them through the very
/// same code path that wraps any other post-OK error: `setup_protocol()`
/// finishes the protocol negotiation that the client also runs, then
/// `io_start_multiplex_out()` flips the writer to framed mode, and finally
/// `rwrite(FERROR, ...)` emits the message as a `MSG_ERROR_XFER` frame
/// followed by `MSG_ERROR_EXIT`.
///
/// Skipping the post-OK protocol-setup writes left the client reading the
/// raw error-frame bytes as the unidirectional compat-flags varint and the
/// checksum seed. The framing then resynchronised partway into our
/// `MSG_ERROR_XFER` payload, decoding the first body byte as a fresh
/// multiplex tag. With `@ERROR: ...` payloads that lands on the letter `A`
/// (ASCII 65) once the header bytes have been consumed, surfacing as
/// `unexpected tag 72` (65 + `MPLEX_BASE`) on the receiver. Raw
/// `@ERROR: ...\n` text would similarly produce `unexpected tag 77` (the
/// byte `T` from "The server ..." minus `MPLEX_BASE = 7`).
///
/// The `MSG_ERROR_EXIT` payload carries `RERR_UNSUPPORTED`, not
/// `RERR_SYNTAX`. upstream: clientserver.c:1254-1268 - the single post-OK
/// fatal branch (`if (!ret || err_msg)`) covers both a refused option and a
/// failed `pre-xfer exec`, and it ends in `exit_cleanup(RERR_UNSUPPORTED)`
/// at clientserver.c:1267. That is the code the peer observes; the sibling
/// read-only/write-only rejection is a different decision made later in
/// `do_server_recv()`/`do_server_sender()` and keeps `RERR_SYNTAX`.
fn handle_refused_option_post_handshake(
    ctx: &mut ModuleRequestContext<'_>,
    refused: &str,
    protocol_version: Option<ProtocolVersion>,
    client_args: &[String],
) -> io::Result<()> {
    finalize_post_ok_protocol_for_error(ctx, protocol_version, client_args)?;
    let error = AtError::message(refused_option_message(refused));
    send_multiplexed_error_and_exit(
        ctx.reader.get_mut(),
        ctx.limiter,
        &error.line(),
        RERR_UNSUPPORTED_EXIT_CODE,
    )?;
    if let Some(log) = ctx.log_sink {
        log_module_refused_option(log, refused);
    }
    Ok(())
}

/// Rejects a post-`@RSYNCD: OK` access-mode violation - a push to a
/// `read only` module or a pull from a `write only` module - through the
/// multiplexed error path.
///
/// upstream: main.c:1183-1187 `do_server_recv()` rejects a read-only push
/// with `rprintf(FERROR, "ERROR: module is read only\n")` then
/// `exit_cleanup(RERR_SYNTAX)`; main.c:949-952 `do_server_sender()` rejects a
/// write-only pull the same way. Both fire after `setup_protocol()` and
/// `io_start_multiplex_out()`, so the message travels as a `MSG_ERROR_XFER`
/// frame followed by `MSG_ERROR_EXIT`. Emitting the raw text with
/// [`send_error`] instead desynchronises the client's multiplex decoder
/// (issue #227): the client reads the plaintext line as a 4-byte frame
/// header and aborts with `invalid multi-message 102 (code 12)` rather than
/// the clean `ERROR: module is read only` + exit 1 upstream produces.
fn handle_access_denied_post_handshake(
    ctx: &mut ModuleRequestContext<'_>,
    payload: &str,
    protocol_version: Option<ProtocolVersion>,
    client_args: &[String],
) -> io::Result<()> {
    finalize_post_ok_protocol_for_error(ctx, protocol_version, client_args)?;
    send_multiplexed_error_and_exit(
        ctx.reader.get_mut(),
        ctx.limiter,
        payload,
        RERR_SYNTAX_EXIT_CODE,
    )
}

/// Rejects a post-`@RSYNCD: OK` client-argument parse failure through the
/// multiplexed error path.
///
/// upstream: clientserver.c:1258-1266 - when `parse_arguments()` returns 0 (or
/// leaves a deferred `err_msg`), `rsync_module()` runs `option_error()`, which
/// emits the parser's own `err_buf` text over `FERROR`
/// (options.c:915 `rprintf(FERROR, RSYNC_NAME ": %s", err_buf)`), and then
/// `exit_cleanup(RERR_UNSUPPORTED)`. Both writes happen *after* the
/// `setup_protocol()` + `io_start_multiplex_out()` pair at
/// clientserver.c:1229-1251, which upstream performs precisely so "we can get
/// the error back to the client".
///
/// Delivering this rejection with [`send_error`] instead put raw `@ERROR:`
/// text on a stream the client had already switched to multiplex input,
/// leaving it to decode the plaintext as a frame header: the daemon's
/// diagnostic was replaced on the client by
/// `unknown multiplexed message code 38` (the `-` of `--max-size` at the tag
/// position, minus `MPLEX_BASE = 7`), so the operator never learned which
/// option was rejected.
fn handle_client_arg_rejection_post_handshake(
    ctx: &mut ModuleRequestContext<'_>,
    payload: &str,
    protocol_version: Option<ProtocolVersion>,
    client_args: &[String],
) -> io::Result<()> {
    finalize_post_ok_protocol_for_error(ctx, protocol_version, client_args)?;
    send_multiplexed_error_and_exit(
        ctx.reader.get_mut(),
        ctx.limiter,
        payload,
        RERR_UNSUPPORTED_EXIT_CODE,
    )
}

/// Mirrors the prefix of upstream's `setup_protocol(f_out, f_in)` that the
/// daemon writes before turning on `io_start_multiplex_out()` for an error
/// it needs to deliver after `@RSYNCD: OK`.
///
/// upstream: clientserver.c:1146-1170 - when `read_args()` succeeds but
/// `parse_arguments()` rejects an option (or any other post-OK fatal path),
/// the daemon still has to finish the protocol setup that the client also
/// runs unconditionally. Without it the client decodes the daemon's first
/// error-frame bytes as the compat-flags varint and the checksum seed,
/// then resynchronises somewhere inside the error payload and rejects
/// whatever ASCII byte landed at the tag position.
///
/// In the daemon flow the post-`OK` protocol setup at protocol >= 30 is
/// effectively unidirectional: the server writes compat flags and the
/// checksum seed, and the client reads them silently before activating
/// `io_start_multiplex_in()`. The bidirectional capability-string
/// negotiation that lives at `compat.c:535-565` only fires when the
/// negotiator decides to advertise variable-strings; for the refused-
/// options abort path we never reach the transfer so we mirror upstream's
/// shortest viable prefix - compat flags plus the checksum seed - and
/// leave the negotiated-string exchange off the wire. That matches what
/// the client expects from `setup_protocol` before it switches to
/// multiplex input, keeping the framing aligned with upstream rsync.
fn finalize_post_ok_protocol_for_error(
    ctx: &mut ModuleRequestContext<'_>,
    protocol_version: Option<ProtocolVersion>,
    client_args: &[String],
) -> io::Result<()> {
    let protocol = match protocol_version {
        Some(p) => p,
        None => return Ok(()),
    };
    let stream = ctx.reader.get_mut();
    if protocol.uses_binary_negotiation() {
        // upstream: compat.c:711-744 - server writes compat_flags as a
        // varint (or as a single byte when the client advertised the
        // pre-release `V` capability). Parse the client's `-e` capability
        // string out of the post-OK argv so the advertised flags match the
        // peer's capability bits exactly.
        let client_info = parse_client_capability_info(client_args);
        let compat_flags = build_default_post_ok_compat_flags(&client_info);
        if has_pre_release_capability(&client_info, 'V') {
            // upstream: compat.c:737-740 - pre-release 'V' clients encode
            // the compat-flags byte directly rather than as a varint.
            // The flags we advertise here intentionally omit
            // `VARINT_FLIST_FLAGS`, even though upstream would have OR'd
            // it in, so the client's `do_negotiated_strings` stays clear
            // and skips the bidirectional vstring exchange we have no
            // peer for on the abort path.
            write_limited(stream, ctx.limiter, &[compat_flags.bits() as u8])?;
        } else {
            let mut buf = Vec::with_capacity(2);
            protocol::write_varint(&mut buf, compat_flags.bits() as i32)?;
            write_limited(stream, ctx.limiter, &buf)?;
        }
    }
    // upstream: compat.c:811-814 - server writes a 4-byte little-endian
    // checksum seed regardless of protocol version. The value is unused
    // because we abort before any transfer, but the slot has to be filled
    // so the client's matching `read_int()` consumes the same bytes
    // upstream would have written.
    let mut seed_buf = Vec::with_capacity(4);
    protocol::write_int(&mut seed_buf, 0)?;
    write_limited(stream, ctx.limiter, &seed_buf)?;
    stream.flush()
}

/// Extracts the `-e<info>` capability string the client emitted in its
/// post-OK argv, matching upstream `compat.c:163-179`'s `client_info`.
///
/// The capability characters live after the literal `.` separator in
/// arguments such as `-vlogDtprez.iLsfxCIvu` or `-e.LsfxCIvu`. When the
/// client sent no `-e` payload, return an empty string so the compat
/// builder falls back to the defaults.
fn parse_client_capability_info(client_args: &[String]) -> String {
    for arg in client_args {
        if let Some(rest) = arg.strip_prefix('-') {
            if rest.starts_with('-') {
                continue;
            }
            if let Some(dot_pos) = rest.find('.') {
                let before_dot = &rest[..dot_pos];
                if before_dot.ends_with('e') || before_dot.is_empty() {
                    return rest[dot_pos + 1..].to_owned();
                }
            }
        }
    }
    String::new()
}

/// Reports whether the client's capability string contains the pre-release
/// `V` letter that upstream uses to gate the single-byte compat-flags
/// encoding.
///
/// Matches upstream `compat.c:745` - `strchr(client_info, 'V') != NULL`.
fn has_pre_release_capability(client_info: &str, letter: char) -> bool {
    client_info.contains(letter)
}

/// Builds the compat flags the daemon advertises when aborting a post-`OK`
/// session, restricted to the subset that does *not* trigger upstream's
/// variable-string negotiation step.
///
/// upstream: compat.c:535-565 - `negotiate_the_strings()` is gated on
/// `do_negotiated_strings`, which the client only sets when the server's
/// compat flags carry `CF_VARINT_FLIST_FLAGS`. Refused-options aborts
/// never reach the file-list phase, so we deliberately *do not* advertise
/// `VARINT_FLIST_FLAGS` here even when the client's capability string
/// would otherwise enable it. Skipping the flag keeps the client out of
/// `recv_negotiate_str()` / `read_vstring()` reads that our writer would
/// never satisfy.
///
/// The remaining flags are the platform-default subset upstream picks up
/// from `compat.c:712-732` for any session that completes `setup_protocol()`
/// without engaging in vstring negotiation: safe flist handling, the
/// xattr-optimisation guard, the inplace partial-dir hint, ID-0 names,
/// the corrected checksum-seed ordering, and incremental recursion only
/// when the client advertised `i`. Anything beyond this prefix is
/// unobservable because the connection terminates before file-list
/// exchange begins.
fn build_default_post_ok_compat_flags(client_info: &str) -> protocol::CompatibilityFlags {
    let mut flags = protocol::CompatibilityFlags::CHECKSUM_SEED_FIX
        | protocol::CompatibilityFlags::SAFE_FILE_LIST
        | protocol::CompatibilityFlags::AVOID_XATTR_OPTIMIZATION
        | protocol::CompatibilityFlags::INPLACE_PARTIAL_DIR
        | protocol::CompatibilityFlags::ID0_NAMES;
    #[cfg(unix)]
    {
        flags |= protocol::CompatibilityFlags::SYMLINK_TIMES;
    }
    if client_info.contains('i') {
        flags |= protocol::CompatibilityFlags::INC_RECURSE;
    }
    flags
}

/// Handles module authentication flow with FSM transition enforcement.
///
/// On success returns `Some((username, access_level))` where `username` is the
/// authenticated user (or `None` when auth was not required) and `access_level`
/// is the per-user `auth users` override applied to the session's `read only`.
/// Returns `Ok(None)` if authentication failed or was denied.
///
/// FSM transitions:
/// - When auth is required: ModuleSelect -> Authenticating -> (on grant) stays
///   until caller transitions to Transferring.
/// - When auth is not required: no Authenticating transition (skipped).
fn handle_authentication(
    ctx: &mut ModuleRequestContext<'_>,
    module: &ModuleDefinition,
    protocol_version: Option<ProtocolVersion>,
) -> io::Result<Option<(Option<String>, UserAccessLevel)>> {
    if !module.requires_authentication() {
        // `@RSYNCD: OK` is deferred to the caller: upstream emits it only after
        // chroot + privilege drop succeed (clientserver.c:1071), so those
        // failures stay raw pre-OK lines instead of desyncing the client.
        // upstream: authenticate.c:238-239 - an empty/absent `auth users` list
        // lets anyone in with no access-level override, so `read only` stays.
        return Ok(Some((None, UserAccessLevel::Default)));
    }

    // FSM: ModuleSelect -> Authenticating - module requires auth, challenge sent.
    ctx.conn_state = ctx
        .conn_state
        .transition(ConnectionState::Authenticating)
        .map_err(transition_error)?;

    match perform_module_authentication(
        ctx.reader,
        ctx.limiter,
        module,
        ctx.peer_ip,
        ctx.messages,
        protocol_version,
        ctx.client_digests,
    )? {
        AuthenticationStatus::DigestUnsupported => {
            // upstream: compat.c:875 - `exit_cleanup(RERR_UNSUPPORTED)` right
            // after the refusal line, with no challenge and no auth-failure log.
            *ctx.session_exit_code = Some(UNSUPPORTED_AUTH_DIGEST_EXIT_CODE);
            ctx.conn_state = ctx.conn_state.close();
            Ok(None)
        }
        AuthenticationStatus::Denied(denial) => {
            // upstream: authenticate.c:433 writes the FLOG auth-failure line
            // from inside auth_server(); only after it returns NULL does
            // rsync_module() emit `@ERROR: auth failed on module %s`
            // (clientserver.c:812). The log must therefore be on disk BEFORE
            // the peer can observe the refusal - a client that reads the error
            // and immediately inspects the daemon log has to find the reason
            // already there. Authentication deliberately does not write the
            // error itself, so this arm owns both halves in upstream's order.
            if let Some(log) = ctx.log_sink {
                log_module_auth_failure(
                    log,
                    ctx.host_display(),
                    ctx.peer_ip,
                    ctx.request,
                    denial.log_suffix().as_deref(),
                );
            }
            let error = AtError::AuthFailed {
                module: sanitize_module_identifier(&module.name).into_owned(),
            };
            send_error(ctx.reader.get_mut(), ctx.limiter, &error)?;
            // FSM: -> Closing on auth failure (session ends).
            ctx.conn_state = ctx.conn_state.close();
            Ok(None)
        }
        AuthenticationStatus::Granted {
            username,
            access_level,
        } => {
            // upstream: authenticate.c auth_server() logs nothing on success -
            // the granted access is observable via the earlier "rsync allowed
            // access on module" line (clientserver.c:742), so no auth-success
            // line is emitted here.
            // `@RSYNCD: OK` is deferred to the caller (see the no-auth path
            // above): it is emitted only after chroot + privilege drop succeed.
            Ok(Some((Some(username), access_level)))
        }
    }
}

/// Handles an unknown module request.
///
/// Sends an error message and logs the event.
fn handle_unknown_module(
    stream: &mut DaemonStream,
    limiter: &mut Option<BandwidthLimiter>,
    request: &str,
    peer_ip: IpAddr,
    session_peer_host: Option<&str>,
    reverse_lookup: bool,
    log_sink: Option<&SharedLogSink>,
) -> io::Result<()> {
    let error = AtError::UnknownModule(sanitize_module_identifier(request).into_owned());

    if let Some(log) = log_sink {
        log_unknown_module(
            log,
            peer_host_display(session_peer_host, reverse_lookup),
            peer_ip,
            request,
        );
    }

    send_error(stream, limiter, &error)
}

/// Handles a denied module access.
///
/// Sends an access denied error and logs the event.
fn handle_module_denied(
    ctx: &mut ModuleRequestContext<'_>,
    module: &ModuleDefinition,
) -> io::Result<()> {
    let host = ctx.host_display().to_owned();
    if let Some(log) = ctx.log_sink {
        log_module_denied(log, &host, ctx.peer_ip, ctx.request);
    }
    deny_module(
        ctx.reader.get_mut(),
        module,
        ctx.peer_ip,
        &host,
        ctx.limiter,
    )
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
    reader: &mut BufReader<DaemonStream>,
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
    client_digests: AdvertisedDigests<'_>,
    session_exit_code: &mut Option<ExitCode>,
    early_input_data: Option<Vec<u8>>,
    conn_state: ConnectionState,
) -> io::Result<()> {
    let Some(module) = modules.iter().find(|module| module.name == request) else {
        return handle_unknown_module(
            reader.get_mut(),
            limiter,
            request,
            peer_ip,
            session_peer_host,
            reverse_lookup,
            log_sink,
        );
    };

    // upstream: clientserver.c:897 `log_init(1)` reopens the daemon log to
    // `lp_log_file(module_id)` once the module is selected, so every subsequent
    // diagnostic for this connection lands in the module's `log file`. Keep the
    // reopened sink alive for the rest of the request; shadow `log_sink` so the
    // bandwidth-change log, refusal path, and transfer all use it.
    let module_log_sink = reopen_module_log_sink(module, log_sink);
    let log_sink = module_log_sink.as_ref().or(log_sink);

    let mut hostname_cache: Option<Option<String>> = None;
    // upstream: clientserver.c:1392 resolves the host when `lp_reverse_lookup(-1)`
    // (the global default = `reverse_lookup`) is set, then rsync_module (:723)
    // resolves it per-module when `lp_reverse_lookup(i)` is set. The effective
    // per-module value is therefore the global default OR the module override.
    let module_reverse_lookup = reverse_lookup || module.reverse_lookup;
    let module_peer_host =
        module_peer_hostname(module, &mut hostname_cache, peer_ip, module_reverse_lookup);

    let mut ctx = ModuleRequestContext {
        reader,
        limiter,
        peer_ip,
        session_peer_host,
        module_peer_host,
        reverse_lookup: module_reverse_lookup,
        request,
        log_sink,
        messages,
        early_input_data,
        client_digests,
        session_exit_code,
        conn_state,
    };

    // upstream: clientserver.c:773 `allow_access(addr, &host, i)` is handed the
    // same never-empty host string the log lines use, sentinels included - so a
    // `hosts allow = UNKNOWN` line works as clientname.c:93-95 documents.
    let access_host = PeerHost::new(module_peer_host, module_reverse_lookup);
    if !module.permits(peer_ip, access_host) {
        return handle_module_denied(&mut ctx, module);
    }

    process_approved_module(&mut ctx, module, options, negotiated_protocol)
}
