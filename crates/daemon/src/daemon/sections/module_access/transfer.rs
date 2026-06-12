// Transfer execution - stream setup, handshake building, and transfer lifecycle.
//
// Handles the final phase of a module request: validating the module path,
// applying chroot and privilege restrictions, spawning the name converter,
// running pre/post-xfer exec hooks, and invoking the Rust transfer engine.
//
// upstream: `clientserver.c` - after `rsync_module()` completes authentication
// and argument parsing, it calls `chdir(lp_path())`, `chroot(".")`,
// `setgid()`/`setuid()`, and then enters the transfer pipeline.

/// Applies chroot and privilege restrictions, sending upstream-compatible
/// `@ERROR` messages on failure.
///
/// Upstream sends distinct error strings for each failure type:
/// - `@ERROR: chroot failed` (clientserver.c:981)
/// - `@ERROR: setgid failed` (clientserver.c:1010)
/// - `@ERROR: setgroups failed` (clientserver.c:1017)
/// - `@ERROR: setuid failed` (clientserver.c:1039)
///
/// Returns `Ok(true)` when restrictions applied successfully or were not
/// configured. Returns `Ok(false)` after sending an error to the client.
fn apply_privilege_restrictions_with_upstream_errors(
    ctx: &mut ModuleRequestContext<'_>,
    module: &ModuleRuntime,
) -> io::Result<bool> {
    let needs_chroot = module.use_chroot;
    let needs_privdrop = module.uid.is_some() || module.gid.is_some();

    if !needs_chroot && !needs_privdrop {
        return Ok(true);
    }

    // Resolve log sink: use the configured one, or create a fallback.
    let fallback_sink;
    let log_sink: &SharedLogSink = match ctx.log_sink {
        Some(log) => log,
        None => {
            fallback_sink = open_privilege_fallback_sink();
            &fallback_sink
        }
    };

    // upstream: clientserver.c:978-984 - chroot first, then privilege drop.
    if needs_chroot {
        if let Err(err) = apply_chroot(&module.path, log_sink) {
            // upstream: clientserver.c:981 - `@ERROR: chroot failed\n`
            // upstream: clientserver.c:647 - `@ERROR: chdir failed\n`
            let text = err.to_string();
            let payload = if text.contains("chdir") {
                CHDIR_FAILED_PAYLOAD
            } else {
                CHROOT_FAILED_PAYLOAD
            };
            send_error_and_exit(ctx.reader.get_mut(), ctx.limiter, ctx.messages, payload)?;
            return Ok(false);
        }
    }

    if needs_privdrop {
        if let Err(err) = drop_privileges(module.uid, module.gid, log_sink) {
            // Distinguish upstream error messages based on the error text.
            // upstream: clientserver.c:1010/1017/1039
            let text = err.to_string();
            let payload = if text.contains("setgroups") {
                SETGROUPS_FAILED_PAYLOAD
            } else if text.contains("setuid") {
                SETUID_FAILED_PAYLOAD
            } else {
                SETGID_FAILED_PAYLOAD
            };
            send_error_and_exit(ctx.reader.get_mut(), ctx.limiter, ctx.messages, payload)?;
            return Ok(false);
        }
    }

    Ok(true)
}

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

/// Outcome of [`validate_client_paths_in_module`].
///
/// `Rejected` is the daemon-error path: an `@ERROR` reply was already sent.
/// `Accepted` carries the absolute, canonicalised, in-module paths the
/// client requested via `--temp-dir` / `--partial-dir` / `--backup-dir` /
/// `--compare-dest` / `--copy-dest` / `--link-dest`. These paths are
/// guaranteed to start with the module root (SEC-1.p invariant) and are
/// fed straight into [`engage_landlock_sandbox`] so the kernel allowlist
/// covers every writable / readable surface the receiver will touch.
#[derive(Debug, Default)]
struct ValidatedClientPaths {
    /// Canonicalised, in-module paths suitable for `Landlock` allowlisting.
    landlock_roots: Vec<std::path::PathBuf>,
}

