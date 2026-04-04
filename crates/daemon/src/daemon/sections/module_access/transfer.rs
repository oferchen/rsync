// Transfer execution - stream setup, handshake building, and transfer lifecycle.
//
// Handles the final phase of a module request: validating the module path,
// applying chroot and privilege restrictions, spawning the name converter,
// running pre/post-xfer exec hooks, and invoking the Rust transfer engine.
//
// upstream: clientserver.c - after `rsync_module()` completes authentication
// and argument parsing, it calls `chdir(lp_path())`, `chroot(".")`,
// `setgid()`/`setuid()`, and then enters the transfer pipeline.

/// Validates that the module path exists.
///
/// Returns `true` if the path exists, or sends an error and returns `false`.
fn validate_module_path(
    ctx: &mut ModuleRequestContext<'_>,
    module: &ModuleRuntime,
) -> io::Result<bool> {
    if Path::new(&module.path).exists() {
        return Ok(true);
    }

    let payload = format!(
        "@ERROR: module '{}' path does not exist: {}",
        sanitize_module_identifier(ctx.request),
        module.path.display()
    );
    send_error_and_exit(ctx.reader.get_mut(), ctx.limiter, ctx.messages, &payload)?;

    if let Some(log) = ctx.log_sink {
        let text = format!(
            "module '{}' path validation failed for {} ({}): path does not exist: {}",
            ctx.request,
            ctx.effective_host().unwrap_or("unknown"),
            ctx.peer_ip,
            module.path.display()
        );
        let message = rsync_error!(1, text).with_role(Role::Daemon);
        log_message(log, &message);
    }

    Ok(false)
}

/// Sets up the TCP streams for the transfer.
///
/// Configures TCP_NODELAY and clones the stream for concurrent read/write.
/// Returns the read and write streams on success, or sends an error and returns `None`.
fn setup_transfer_streams(
    ctx: &mut ModuleRequestContext<'_>,
) -> io::Result<Option<(TcpStream, TcpStream)>> {
    let stream = ctx.reader.get_mut();
    stream.set_nodelay(true)?;

    let read_stream = match stream.try_clone() {
        Ok(s) => s,
        Err(err) => {
            let payload = format!("@ERROR: failed to clone stream: {err}");
            send_error_and_exit(ctx.reader.get_mut(), ctx.limiter, ctx.messages, &payload)?;
            return Ok(None);
        }
    };

    let write_stream = match stream.try_clone() {
        Ok(s) => s,
        Err(err) => {
            return Err(io::Error::other(format!(
                "failed to clone write stream: {err}"
            )));
        }
    };

    Ok(Some((read_stream, write_stream)))
}

/// Builds the handshake result for the transfer.
fn build_handshake_result(
    reader: &BufReader<TcpStream>,
    negotiated_protocol: Option<ProtocolVersion>,
    client_args: Vec<String>,
    module: &ModuleRuntime,
) -> HandshakeResult {
    let final_protocol = negotiated_protocol.unwrap_or(ProtocolVersion::V30);
    let buffered_data = reader.buffer().to_vec();

    HandshakeResult {
        protocol: final_protocol,
        buffered: buffered_data,
        compat_exchanged: false,
        client_args: Some(client_args),
        io_timeout: module.timeout.map(|t| t.get()),
        negotiated_algorithms: None,
        compat_flags: None,
        checksum_seed: 0,
    }
}

