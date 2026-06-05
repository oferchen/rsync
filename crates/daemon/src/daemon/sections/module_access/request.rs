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
    request: &'a str,
    log_sink: Option<&'a SharedLogSink>,
    messages: &'a LegacyMessageCache,
    /// Early-input data sent by the client before the module name.
    ///
    /// upstream: clientserver.c:583-584 - the daemon writes `early_input` to
    /// the pre-xfer exec script's stdin.
    early_input_data: Option<Vec<u8>>,
    /// Typed FSM state tracking the connection lifecycle phase.
    ///
    /// Every phase transition goes through `ConnectionState::transition()`,
    /// which rejects invalid progressions. The field is the single source of
    /// truth for which protocol phase the connection is in.
    conn_state: ConnectionState,
}

impl<'a> ModuleRequestContext<'a> {
    /// Returns the effective host for logging (module-specific or session-level).
    fn effective_host(&self) -> Option<&str> {
        self.module_peer_host.or(self.session_peer_host)
    }
}

/// Sends an error message and exit marker to the client.
fn send_error_and_exit(
    stream: &mut DaemonStream,
    limiter: &mut Option<BandwidthLimiter>,
    messages: &LegacyMessageCache,
    payload: &str,
) -> io::Result<()> {
    write_limited(stream, limiter, payload.as_bytes())?;
    write_limited(stream, limiter, b"\n")?;
    messages.write_exit(stream, limiter)?;
    stream.flush()
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
    host: Option<&str>,
    limiter: &mut Option<BandwidthLimiter>,
    messages: &LegacyMessageCache,
) -> io::Result<()> {
    let module_display = sanitize_module_identifier(&module.name);
    let payload = if !module.listable {
        // upstream: clientserver.c:730 - hide module existence for non-listable modules.
        UNKNOWN_MODULE_PAYLOAD.replace("{module}", module_display.as_ref())
    } else {
        let addr_str = peer_ip.to_string();
        let host_display = host.unwrap_or(&addr_str);
        ACCESS_DENIED_PAYLOAD
            .replace("{module}", module_display.as_ref())
            .replace("{host}", host_display)
            .replace("{addr}", &addr_str)
    };
    send_error_and_exit(stream, limiter, messages, &payload)
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
/// Sends an error message indicating the connection limit was reached and logs the event.
fn handle_max_connections_exceeded(
    ctx: &mut ModuleRequestContext<'_>,
    module: &ModuleRuntime,
    limit: NonZeroU32,
) -> io::Result<()> {
    let payload = MODULE_MAX_CONNECTIONS_PAYLOAD.replace("{limit}", &limit.get().to_string());
    send_error_and_exit(ctx.reader.get_mut(), ctx.limiter, ctx.messages, &payload)?;
    if let Some(log) = ctx.log_sink {
        let current = module
            .active_connections
            .load(std::sync::atomic::Ordering::Acquire);
        log_module_limit(
            log,
            ctx.effective_host(),
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
    send_error_and_exit(
        ctx.reader.get_mut(),
        ctx.limiter,
        ctx.messages,
        MODULE_LOCK_ERROR_PAYLOAD,
    )?;
    if let Some(log) = ctx.log_sink {
        log_module_lock_error(log, ctx.effective_host(), ctx.peer_ip, ctx.request, error);
    }
    Ok(())
}

/// Handles refused options for a module.
///
/// Sends an error message indicating the option is refused and logs the event.
fn handle_refused_option(ctx: &mut ModuleRequestContext<'_>, refused: &str) -> io::Result<()> {
    let payload = format!("@ERROR: The server is configured to refuse {refused}");
    send_error_and_exit(ctx.reader.get_mut(), ctx.limiter, ctx.messages, &payload)?;
    if let Some(log) = ctx.log_sink {
        log_module_refused_option(log, ctx.effective_host(), ctx.peer_ip, ctx.request, refused);
    }
    Ok(())
}

/// Handles module authentication flow with FSM transition enforcement.
///
/// Returns `Some(username)` if authentication succeeded, where the username is
/// the authenticated user (or `None` inside `Some` when auth was not required).
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
) -> io::Result<Option<Option<String>>> {
    if !module.requires_authentication() {
        send_daemon_ok(ctx.reader.get_mut(), ctx.limiter, ctx.messages)?;
        return Ok(Some(None));
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
    )? {
        AuthenticationStatus::Denied => {
            if let Some(log) = ctx.log_sink {
                log_module_auth_failure(log, ctx.effective_host(), ctx.peer_ip, ctx.request);
            }
            // FSM: -> Closing on auth failure (session ends).
            ctx.conn_state = ctx
                .conn_state
                .transition(ConnectionState::Closing)
                .map_err(transition_error)?;
            Ok(None)
        }
        AuthenticationStatus::Granted(username) => {
            if let Some(log) = ctx.log_sink {
                log_module_auth_success(log, ctx.effective_host(), ctx.peer_ip, ctx.request);
            }
            send_daemon_ok(ctx.reader.get_mut(), ctx.limiter, ctx.messages)?;
            Ok(Some(Some(username)))
        }
    }
}

/// Handles an unknown module request.
///
/// Sends an error message and logs the event.
fn handle_unknown_module(
    stream: &mut DaemonStream,
    limiter: &mut Option<BandwidthLimiter>,
    messages: &LegacyMessageCache,
    request: &str,
    peer_ip: IpAddr,
    session_peer_host: Option<&str>,
    log_sink: Option<&SharedLogSink>,
) -> io::Result<()> {
    let module_display = sanitize_module_identifier(request);
    let payload = UNKNOWN_MODULE_PAYLOAD.replace("{module}", module_display.as_ref());

    if let Some(log) = log_sink {
        log_unknown_module(log, session_peer_host, peer_ip, request);
    }

    send_error_and_exit(stream, limiter, messages, &payload)
}

/// Handles a denied module access.
///
/// Sends an access denied error and logs the event.
fn handle_module_denied(
    ctx: &mut ModuleRequestContext<'_>,
    module: &ModuleDefinition,
) -> io::Result<()> {
    let host = ctx.module_peer_host.or(ctx.session_peer_host);
    if let Some(log) = ctx.log_sink {
        log_module_denied(log, host, ctx.peer_ip, ctx.request);
    }
    deny_module(
        ctx.reader.get_mut(),
        module,
        ctx.peer_ip,
        host,
        ctx.limiter,
        ctx.messages,
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
    early_input_data: Option<Vec<u8>>,
    conn_state: ConnectionState,
) -> io::Result<()> {
    let Some(module) = modules.iter().find(|module| module.name == request) else {
        return handle_unknown_module(
            reader.get_mut(),
            limiter,
            messages,
            request,
            peer_ip,
            session_peer_host,
            log_sink,
        );
    };

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

    let mut ctx = ModuleRequestContext {
        reader,
        limiter,
        peer_ip,
        session_peer_host,
        module_peer_host,
        request,
        log_sink,
        messages,
        early_input_data,
        conn_state,
    };

    if !module.permits(peer_ip, module_peer_host) {
        return handle_module_denied(&mut ctx, module);
    }

    process_approved_module(&mut ctx, module, options, negotiated_protocol)
}