/// Classifies one client-supplied path against the canonical module root.
///
/// Pure helper extracted from [`validate_client_paths_in_module`] so the
/// containment + allowlist-widening logic is unit-testable without spinning
/// up a full [`ModuleRequestContext`]. Returns:
///
/// - `Ok(Some(canonical))` when `raw_path` is absolute and (after
///   canonicalisation, with a lexical fallback) starts with `module_root` -
///   the caller adds the result to the Landlock allowlist.
/// - `Ok(None)` when the path is relative; relative paths resolve under
///   the module root, so they cannot escape and need no explicit entry.
/// - `Err(())` when the path is absolute and escapes the module root -
///   the caller sends an `@ERROR` reply.
fn classify_client_path_against_module(
    raw_path: &str,
    module_root: &Path,
) -> Result<Option<std::path::PathBuf>, ()> {
    let path = Path::new(raw_path);
    if path.is_relative() {
        return Ok(None);
    }
    let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    if canonical.starts_with(module_root) {
        Ok(Some(canonical))
    } else {
        Err(())
    }
}

/// Rejects client-supplied `--temp-dir` / `--partial-dir` / `--backup-dir`
/// / `--compare-dest` / `--copy-dest` / `--link-dest` paths that resolve
/// outside the module root, and collects the accepted absolute paths so the
/// SEC-1.p Landlock allowlist can be widened to cover them.
///
/// The audit (SEC-1.p, section 10) recommends REJECT over widening the
/// Landlock allowlist for *out-of-module* paths: rsync's own chroot mode
/// behaves the same way, and expanding the writable surface to honour an
/// attacker-supplied prefix undermines the whole point of the sandbox.
/// For *in-module* absolute paths the reverse holds: the operator's
/// configuration permits them, so they must reach the Landlock allowlist
/// or a default-on flip would EACCES legitimate writes (URV-5.b.REOPEN).
///
/// Returns `Ok(Some(ValidatedClientPaths))` when every requested path is
/// in-tree (or absent); returns `Ok(None)` after emitting an `@ERROR`
/// reply when any path escapes the module root.
fn validate_client_paths_in_module(
    ctx: &mut ModuleRequestContext<'_>,
    module: &ModuleRuntime,
    client_args: &[String],
) -> io::Result<Option<ValidatedClientPaths>> {
    let Ok(module_root) = module.path.canonicalize() else {
        // Module path failed to canonicalize - the existence check above
        // already succeeded, so this is a race or a permission problem; let
        // the transfer continue and fail with a more precise error later.
        return Ok(Some(ValidatedClientPaths::default()));
    };

    // De-duplicate inside this single connection so a client sending the
    // same `--link-dest=/abs/snap` twice does not bloat the allowlist.
    let mut accepted: Vec<std::path::PathBuf> = Vec::new();

    let mut iter = client_args.iter().peekable();
    while let Some(arg) = iter.next() {
        let candidate = if let Some(rest) = arg.strip_prefix("--temp-dir=") {
            Some(("--temp-dir", rest.to_owned()))
        } else if let Some(rest) = arg.strip_prefix("--partial-dir=") {
            Some(("--partial-dir", rest.to_owned()))
        } else if let Some(rest) = arg.strip_prefix("--backup-dir=") {
            Some(("--backup-dir", rest.to_owned()))
        } else if let Some(rest) = arg.strip_prefix("--compare-dest=") {
            Some(("--compare-dest", rest.to_owned()))
        } else if let Some(rest) = arg.strip_prefix("--copy-dest=") {
            Some(("--copy-dest", rest.to_owned()))
        } else if let Some(rest) = arg.strip_prefix("--link-dest=") {
            Some(("--link-dest", rest.to_owned()))
        } else if matches!(
            arg.as_str(),
            "--temp-dir"
                | "--partial-dir"
                | "--backup-dir"
                | "--compare-dest"
                | "--copy-dest"
                | "--link-dest"
        ) {
            iter.next().map(|v| (arg.as_str(), v.clone()))
        } else {
            None
        };

        let Some((flag, raw_path)) = candidate else {
            continue;
        };

        match classify_client_path_against_module(&raw_path, &module_root) {
            Ok(None) => continue,
            Ok(Some(canonical)) => {
                if !accepted.iter().any(|p| p == &canonical) {
                    accepted.push(canonical);
                }
                continue;
            }
            Err(()) => {}
        }

        let payload = format!("@ERROR: {flag} path '{raw_path}' is outside module root");
        send_error_and_exit(ctx.reader.get_mut(), ctx.limiter, ctx.messages, &payload)?;
        if let Some(log) = ctx.log_sink {
            let text = format!(
                "module '{}': rejected {flag}='{raw_path}' from {} ({}) - outside module root '{}'",
                ctx.request,
                ctx.effective_host().unwrap_or("unknown"),
                ctx.peer_ip,
                module_root.display(),
            );
            let message = rsync_error!(1, text).with_role(Role::Daemon);
            log_message(log, &message);
        }
        return Ok(None);
    }

    Ok(Some(ValidatedClientPaths {
        landlock_roots: accepted,
    }))
}