/// Executes the server transfer and logs the result.
///
/// When the module has `transfer_logging` enabled and a log sink is available,
/// a per-transfer log line is emitted using the module's configured format
/// string (or `DEFAULT_LOG_FORMAT` as fallback).
///
/// Returns the transfer exit status: `0` on success, non-zero on failure.
fn execute_transfer(
    ctx: &ModuleRequestContext<'_>,
    config: ServerConfig,
    handshake: HandshakeResult,
    read_stream: &mut TcpStream,
    write_stream: &mut TcpStream,
    role: ServerRole,
    final_protocol: ProtocolVersion,
    module: &ModuleRuntime,
) -> i32 {
    if let Some(log) = ctx.log_sink {
        let text = format!(
            "module '{}' from {} ({}): protocol {}, role: {:?}",
            ctx.request,
            ctx.effective_host().unwrap_or("unknown"),
            ctx.peer_ip,
            final_protocol.as_u8(),
            role
        );
        let message = rsync_info!(text).with_role(Role::Daemon);
        log_message(log, &message);
    }

    // On Unix, wrap socket fds in io_uring RECV/SEND for batched syscalls.
    // Auto policy: uses io_uring on Linux 5.6+, falls back to standard I/O elsewhere.
    #[cfg(unix)]
    let result = {
        use std::os::unix::io::AsRawFd;
        let policy = fast_io::IoUringPolicy::Auto;
        let reader_res =
            fast_io::socket_reader_from_fd(read_stream.as_raw_fd(), 64 * 1024, policy);
        let writer_res =
            fast_io::socket_writer_from_fd(write_stream.as_raw_fd(), 64 * 1024, policy);
        if let (Ok(mut reader), Ok(mut writer)) = (reader_res, writer_res) {
            run_server_with_handshake(config, handshake, &mut reader, &mut writer, None, None, None)
        } else {
            run_server_with_handshake(config, handshake, read_stream, write_stream, None, None, None)
        }
    };
    #[cfg(not(unix))]
    let result = run_server_with_handshake(config, handshake, read_stream, write_stream, None, None, None);

    match result {
        Ok(_server_stats) => {
            if let Some(log) = ctx.log_sink {
                if module.transfer_logging {
                    let operation = match role {
                        ServerRole::Generator => TransferOperation::Send,
                        ServerRole::Receiver => TransferOperation::Recv,
                    };
                    let addr_str = ctx.peer_ip.to_string();
                    let path_str = module.path.display().to_string();
                    let pid = std::process::id();
                    let now = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default();
                    let secs = now.as_secs();
                    let timestamp = format_daemon_timestamp(secs);

                    let log_ctx = LogFormatContext {
                        operation,
                        hostname: ctx.effective_host().unwrap_or("unknown"),
                        remote_addr: &addr_str,
                        module_name: ctx.request,
                        username: "",
                        filename: "",
                        file_length: 0,
                        pid,
                        module_path: &path_str,
                        timestamp: &timestamp,
                        bytes_transferred: 0,
                        bytes_checksumed: 0,
                        itemize_string: "",
                    };

                    let fmt = effective_log_format(module);
                    log_transfer(fmt, &log_ctx, log);
                }

                let text = format!(
                    "transfer to {} ({}): module={} status=success",
                    ctx.effective_host().unwrap_or("unknown"),
                    ctx.peer_ip,
                    ctx.request
                );
                let message = rsync_info!(text).with_role(Role::Daemon);
                log_message(log, &message);
            }
            0
        }
        Err(err) => {
            if let Some(log) = ctx.log_sink {
                let text = format!(
                    "transfer failed to {} ({}): module={} error={}",
                    ctx.effective_host().unwrap_or("unknown"),
                    ctx.peer_ip,
                    ctx.request,
                    err
                );
                let message = rsync_error!(1, text).with_role(Role::Daemon);
                log_message(log, &message);
            }
            1
        }
    }
}

