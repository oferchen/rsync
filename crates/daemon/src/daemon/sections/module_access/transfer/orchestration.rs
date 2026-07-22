// Top-level approved-module request driver: connection acquisition,
// auth, exec hooks, config build, transfer execution, and TCP drain.

/// Runs the module's `post-xfer exec` hook, if one is configured and exec
/// hooks are enabled, with the given transfer `exit_status`.
///
/// upstream: clientserver.c:906-931 - the daemon forks a child that runs the
/// whole module session while the parent waits for the child's exit status and
/// then runs `post-xfer exec` with it. Because the parent waits for *any* child
/// outcome, the hook fires regardless of transfer success - including a refused
/// request (a read-only push or write-only pull exits `RERR_SYNTAX`, so the
/// hook sees `RSYNC_EXIT_STATUS=1`). This finalizer lets the early-return refuse
/// paths mirror that "post-xfer always runs" flow, matching the inline
/// post-xfer dispatch on the success path.
fn run_post_xfer_finalizer(
    ctx: &ModuleRequestContext<'_>,
    module: &ModuleRuntime,
    host_name: Option<&str>,
    user_name: Option<&str>,
    client_args: &[String],
    exit_status: i32,
) {
    let Some(command) = module
        .post_xfer_exec
        .as_deref()
        .filter(|_| xfer_exec_enabled())
    else {
        return;
    };

    let addr_str = ctx.peer_ip.to_string();
    let path_str = module.path.display().to_string();
    // Mirror the success-path `exec_path_ctx`: %-expansion of the command
    // string uses an empty username, while RSYNC_USER_NAME carries the
    // authenticated user via the `XferExecContext` below.
    let path_ctx = PathExpansionContext {
        module_path: &path_str,
        module_name: &module.name,
        username: "",
        remote_addr: &addr_str,
        hostname: host_name.unwrap_or(""),
        pid: std::process::id(),
    };
    let expanded_command = expand_exec_command(command, &path_ctx);
    let xfer_ctx = XferExecContext {
        module_name: &module.name,
        module_path: &module.path,
        host_addr: ctx.peer_ip,
        host_name,
        user_name,
        request: ctx.request,
        client_args,
    };
    run_post_xfer_exec(&expanded_command, &xfer_ctx, exit_status, ctx.log_sink);
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
    let _connection_guard = match module.try_acquire_connection() {
        Ok(guard) => guard,
        Err(ModuleConnectionError::Limit(limit)) => {
            return handle_max_connections_exceeded(ctx, module, limit);
        }
        Err(ModuleConnectionError::Io(error)) => {
            return handle_lock_error(ctx, &error);
        }
    };

    if let Some(log) = ctx.log_sink {
        log_module_request(log, ctx.effective_host(), ctx.peer_ip, ctx.request);
    }

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
                    send_error(ctx.reader.get_mut(), ctx.limiter, &payload)?;
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

    // Reconfigure the process-wide syslog handle for this connection when the
    // selected module carries a `syslog tag`/`syslog facility` (its own override
    // or an inherited global-section value). The guard restores the daemon-global
    // logger on drop, so it is held for the module's whole session. In log-file
    // mode (`log_sink` is Some) syslog is inactive, so the reconfiguration is
    // skipped. upstream: log.c:169 log_init reopens syslog per selected module.
    #[cfg(unix)]
    let _module_syslog_guard = if ctx.log_sink.is_none() {
        module.reconfigure_syslog()
    } else {
        None
    };

    apply_module_timeout(ctx.reader.get_mut(), module)?;

    let (auth_user, auth_access_level) =
        match handle_authentication(ctx, module, negotiated_protocol)? {
            Some(outcome) => outcome,
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
                // Early exec runs before client args are received.
                client_args: &[],
            };
            match run_early_exec(&expanded_command, &early_ctx) {
                Ok(Ok(())) => {
                    if let Some(log) = ctx.log_sink {
                        let text = format!("early exec succeeded for module '{}'", ctx.request);
                        let message = rsync_info!(text).with_role(Role::Daemon);
                        log_message(log, &message);
                    }
                }
                Ok(Err(error_msg)) => {
                    let payload = format!("@ERROR: {error_msg}");
                    send_error(ctx.reader.get_mut(), ctx.limiter, &payload)?;
                    return Ok(());
                }
                Err(err) => {
                    let payload = format!(
                        "@ERROR: failed to run early exec command for module '{}': {err}",
                        ctx.request
                    );
                    send_error(ctx.reader.get_mut(), ctx.limiter, &payload)?;
                    return Ok(());
                }
            }
        }
    }

    let client_args = match read_and_log_client_args(ctx, negotiated_protocol)? {
        Some(args) => args,
        None => return Ok(()),
    };

    // upstream: clientserver.c:rsync_module() -> parse_arguments() applies the
    // module's `refuse options` list against the actual client argv after the
    // post-OK `read_args()` round-trip. The earlier check at the OPTION-line
    // pre-handshake stage only sees client-supplied dparam overrides, never
    // the real transfer flags (e.g. `-z` packed into `-vlogDtprez.iLsfxCIvu`).
    //
    // Because `@RSYNCD: OK` has already been emitted, the client has switched
    // to multiplexed input. The error must travel as `MSG_ERROR_XFER` +
    // `MSG_ERROR_EXIT` frames; raw `@ERROR:` text would surface on the
    // receiver as `unexpected tag 77` (the 'T' from "The server ..." minus
    // `MPLEX_BASE = 7`). upstream: clientserver.c:1146 io_start_multiplex_out
    // immediately followed by `rwrite(FERROR, ...)`.
    if let Some(refused) = refused_client_arg(module, &client_args) {
        return handle_refused_option_post_handshake(
            ctx,
            &refused,
            negotiated_protocol,
            &client_args,
        );
    }

    // Enforce read-only / write-only access restrictions.
    // upstream: main.c:1166-1169 `do_server_recv()` rejects a read-only push
    // and main.c:934-936 `do_server_sender()` rejects a write-only pull, both
    // via `rprintf(FERROR, "ERROR: module is ...\n")` + `exit_cleanup(
    // RERR_SYNTAX)`. When --sender is absent the client is pushing (server =
    // Receiver); a read-only module must reject pushes and a write-only module
    // must reject pulls.
    //
    // Both upstream checks fire after `setup_protocol()` and
    // `io_start_multiplex_out()`, so the error must be framed as
    // `MSG_ERROR_XFER` + `MSG_ERROR_EXIT` rather than written as a raw line.
    // The `@RSYNCD: OK` acknowledgement already flipped the client to
    // multiplexed input; a raw `ERROR: ...\n` line would be decoded as a
    // 4-byte frame header and desync the stream (issue #227:
    // `invalid multi-message 102 (code 12)`).
    // `handle_access_denied_post_handshake` finishes the post-OK protocol
    // setup, then emits the framed error and exit code.
    //
    // upstream: clientserver.c:760 seeds `read_only = lp_read_only(module_id)`;
    // auth_server() (authenticate.c:340-343) then overrides it from the
    // authenticated user's `:ro` / `:rw` suffix. Apply that override here so a
    // `name:rw` user may push to a `read only = yes` module and a `name:ro`
    // user is refused writes to a `read only = no` module. The `write only`
    // check is unaffected: upstream's auth override only touches `read_only`.
    let role = determine_server_role(&client_args);
    let effective_read_only = access_effective_read_only(module.read_only, auth_access_level);
    // upstream: clientserver.c:906-931 - the post-xfer parent waits for the
    // module child and runs `post-xfer exec` regardless of outcome. A refused
    // read-only push / write-only pull exits `RERR_SYNTAX` (1) in the child, so
    // the hook still fires with `RSYNC_EXIT_STATUS=1`. Emit the framed
    // rejection first (the child's `rprintf(FERROR, ...)` + `exit_cleanup`),
    // then run the post-xfer finalizer, matching that ordering.
    if effective_read_only && matches!(role, ServerRole::Receiver) {
        let host_owned = ctx.effective_host().map(str::to_owned);
        let result = handle_access_denied_post_handshake(
            ctx,
            MODULE_READ_ONLY_PAYLOAD,
            negotiated_protocol,
            &client_args,
        );
        run_post_xfer_finalizer(
            ctx,
            module,
            host_owned.as_deref(),
            auth_user.as_deref(),
            &client_args,
            RERR_SYNTAX_EXIT_CODE,
        );
        return result;
    }
    if module.write_only && matches!(role, ServerRole::Generator) {
        let host_owned = ctx.effective_host().map(str::to_owned);
        let result = handle_access_denied_post_handshake(
            ctx,
            MODULE_WRITE_ONLY_PAYLOAD,
            negotiated_protocol,
            &client_args,
        );
        run_post_xfer_finalizer(
            ctx,
            module,
            host_owned.as_deref(),
            auth_user.as_deref(),
            &client_args,
            RERR_SYNTAX_EXIT_CODE,
        );
        return result;
    }

    if !validate_module_path(ctx, module)? {
        return Ok(());
    }

    // SEC-1.p: harvest the in-module --temp-dir / --partial-dir /
    // --backup-dir / --compare-dest / --copy-dest / --link-dest paths the
    // operator's configuration permits, so the Landlock allowlist below can
    // be widened to cover them (URV-5.b.REOPEN). Out-of-module paths are
    // silently dropped here and again in `build_server_config`'s ref_dir
    // retain block - upstream `main.c:841 check_alt_basis_dirs` warns on
    // a missing/out-of-tree basis but never aborts, and the standalone
    // link-dest / copy-dest interop fixtures rely on that contract.
    let Some(validated_client_paths) = validate_client_paths_in_module(ctx, module, &client_args)?
    else {
        return Ok(());
    };

    // Apply chroot and privilege restrictions before building server config.
    // After chroot the effective module path becomes "/" since the process root
    // is now the module directory itself.
    // upstream: clientserver.c:rsync_module() - chroot + setuid/setgid happen
    // after auth and arg reading but before the transfer starts.
    // Split into separate steps so each failure sends the correct upstream
    // error message: `@ERROR: chroot failed` vs `@ERROR: setuid failed` etc.
    let privilege_outcome = match apply_privilege_restrictions_with_upstream_errors(ctx, module)? {
        Some(outcome) => outcome,
        None => return Ok(()),
    };

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
                send_error(ctx.reader.get_mut(), ctx.limiter, &payload)?;
                return Ok(());
            }
        }
    } else {
        None
    };

    #[cfg(windows)]
    let _name_converter_guard = Some(install_windows_name_converter());

    // Rewrite the module path to the post-chroot inner directory only when
    // chroot was actually applied. After a rootless auto-fallback (D3) the
    // process is not chrooted, so the real absolute module path must be
    // preserved. The inner directory is "/" unless the module path carried a
    // `/./` marker, in which case it is the normalized remainder after it.
    // upstream: clientserver.c:845-862 - `module_dir` after the `/./` split.
    let effective_module;
    let config_module = if privilege_outcome.chroot_applied {
        let mut adjusted = module.definition.clone();
        adjusted.path = privilege_outcome
            .inner_module_path
            .clone()
            .unwrap_or_else(|| PathBuf::from("/"));
        effective_module = ModuleRuntime::from(adjusted);
        &effective_module
    } else {
        module
    };

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
            send_error(ctx.reader.get_mut(), ctx.limiter, &payload)?;
            return Ok(());
        }
    }

    // LSM-CAP.3: drop every Linux capability not required by this module
    // before Landlock engages. The worker process inherits the resulting
    // capability set across the transfer pipeline; combined with Landlock
    // (kernel-enforced filesystem allowlist) and seccomp (syscall surface
    // narrowing) this is the third layer of the LSM defense-in-depth stack.
    // Stub on non-Linux short-circuits to a no-op.
    drop_worker_capabilities(module, ctx.log_sink);

    // SEC-1.p: engage the Landlock LSM allowlist now that chroot, the
    // uid/gid drop, and daemon-config filter-rule loading have completed.
    // Filter rules referencing files outside module.path (e.g.
    // `exclude from = <abs-path>`) are read into memory above; once
    // Landlock engages, those external paths become unreadable. Stub on
    // non-Linux short-circuits to `Unavailable`. Failure to engage is
    // logged but does not abort the connection: SEC-1 *at* helpers still
    // provide the primary defense. The validated client-supplied paths
    // collected above are admitted to the allowlist alongside the module
    // root (URV-5.b.REOPEN): they are guaranteed in-tree and would
    // otherwise EACCES under a default-on flip.
    let extra_allowed: Vec<&Path> = validated_client_paths
        .landlock_roots
        .iter()
        .map(|p| p.as_path())
        .collect();
    if !engage_landlock_sandbox(ctx, module, &extra_allowed)? {
        return Ok(());
    }

    // LSM-SECCOMP: layer the BPF syscall allowlist over Landlock. Same
    // lifecycle phase as the LSM helper above: post-chroot, post-
    // privilege-drop, post-filter-load, pre-client-data. The seccomp
    // helper is a no-op on builds without the `daemon-seccomp` feature so
    // the call is unconditional. Stdio sessions are skipped because the
    // process IS the worker, so a process-scoped filter would restrict its
    // post-transfer cleanup. Failures do not abort the connection -
    // Landlock + SEC-1 `*at` remain the primary defenses.
    engage_seccomp_sandbox(ctx)?;

    // #503: arm the background delta-drain thread only for a real transfer. An
    // empty client-arg list means the peer requested the module then dropped the
    // socket without sending a transfer request, so no bidirectional delta data
    // flows and there is nothing to deadlock. Reading the socket directly on that
    // degenerate path returns EOF promptly on every platform (see
    // `setup_transfer_streams`); arming the drain there would spawn a thread that
    // hangs on a half-closed socket clone on Windows.
    let arm_drain = should_arm_delta_drain(&client_args);
    // The daemon-sender's socket write side opts into io_uring SEND_ZC only when
    // the client sent `--zero-copy` (parsed into `config.write.zero_copy_policy`
    // by `apply_long_form_args`). Auto/Disabled keep the current writer, so the
    // default path is byte- and behavior-identical.
    let zero_copy_policy = config.write.zero_copy_policy;
    let mut streams = match setup_transfer_streams(ctx, arm_drain, zero_copy_policy)? {
        Some(s) => s,
        None => return Ok(()),
    };

    // Extract host name before building structs that borrow ctx, so the
    // borrow is released before the FSM transition mutates ctx.conn_state.
    let host_name_owned = ctx.effective_host().map(str::to_owned);

    let xfer_ctx = XferExecContext {
        module_name: &module.name,
        module_path: &module.path,
        host_addr: ctx.peer_ip,
        host_name: host_name_owned.as_deref(),
        user_name: auth_user.as_deref(),
        request: ctx.request,
        client_args: &client_args,
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
        hostname: host_name_owned.as_deref().unwrap_or(""),
        pid: std::process::id(),
    };

    // upstream: clientserver.c - pre_exec() runs before the transfer starts.
    // Early-input data (if any) is piped to the script's stdin. Stdout from the
    // script is sent to the client as an info message.
    if let Some(command) = module
        .pre_xfer_exec
        .as_deref()
        .filter(|_| xfer_exec_enabled())
    {
        let expanded_command = expand_exec_command(command, &exec_path_ctx);
        match run_pre_xfer_exec(
            &expanded_command,
            &xfer_ctx,
            ctx.early_input_data.as_deref(),
        ) {
            Ok(Ok(output)) => {
                // upstream: clientserver.c:pre_exec() - stdout from the script is
                // sent to the client as an info message before the transfer.
                if !output.stdout.is_empty() {
                    write_limited(ctx.reader.get_mut(), ctx.limiter, output.stdout.as_bytes())?;
                    write_limited(ctx.reader.get_mut(), ctx.limiter, b"\n")?;
                }
                if let Some(log) = ctx.log_sink {
                    let text = format!("pre-xfer exec succeeded for module '{}'", ctx.request);
                    let message = rsync_info!(text).with_role(Role::Daemon);
                    log_message(log, &message);
                }
            }
            Ok(Err(err)) => {
                // upstream: clientserver.c - stdout from the script is sent to the
                // client before the @ERROR line.
                if !err.stdout.is_empty() {
                    write_limited(ctx.reader.get_mut(), ctx.limiter, err.stdout.as_bytes())?;
                    write_limited(ctx.reader.get_mut(), ctx.limiter, b"\n")?;
                }
                let payload = format!("@ERROR: {}", err.message);
                send_error(ctx.reader.get_mut(), ctx.limiter, &payload)?;
                if let Some(log) = ctx.log_sink {
                    let message = rsync_error!(1, err.message).with_role(Role::Daemon);
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
                send_error(ctx.reader.get_mut(), ctx.limiter, &payload)?;
                if let Some(log) = ctx.log_sink {
                    let message = rsync_error!(1, error_msg).with_role(Role::Daemon);
                    log_message(log, &message);
                }
                return Ok(());
            }
        }
    }

    // FSM: -> Transferring - auth passed (or was not required), all
    // pre-transfer validation complete, transfer engine about to run.
    ctx.conn_state = ctx
        .conn_state
        .transition(ConnectionState::Transferring)
        .map_err(transition_error)?;

    let handshake =
        build_handshake_result(ctx.reader, negotiated_protocol, client_args.clone(), module);
    let final_protocol = handshake.protocol;

    let supports_tcp_shutdown = streams.supports_tcp_shutdown;
    let exit_status = execute_transfer(
        ctx,
        config,
        handshake,
        &mut *streams.read,
        &mut *streams.write,
        role,
        final_protocol,
        module,
    );

    // #503: stop and join the background delta-drain thread before the TCP
    // goodbye drain below reads the socket via a different clone. The engine's
    // own goodbye handshake completed inside `execute_transfer` (it read
    // through the `DrainingReader`), so stopping here only halts draining of
    // any post-goodbye trailing bytes, which the drain-until-EOF loop below
    // discards anyway. Joining before that loop prevents two readers competing
    // for the same socket. `Drop` on `streams` is a backstop for early-return
    // paths above that never reach this point.
    if let Some(drain) = streams.drain_handle.take() {
        drain.stop();
    }

    // Graceful TCP shutdown: drain peer's goodbye, then linger + close.
    //
    // Background:
    //
    // Upstream rsync forks a child process per connection; the child holds
    // the sole fd reference and exits via `exit_cleanup` which calls
    // `_exit()`. The kernel closes the orphaned fd, the TCP stack delivers
    // any pending TX bytes, and FIN is queued AFTER the kernel finishes
    // processing the receive buffer. In particular, the sender's
    // `read_final_goodbye()` returns AFTER reading the receiver's final
    // NDX_DONE, but the kernel still has room to receive whatever the
    // receiver writes immediately afterward (its MSG_STATS / extra
    // NDX_DONE pair) before the process exit triggers FIN.
    //
    // Our daemon uses threads, not fork. The connection thread holds
    // multiple cloned `TcpStream` handles (a read clone, a write clone,
    // and the original `DaemonStream`) for the same kernel socket. When
    // the thread function returns, those drop and the kernel closes the
    // last fd. The structural challenge is that calling
    // `shutdown(SHUT_WR)` BEFORE the receiver has finished writing its
    // goodbye causes the receiver to see FIN immediately, abort its
    // pending writes (which our `read_final_goodbye()` equivalent has not
    // yet drained), and report
    // "connection unexpectedly closed (N bytes received so far)".
    //
    // The failure cluster (batch-mode, alt-dest, daemon-gzip-download,
    // daemon-refuse-compress) all hit this race: the engine's
    // `handle_goodbye_with_finalizer` reads the receiver's first NDX_DONE,
    // writes the daemon's NDX_DONE, and reads the receiver's second
    // NDX_DONE; but upstream's receiver then writes MSG_STATS + a final
    // NDX_DONE on its side, relayed through the generator-to-sender pipe.
    // Closing the socket immediately after our `handle_goodbye` returns
    // races those trailing bytes.
    //
    // The structural fix mirrors upstream's
    // `noop_io_until_death()` semantics for the sender side: keep reading
    // from the peer until it sends FIN (EOF). The peer FINs once its own
    // `exit_cleanup` runs, which only happens after it has flushed its
    // goodbye. Bounded by a generous timeout (5 seconds) so a wedged
    // peer cannot block the daemon thread indefinitely.
    //
    // Sequence:
    //   1. SO_LINGER(5s) - kernel blocks close() until TX data is ACKed
    //      or the linger window expires, so the final goodbye bytes our
    //      engine wrote reach the peer even after we drop the socket.
    //   2. Drain read until EOF - waits for the peer to finish its own
    //      goodbye and FIN. We do NOT call `shutdown(Write)` first;
    //      sending FIN early would tell the peer "I'm done" before the
    //      peer has written its trailing goodbye (the oc<->upstream
    //      download case where the receiver still sends MSG_STATS + a
    //      final NDX_DONE after our engine's handle_goodbye returns).
    //   3. shutdown(SHUT_WR) - half-close the write side now the protocol
    //      is complete, so the peer observes our FIN and stops reading.
    //   4. Drain read until EOF AGAIN - once we have FINed, wait for the
    //      peer to observe it and close, draining any last bytes so the
    //      final close() finds an EMPTY receive buffer. A close() with
    //      unread bytes queued is an abortive close: the kernel discards
    //      the data and sends a RST instead of a FIN, which the peer
    //      reports as "Connection reset by peer (os error 104)" (exit 23)
    //      even though the transfer completed. Steps 2 and 4 bracket the
    //      half-close so neither an early FIN (step 2 first) nor a slow
    //      peer that has not yet FINed by step 2's timeout (step 4 catches
    //      it after the half-close prompts its close) can leave unread
    //      bytes at the final close.
    //   5. close() - the linger window guarantees in-flight TX bytes are
    //      delivered before the close completes.
    //
    // Every drain is bounded by a read timeout so a wedged peer can never
    // pin the connection thread (never an unbounded blocking read).
    //
    // upstream: io.c:943-963 noop_io_until_death() loops on read() until
    // the peer sends FIN; cleanup.c:265 then calls close_all(). Our
    // sequence collapses that pattern to fit the threaded daemon model.
    //
    // For stdio streams (remote-shell daemon mode), TCP shutdown is not
    // applicable - the pipe/fd closes naturally when dropped.

    if supports_tcp_shutdown {
        let stream = ctx.reader.get_mut();

        // SO_LINGER ensures the final goodbye TX bytes the engine wrote
        // are delivered before the kernel reclaims the socket. The 5s
        // window matches upstream's expected goodbye round-trip latency.
        // Catastrophic-failure fallback: even if the UTS-V3.A
        // `shutdown_send_side` barrier below cannot be reached (e.g. the
        // TcpStream accessor is unavailable on a non-socket path),
        // SO_LINGER still bounds the kernel-level drain.
        if let Some(tcp) = stream.tcp_stream() {
            let sock = socket2::SockRef::from(tcp);
            let _ = sock.set_linger(Some(Duration::from_secs(5)));
        }

        // Pre-shutdown drain: read the peer's trailing bytes until FIN (EOF).
        // We do NOT shutdown(Write) first: that would tell the peer "I'm done"
        // before it has written its trailing MSG_STATS / NDX_DONE, which the
        // peer (an upstream receiver in the oc<->upstream download case)
        // abandons on receipt of FIN. Mirrors upstream's
        // `noop_io_until_death()`: read until the peer FINs on its own
        // `exit_cleanup`. Bounded by a read timeout so a wedged peer cannot pin
        // the daemon thread; the window is generous for any goodbye exchange.
        drain_until_peer_eof(stream, GOODBYE_DRAIN_TIMEOUT);

        // UTS-V3.A explicit drain barrier (kernel-level half-close).
        // Once the peer has FINed (read-drain returned EOF/timeout) and
        // the generator orchestrator has already flushed every user-space
        // byte via `ServerWriter::flush_all_pending`, an explicit
        // `shutdown(SHUT_WR)` is safe and observable: it confirms the
        // half-close with a bounded SO_LINGER drain. Errors that mean
        // "peer already closed" are tolerated; any other shutdown error
        // is logged so the operator sees the failure rather than relying
        // on the implicit Drop-time close. The companion SO_LINGER set
        // above is the catastrophic-failure fallback.
        //
        // upstream: cleanup.c::handle_cleanup() -> close_all() emits the
        // kernel FIN as the process exits; the threaded daemon collapses
        // that pattern into the explicit shutdown here.
        if let Some(tcp) = stream.tcp_stream() {
            if let Err(err) =
                core::server::writer::shutdown_send_side(tcp, Duration::from_secs(5))
            {
                if let Some(log) = ctx.log_sink {
                    let text = format!("daemon-sender drain-barrier shutdown failed: {err}");
                    let message = rsync_warning!(text).with_role(Role::Daemon);
                    log_message(log, &message);
                }
            }
        }

        // Post-shutdown drain: now that our FIN is on the wire, wait for the
        // peer to observe it and close, consuming any last bytes so the final
        // close() below finds an EMPTY receive buffer and emits a clean FIN
        // rather than an abortive RST. This catches the slow-peer case where
        // the pre-shutdown drain timed out before the peer had FINed: the
        // half-close prompts the peer to finish reading and close, and this
        // drain reaps its FIN. Bounded by the same read timeout so it can never
        // hang. See `graceful_close.rs` for the abortive-close rationale.
        drain_until_peer_eof(stream, GOODBYE_DRAIN_TIMEOUT);
    }

    // Drop transfer-engine stream clones after the drain completes.
    // These cloned TcpStream handles share the same kernel socket as the
    // DaemonStream; keeping them alive during the drain preserves the fd
    // refcount that was present during the transfer. Dropping now lets
    // the kernel queue FIN + close once the SO_LINGER window completes
    // delivery of any in-flight TX bytes.
    drop(streams);

    // upstream: clientserver.c - post_exec() runs after the transfer, regardless of outcome
    if let Some(command) = module
        .post_xfer_exec
        .as_deref()
        .filter(|_| xfer_exec_enabled())
    {
        let expanded_command = expand_exec_command(command, &exec_path_ctx);
        run_post_xfer_exec(&expanded_command, &xfer_ctx, exit_status, ctx.log_sink);
    }

    // FSM: Transferring -> Closing - transfer and post-xfer hooks complete.
    ctx.conn_state = ctx
        .conn_state
        .transition(ConnectionState::Closing)
        .map_err(transition_error)?;

    Ok(())
}