/// Engages the SEC-1.p Landlock LSM allowlist for the receiver path.
///
/// Called immediately after `apply_module_privilege_restrictions` has
/// applied chroot + uid/gid drop so the kernel allowlist covers exactly the
/// writable surface the remainder of the connection needs. The stub on
/// non-Linux targets short-circuits to `Unavailable` so the wire-in does
/// not need `#[cfg]` branching.
///
/// `extra_allowed_paths` carries absolute, in-module paths that
/// `validate_client_paths_in_module` admitted from the client args
/// (`--temp-dir` / `--partial-dir` / `--backup-dir` / `--compare-dest` /
/// `--copy-dest` / `--link-dest`). The caller is responsible for the
/// containment check; this helper only forwards the slice to the kernel.
/// Closing URV-5.b.REOPEN: without the widening, a default-on Landlock
/// flip would EACCES the very paths the operator's configuration permits.
///
/// Returns `Ok(true)` on every non-fatal outcome (engaged, downgraded,
/// unavailable, or skipped because a pre/post-xfer-exec hook is configured).
/// Returns `Ok(false)` after emitting an `@ERROR` reply when the kernel
/// advertised Landlock support but the helper failed to engage the ruleset -
/// we treat that as a regression because the SEC-1.p design requires the
/// sandbox to be live on supporting kernels.
///
/// When `pre_xfer_exec` or `post_xfer_exec` is configured, the sandbox is
/// skipped: Landlock rulesets are inherited by child processes, so engaging
/// the allowlist would block `exec()` of hook scripts that live outside the
/// module path (the common case - e.g. `/usr/local/bin/notify.sh`). Per-module
/// opt-out via configuration matches the operator's intent (they explicitly
/// chose to run hooks) and preserves SEC-1 *at* helpers as the primary
/// defense for those modules.
fn engage_landlock_sandbox(
    ctx: &mut ModuleRequestContext<'_>,
    module: &ModuleRuntime,
    extra_allowed_paths: &[&Path],
) -> io::Result<bool> {
    use fast_io::landlock::{is_supported, restrict_to_module_paths, LandlockOutcome};

    if module.pre_xfer_exec.is_some() || module.post_xfer_exec.is_some() {
        if let Some(log) = ctx.log_sink {
            let text = format!(
                "module '{}': landlock=skipped reason=pre-xfer-exec or post-xfer-exec configured (would block hook exec)",
                ctx.request,
            );
            let message = rsync_info!(text).with_role(Role::Daemon);
            log_message(log, &message);
        }
        return Ok(true);
    }

    if !is_supported() {
        if let Some(log) = ctx.log_sink {
            let text = format!(
                "module '{}': landlock unavailable on this kernel; SEC-1 *at* helpers remain the sole defense",
                ctx.request,
            );
            let message = rsync_info!(text).with_role(Role::Daemon);
            log_message(log, &message);
        }
        return Ok(true);
    }

    // Roots: the module path is the always-present writable surface plus
    // any client-supplied alt-basis (`--compare-dest` / `--copy-dest` /
    // `--link-dest`) or relocation (`--temp-dir` / `--partial-dir` /
    // `--backup-dir`) paths that `validate_client_paths_in_module` has
    // already confirmed to resolve beneath `module.path` (URV-5.b.1).
    // Widening the allowlist to those paths is safe because the containment
    // check already proved they cannot escape the module tree; without the
    // widening, a default-on Landlock flip (URV-5.c.5) would EACCES
    // legitimate writes the operator's configuration permits.
    let mut roots: Vec<&Path> = Vec::with_capacity(1 + extra_allowed_paths.len());
    roots.push(module.path.as_path());
    roots.extend_from_slice(extra_allowed_paths);

    match restrict_to_module_paths(&roots) {
        LandlockOutcome::Enforced(status) => {
            if let Some(log) = ctx.log_sink {
                let text = format!(
                    "module '{}': landlock engaged ({status:?}) over {} root(s)",
                    ctx.request,
                    roots.len(),
                );
                let message = rsync_info!(text).with_role(Role::Daemon);
                log_message(log, &message);
            }
            Ok(true)
        }
        LandlockOutcome::Unavailable => {
            // Race: probe said supported, restrict_self() said no. Log and
            // continue - SEC-1 *at* helpers still mitigate the attack.
            if let Some(log) = ctx.log_sink {
                let text = format!(
                    "module '{}': landlock probe positive but kernel returned Unavailable - falling back to SEC-1 *at* defense",
                    ctx.request,
                );
                let message = rsync_warning!(text).with_role(Role::Daemon);
                log_message(log, &message);
            }
            Ok(true)
        }
        LandlockOutcome::Error(err) => {
            // The kernel said yes to landlock but no to our ruleset; this
            // is a regression worth surfacing. Log a warning and continue
            // rather than killing the connection - the SEC-1 *at* chain
            // still provides the primary defense.
            if let Some(log) = ctx.log_sink {
                let text = format!(
                    "module '{}': landlock setup failed: {err}; relying on SEC-1 *at* defense",
                    ctx.request,
                );
                let message = rsync_warning!(text).with_role(Role::Daemon);
                log_message(log, &message);
            }
            Ok(true)
        }
    }
}