/// Processes an approved module request.
///
/// Handles the full transfer flow: connection acquisition, authentication,
/// reading client arguments, building configuration, and executing transfer.
fn process_approved_module(
    ctx: &mut ModuleRequestContext<'_>,
    module: &ModuleRuntime,
    options: &[String],
    negotiated_protocol: Option<ProtocolVersion>,
) -> io::Result<()> {
    // Acquire connection slot
    let _connection_guard = match module.try_acquire_connection() {
        Ok(guard) => guard,
        Err(ModuleConnectionError::Limit(limit)) => {
            return handle_max_connections_exceeded(ctx, limit);
        }
        Err(ModuleConnectionError::Io(error)) => {
            return handle_lock_error(ctx, &error);
        }
    };

    if let Some(log) = ctx.log_sink {
        log_module_request(log, ctx.effective_host(), ctx.peer_ip, ctx.request);
    }

    // Check for refused options
    if let Some(refused) = refused_option(module, options) {
        return handle_refused_option(ctx, refused);
    }

    // Apply client-sent daemon parameter overrides to a session-local copy
    // of the module definition. This avoids mutating the shared module state
    // while honouring per-connection --dparam values.
    // After overrides, expand %-variables (e.g. %MODULE%, %ADDR%) in path-type
    // fields using the connection's client address and hostname.
    // upstream: loadparm.c:lp_string() - variable substitution at access time.
    let effective_module = {
        let mut definition = module.definition.clone();
        if !options.is_empty() {
            match apply_daemon_param_overrides(options, &mut definition) {
                Ok(()) => {}
                Err(err) => {
                    let payload = format!("@ERROR: invalid daemon param: {err}");
                    send_error_and_exit(ctx.reader.get_mut(), ctx.limiter, ctx.messages, &payload)?;
                    return Ok(());
                }
            }
        }
        let client_addr = ctx.peer_ip.to_string();
        let client_host = ctx.effective_host().unwrap_or(&client_addr);
        expand_module_vars(&mut definition, &client_addr, client_host);
        ModuleRuntime::from(definition)
    };
    let module = &effective_module;

    apply_module_timeout(ctx.reader.get_mut(), module)?;

    // Handle authentication
    let auth_user = match handle_authentication(ctx, module, negotiated_protocol)? {
        Some(user) => user,
        None => return Ok(()),
    };

    // Run early exec after authentication so the authenticated username
    // is available in the RSYNC_USER_NAME environment variable.
    // upstream: clientserver.c - early_exec() runs after auth completes.
    if xfer_exec_enabled() {
        if let Some(command) = &module.early_exec {
            let early_path_ctx = PathExpansionContext {
                module_path: &module.path.display().to_string(),
                module_name: &module.name,
                username: auth_user.as_deref().unwrap_or(""),
                remote_addr: &ctx.peer_ip.to_string(),
                hostname: ctx.effective_host().unwrap_or(""),
                pid: std::process::id(),
            };
            let expanded_command = expand_exec_command(command, &early_path_ctx);
            let early_ctx = XferExecContext {
                module_name: &module.name,
                module_path: &module.path,
                host_addr: ctx.peer_ip,
                host_name: ctx.effective_host(),
                user_name: auth_user.as_deref(),
                request: ctx.request,
            };
            match run_early_exec(&expanded_command, &early_ctx) {
                Ok(Ok(())) => {
                    if let Some(log) = ctx.log_sink {
                        let text = format!(
                            "early exec succeeded for module '{}'",
                            ctx.request
                        );
                        let message = rsync_info!(text).with_role(Role::Daemon);
                        log_message(log, &message);
                    }
                }
                Ok(Err(error_msg)) => {
                    let payload = format!("@ERROR: {error_msg}");
                    send_error_and_exit(
                        ctx.reader.get_mut(),
                        ctx.limiter,
                        ctx.messages,
                        &payload,
                    )?;
                    return Ok(());
                }
                Err(err) => {
                    let payload = format!(
                        "@ERROR: failed to run early exec command for module '{}': {err}",
                        ctx.request
                    );
                    send_error_and_exit(
                        ctx.reader.get_mut(),
                        ctx.limiter,
                        ctx.messages,
                        &payload,
                    )?;
                    return Ok(());
                }
            }
        }
    }

    // Read client arguments
    let client_args = match read_and_log_client_args(ctx, negotiated_protocol)? {
        Some(args) => args,
        None => return Ok(()),
    };

    // Enforce read-only / write-only access restrictions.
    // upstream: clientserver.c:rsync_module() - after reading args, check
    // am_sender against lp_read_only(i) and lp_write_only(i).
    // When --sender is absent the client is pushing (server = Receiver).
    // A read-only module must reject pushes; a write-only module must reject pulls.
    let role = determine_server_role(&client_args);
    if module.read_only && matches!(role, ServerRole::Receiver) {
        let payload = "@ERROR: module is read only".to_string();
        send_error_and_exit(ctx.reader.get_mut(), ctx.limiter, ctx.messages, &payload)?;
        return Ok(());
    }
    if module.write_only && matches!(role, ServerRole::Generator) {
        let payload = "@ERROR: module is write only".to_string();
        send_error_and_exit(ctx.reader.get_mut(), ctx.limiter, ctx.messages, &payload)?;
        return Ok(());
    }

    // Validate module path before chroot (path must be accessible pre-chroot)
    if !validate_module_path(ctx, module)? {
        return Ok(());
    }

    // Apply chroot and privilege restrictions before building server config.
    // After chroot the effective module path becomes "/" since the process root
    // is now the module directory itself.
    // upstream: clientserver.c:rsync_module() - chroot + setuid/setgid happen
    // after auth and arg reading but before the transfer starts.
    if let Some(log) = ctx.log_sink {
        if let Err(err) = apply_module_privilege_restrictions(module, log) {
            let payload = format!("@ERROR: chroot/privilege setup failed: {err}");
            send_error_and_exit(ctx.reader.get_mut(), ctx.limiter, ctx.messages, &payload)?;
            return Ok(());
        }
    } else if module.use_chroot || module.uid.is_some() || module.gid.is_some() {
        let sink = open_privilege_fallback_sink();
        if let Err(err) = apply_module_privilege_restrictions(module, &sink) {
            let payload = format!("@ERROR: chroot/privilege setup failed: {err}");
            send_error_and_exit(ctx.reader.get_mut(), ctx.limiter, ctx.messages, &payload)?;
            return Ok(());
        }
    }

    // upstream: clientserver.c:962-969 - spawn name converter after privilege
    // reduction so it runs with reduced privileges inside the chroot.
    #[cfg(unix)]
    let _name_converter_guard = if let Some(ref cmd) = module.name_converter {
        let nc_path_ctx = PathExpansionContext {
            module_path: &module.path.display().to_string(),
            module_name: &module.name,
            username: auth_user.as_deref().unwrap_or(""),
            remote_addr: &ctx.peer_ip.to_string(),
            hostname: ctx.effective_host().unwrap_or(""),
            pid: std::process::id(),
        };
        let expanded = expand_exec_command(cmd, &nc_path_ctx);
        match NameConverter::spawn(&expanded) {
            Ok(nc) => Some(install_name_converter(nc)),
            Err(err) => {
                let payload = format!("@ERROR: name-converter exec failed: {err}");
                send_error_and_exit(
                    ctx.reader.get_mut(),
                    ctx.limiter,
                    ctx.messages,
                    &payload,
                )?;
                return Ok(());
            }
        }
    } else {
        None
    };

    // Windows: install a Win32 API-based name converter. No subprocess needed
    // since Windows doesn't use chroot; name resolution uses LookupAccountNameW
    // and NetUserEnum directly from the platform crate.
    #[cfg(windows)]
    let _name_converter_guard = Some(install_windows_name_converter());

    // After chroot the server must use "/" as the module root
    let effective_module;
    let config_module = if module.use_chroot {
        let mut adjusted = module.definition.clone();
        adjusted.path = PathBuf::from("/");
        effective_module = ModuleRuntime::from(adjusted);
        &effective_module
    } else {
        module
    };

    // Build server configuration with the effective (post-chroot) path
    let mut config = match build_server_config(ctx, &client_args, config_module)? {
        Some(cfg) => cfg,
        None => return Ok(()),
    };

    // upstream: clientserver.c:rsync_module() - build daemon_filter_list from
    // module filter/exclude/include/exclude_from/include_from parameters.
    // These rules are enforced server-side regardless of client-sent filters.
    match build_daemon_filter_rules(module) {
        Ok(rules) => config.daemon_filter_rules = rules,
        Err(err) => {
            let payload = format!("@ERROR: failed to load module filter rules: {err}");
            send_error_and_exit(ctx.reader.get_mut(), ctx.limiter, ctx.messages, &payload)?;
            return Ok(());
        }
    }

    // Setup transfer streams
    let (mut read_stream, mut write_stream) = match setup_transfer_streams(ctx)? {
        Some(streams) => streams,
        None => return Ok(()),
    };

    // Build XferExecContext for pre/post-xfer exec commands
    let xfer_ctx = XferExecContext {
        module_name: &module.name,
        module_path: &module.path,
        host_addr: ctx.peer_ip,
        host_name: ctx.effective_host(),
        user_name: auth_user.as_deref(),
        request: ctx.request,
    };

    // Build path expansion context for %-variable substitution in exec commands.
    // upstream: clientserver.c - exec command strings support %P, %m, %u, %a, %h, %p.
    let addr_str_exec = ctx.peer_ip.to_string();
    let path_str_exec = module.path.display().to_string();
    let exec_path_ctx = PathExpansionContext {
        module_path: &path_str_exec,
        module_name: &module.name,
        username: "",
        remote_addr: &addr_str_exec,
        hostname: ctx.effective_host().unwrap_or(""),
        pid: std::process::id(),
    };

    // Run pre-xfer exec if configured
    // upstream: clientserver.c - pre_exec() runs before the transfer starts.
    // Early-input data (if any) is piped to the script's stdin.
    if let Some(command) = module.pre_xfer_exec.as_deref().filter(|_| xfer_exec_enabled()) {
        let expanded_command = expand_exec_command(command, &exec_path_ctx);
        match run_pre_xfer_exec(&expanded_command, &xfer_ctx, ctx.early_input_data.as_deref()) {
            Ok(Ok(())) => {
                if let Some(log) = ctx.log_sink {
                    let text = format!(
                        "pre-xfer exec succeeded for module '{}'",
                        ctx.request
                    );
                    let message = rsync_info!(text).with_role(Role::Daemon);
                    log_message(log, &message);
                }
            }
            Ok(Err(error_msg)) => {
                let payload = format!("@ERROR: {error_msg}");
                send_error_and_exit(
                    ctx.reader.get_mut(),
                    ctx.limiter,
                    ctx.messages,
                    &payload,
                )?;
                if let Some(log) = ctx.log_sink {
                    let message = rsync_error!(1, error_msg).with_role(Role::Daemon);
                    log_message(log, &message);
                }
                return Ok(());
            }
            Err(io_err) => {
                let error_msg = format!(
                    "failed to run pre-xfer exec command for module '{}': {io_err}",
                    ctx.request
                );
                let payload = format!("@ERROR: {error_msg}");
                send_error_and_exit(
                    ctx.reader.get_mut(),
                    ctx.limiter,
                    ctx.messages,
                    &payload,
                )?;
                if let Some(log) = ctx.log_sink {
                    let message = rsync_error!(1, error_msg).with_role(Role::Daemon);
                    log_message(log, &message);
                }
                return Ok(());
            }
        }
    }

    // Build handshake and execute transfer
    let handshake = build_handshake_result(ctx.reader, negotiated_protocol, client_args, module);
    let final_protocol = handshake.protocol;

    let exit_status = execute_transfer(
        ctx,
        config,
        handshake,
        &mut read_stream,
        &mut write_stream,
        role,
        final_protocol,
        module,
    );

    // Run post-xfer exec if configured
    // upstream: clientserver.c - post_exec() runs after the transfer, regardless of outcome
    if let Some(command) = module.post_xfer_exec.as_deref().filter(|_| xfer_exec_enabled()) {
        let expanded_command = expand_exec_command(command, &exec_path_ctx);
        run_post_xfer_exec(&expanded_command, &xfer_ctx, exit_status, ctx.log_sink);
    }

    Ok(())
}