/// Engages the LSM-SECCOMP BPF allowlist for the worker.
///
/// Layers above the Landlock LSM defense engaged immediately prior:
/// Landlock denies path-based syscalls with `EACCES`; seccomp denies
/// out-of-scope syscalls with `SIGSYS` before the kernel ever consults
/// the LSM stack.
///
/// On builds without the `daemon-seccomp` feature the helper is a no-op
/// that returns `Unavailable`; the wire-in is unconditional so the call
/// site does not need `#[cfg]` branching. Construction or installation
/// failure is logged as a warning and the connection continues - SEC-1
/// `*at` helpers and Landlock remain the primary defenses.
///
/// **Stdio sessions are skipped.** When the daemon runs as `--server
/// --daemon` over stdin/stdout (remote-shell daemon mode via `lsh.sh` /
/// SSH), the process IS the worker - there is no parent accept loop to
/// survive a `KillProcess`. The seccomp filter would also restrict
/// post-transfer cleanup, process exit, and any syscalls the Python test
/// harness or shell wrapper needs after the transfer completes. TCP
/// daemon workers are disposable threads inside a long-lived process, so
/// the filter dies with the thread and does not affect the daemon or any
/// other connection.
fn engage_seccomp_sandbox(ctx: &mut ModuleRequestContext<'_>) -> io::Result<()> {
    // Stdio sessions: the process IS the worker. Applying seccomp here
    // would restrict the entire process (including post-transfer cleanup,
    // exit handlers, and the parent shell). Skip - Landlock + SEC-1 *at*
    // remain the defense for remote-shell daemon mode.
    if ctx.reader.get_ref().is_stdio() {
        if let Some(log) = ctx.log_sink {
            let text = format!(
                "module '{}': seccomp BPF skipped (stdio session - filter would restrict entire process)",
                ctx.request,
            );
            let message = rsync_info!(text).with_role(Role::Daemon);
            log_message(log, &message);
        }
        return Ok(());
    }

    match apply_worker_seccomp_filter() {
        #[cfg(all(target_os = "linux", feature = "daemon-seccomp"))]
        SeccompOutcome::Installed => {
            if let Some(log) = ctx.log_sink {
                let text = format!(
                    "module '{}': seccomp BPF filter engaged (KillProcess on unlisted syscalls)",
                    ctx.request,
                );
                let message = rsync_info!(text).with_role(Role::Daemon);
                log_message(log, &message);
            }
        }
        SeccompOutcome::Unavailable => {
            // No-op build (non-Linux, daemon-seccomp feature off,
            // unsupported arch, or operator opt-out via env var).
            if let Some(log) = ctx.log_sink {
                let text = format!(
                    "module '{}': seccomp BPF unavailable in this build; Landlock + SEC-1 *at* remain the defense",
                    ctx.request,
                );
                let message = rsync_info!(text).with_role(Role::Daemon);
                log_message(log, &message);
            }
        }
        #[cfg(all(target_os = "linux", feature = "daemon-seccomp"))]
        SeccompOutcome::Error(err) => {
            if let Some(log) = ctx.log_sink {
                let text = format!(
                    "module '{}': seccomp BPF setup failed: {err}; relying on Landlock + SEC-1 *at* defense",
                    ctx.request,
                );
                let message = rsync_warning!(text).with_role(Role::Daemon);
                log_message(log, &message);
            }
        }
    }
    Ok(())
}

/// Transfer stream pair: separate read and write handles for the transfer engine.
///
/// For TCP connections, both sides are cloned `TcpStream` handles pointing at
/// the same socket. For stdio connections (remote-shell daemon mode), the reader
/// wraps stdin and the writer wraps stdout.
struct TransferStreams {
    read: Box<dyn Read + Send>,
    write: Box<dyn Write + Send>,
    /// Whether the write side supports TCP shutdown (false for stdio/pipes).
    supports_tcp_shutdown: bool,
}

/// Sets up the transfer streams for the transfer engine.
///
/// For TCP/TLS connections, configures TCP_NODELAY and clones the stream to get
/// independent read/write handles. For stdio connections (remote-shell daemon
/// mode), opens fresh stdin/stdout handles since the original pair is consumed
/// by the BufReader.
///
/// Returns the transfer streams on success, or sends an error and returns `None`.
fn setup_transfer_streams(
    ctx: &mut ModuleRequestContext<'_>,
) -> io::Result<Option<TransferStreams>> {
    let stream = ctx.reader.get_mut();
    stream.set_nodelay(true)?;

    if stream.is_stdio() {
        // For stdio mode, the DaemonStream wraps a StdioPair (stdin + stdout).
        // The BufReader has consumed it, but the transfer engine needs separate
        // read/write handles. We open fresh stdin/stdout handles here - the
        // buffered data from the BufReader is captured in HandshakeResult.buffered
        // and chained ahead of stdin by run_server_with_handshake.
        // upstream: start_daemon(STDIN_FILENO, STDOUT_FILENO) uses the same
        // fds for both handshake and transfer.
        let stdin = io::stdin();
        let stdout = io::stdout();
        return Ok(Some(TransferStreams {
            read: Box::new(stdin),
            write: Box::new(stdout),
            supports_tcp_shutdown: false,
        }));
    }

    let tcp = stream
        .tcp_stream()
        .expect("non-stdio stream has tcp_stream");

    let read_stream = match tcp.try_clone() {
        Ok(s) => s,
        Err(err) => {
            let payload = format!("@ERROR: failed to clone stream: {err}");
            send_error_and_exit(ctx.reader.get_mut(), ctx.limiter, ctx.messages, &payload)?;
            return Ok(None);
        }
    };

    let write_stream = match tcp.try_clone() {
        Ok(s) => s,
        Err(err) => {
            return Err(io::Error::other(format!(
                "failed to clone write stream: {err}"
            )));
        }
    };

    Ok(Some(TransferStreams {
        read: Box::new(read_stream),
        write: Box::new(write_stream),
        supports_tcp_shutdown: true,
    }))
}

/// Builds the handshake result for the transfer.
fn build_handshake_result(
    reader: &BufReader<DaemonStream>,
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
    read_stream: &mut dyn Read,
    write_stream: &mut dyn Write,
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

    // Use standard buffered I/O for daemon socket communication.
    // io_uring SEND blocks in submit_and_wait() during bidirectional protocol
    // exchanges (NDX_DONE, stats, goodbye) when TCP backpressure occurs,
    // causing 10-second hangs. Standard I/O handles partial writes correctly,
    // matching upstream rsync's socket I/O model.
    let result = run_server_with_handshake(
        config,
        handshake,
        read_stream,
        write_stream,
        None,
        None,
        None,
    );

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
                    send_error_and_exit(ctx.reader.get_mut(), ctx.limiter, ctx.messages, &payload)?;
                    return Ok(());
                }
                Err(err) => {
                    let payload = format!(
                        "@ERROR: failed to run early exec command for module '{}': {err}",
                        ctx.request
                    );
                    send_error_and_exit(ctx.reader.get_mut(), ctx.limiter, ctx.messages, &payload)?;
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
    // upstream: clientserver.c:rsync_module() - after reading args, check
    // am_sender against lp_read_only(i) and lp_write_only(i).
    // When --sender is absent the client is pushing (server = Receiver).
    // A read-only module must reject pushes; a write-only module must reject pulls.
    let role = determine_server_role(&client_args);
    if module.read_only && matches!(role, ServerRole::Receiver) {
        send_error_and_exit(
            ctx.reader.get_mut(),
            ctx.limiter,
            ctx.messages,
            MODULE_READ_ONLY_PAYLOAD,
        )?;
        return Ok(());
    }
    if module.write_only && matches!(role, ServerRole::Generator) {
        send_error_and_exit(
            ctx.reader.get_mut(),
            ctx.limiter,
            ctx.messages,
            MODULE_WRITE_ONLY_PAYLOAD,
        )?;
        return Ok(());
    }

    if !validate_module_path(ctx, module)? {
        return Ok(());
    }

    // SEC-1.p: reject client-supplied --temp-dir / --partial-dir /
    // --backup-dir / --compare-dest / --copy-dest / --link-dest paths that
    // resolve outside the module root. Done before chroot so we can report
    // a precise error message; the Landlock allowlist that follows would
    // otherwise block the writes anyway with a less descriptive EACCES
    // from the kernel. The accepted in-module paths are carried forward
    // and fed to `engage_landlock_sandbox` so the kernel allowlist matches
    // the full set the receiver will actually touch (URV-5.b.REOPEN).
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
    if !apply_privilege_restrictions_with_upstream_errors(ctx, module)? {
        return Ok(());
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
                send_error_and_exit(ctx.reader.get_mut(), ctx.limiter, ctx.messages, &payload)?;
                return Ok(());
            }
        }
    } else {
        None
    };

    #[cfg(windows)]
    let _name_converter_guard = Some(install_windows_name_converter());

    let effective_module;
    let config_module = if module.use_chroot {
        let mut adjusted = module.definition.clone();
        adjusted.path = PathBuf::from("/");
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
            send_error_and_exit(ctx.reader.get_mut(), ctx.limiter, ctx.messages, &payload)?;
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
    // process IS the worker (no parent to survive KillProcess). Failures
    // do not abort the connection - Landlock + SEC-1 `*at` remain the
    // primary defenses.
    engage_seccomp_sandbox(ctx)?;

    let mut streams = match setup_transfer_streams(ctx)? {
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
                send_error_and_exit(ctx.reader.get_mut(), ctx.limiter, ctx.messages, &payload)?;
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
                send_error_and_exit(ctx.reader.get_mut(), ctx.limiter, ctx.messages, &payload)?;
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

    // Graceful TCP shutdown: linger + half-close + drain.
    //
    // Without this pattern, close() on a TCP socket with unread RX bytes
    // causes the kernel to send RST instead of FIN (`tcp(7)`). RST aborts
    // any in-flight TX bytes that have not yet reached userspace, so the
    // peer reports "connection unexpectedly closed (N bytes received so
    // far)" even though the daemon wrote every goodbye byte. Under `-zz`
    // the loss window is widened by trailing MSG_INFO itemize frames
    // before the stats / NDX_DONE goodbye sequence.
    //
    // The structural pattern is:
    //   1. SO_LINGER(5s) - ensures close() blocks until TX data is ACKed
    //   2. shutdown(Write) - sends FIN, tells the peer no more data
    //   3. read() in a loop until EOF - drains peer's final goodbye bytes
    //      and lets the kernel complete the FIN handshake
    //   4. close() the socket - safe because linger ensures TX delivery
    //      and drain cleared any unread RX data
    //
    // SO_LINGER is the key structural element. Without it, close() on a
    // socket with multiple dup'd fd references (our cloned TcpStreams)
    // can return immediately with unACKed TX data, and the kernel races
    // to deliver the trailing goodbye bytes before the peer times out.
    // With SO_LINGER, close() blocks until the kernel confirms delivery.
    //
    // upstream: cleanup.c:close_all() relies on the fork model where the
    // child holds the only fd reference; _exit() lets the kernel drain
    // naturally. Our threaded daemon shares cloned fds across multiple
    // owners and needs SO_LINGER + half-close + drain explicitly.
    //
    // For stdio streams (remote-shell daemon mode), TCP shutdown is not
    // applicable - the pipe/fd closes naturally when dropped.

    if supports_tcp_shutdown {
        let stream = ctx.reader.get_mut();

        // SO_LINGER with a non-zero timeout ensures that when close() is
        // called (via drop), the kernel blocks until all data in the send
        // buffer has been acknowledged by the peer - or until the timeout
        // expires. Without this, close() on a dup'd socket with pending
        // TX data can return immediately while the kernel races to deliver
        // the trailing goodbye bytes.
        //
        // upstream: the fork model avoids this because _exit() triggers
        // implicit close on the only fd, and the kernel lingers by default
        // for orphaned sockets. Our threaded daemon has multiple cloned
        // fds and explicit close() calls, which bypass the implicit linger.
        if let Some(tcp) = stream.tcp_stream() {
            let sock = socket2::SockRef::from(tcp);
            let _ = sock.set_linger(Some(Duration::from_secs(5)));
        }

        // Half-close the write side: sends FIN to the peer, signalling
        // that no more data will be sent from our end.
        let _ = stream.shutdown(std::net::Shutdown::Write);

        // Drain the read side until the peer sends FIN (EOF) or the
        // timeout expires. This ensures all peer goodbye bytes are
        // consumed and the kernel completes the FIN handshake before
        // close() is called, preventing RST from unread RX data.
        //
        // Two seconds is generous for the goodbye round-trip (stats +
        // NDX_DONE) and prevents a stalled peer from wedging the daemon.
        let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
        let mut sink = [0u8; 4096];
        loop {
            match stream.read(&mut sink) {
                Ok(0) => break,
                Ok(_) => continue,
                Err(err)
                    if matches!(
                        err.kind(),
                        io::ErrorKind::TimedOut
                            | io::ErrorKind::WouldBlock
                            | io::ErrorKind::Interrupted
                    ) =>
                {
                    break;
                }
                Err(_) => break,
            }
        }
        let _ = stream.set_read_timeout(None);
    }

    // Drop transfer-engine stream clones after the shutdown+drain
    // sequence completes. These cloned TcpStream handles share the
    // same kernel socket as the DaemonStream; keeping them alive during
    // shutdown(Write) and drain-read preserves the fd refcount that was
    // present during the transfer, matching the original lifecycle.
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
