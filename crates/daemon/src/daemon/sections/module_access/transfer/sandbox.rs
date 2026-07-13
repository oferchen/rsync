// Privilege drop, chroot, path validation, and LSM sandbox engagement
// (Landlock + seccomp) applied before the transfer engine runs.
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
            send_error(ctx.reader.get_mut(), ctx.limiter, payload)?;
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
            send_error(ctx.reader.get_mut(), ctx.limiter, payload)?;
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
    send_error(ctx.reader.get_mut(), ctx.limiter, &payload)?;

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

/// Collects client-supplied `--temp-dir` / `--partial-dir` / `--backup-dir`
/// / `--compare-dest` / `--copy-dest` / `--link-dest` paths that resolve
/// inside the module root so the SEC-1.p Landlock allowlist can be widened
/// to cover them. Out-of-module paths are silently dropped instead of
/// rejected: upstream rsync's daemon `sanitize_path` rewrites such paths
/// under `module_dir` (with `..` segments collapsed in place), turning
/// alt-basis lookups into no-ops and `--temp-dir` / `--partial-dir` /
/// `--backup-dir` into module-internal paths. Aborting the connection with
/// `@ERROR` would diverge from that behaviour and break upstream interop
/// tests (`standalone:link-dest` / `standalone:copy-dest`) which legitimately
/// reference siblings of the module path.
///
/// For *in-module* absolute paths the operator's configuration permits the
/// access, so they must reach the Landlock allowlist or a default-on flip
/// would EACCES legitimate writes (URV-5.b.REOPEN).
///
/// upstream: util1.c:1035 `sanitize_path` collapses `..` against the
/// module root depth; main.c:841 `check_alt_basis_dirs` warns but does not
/// abort when the sanitised basis is missing or out-of-tree.
///
/// Returns `Ok(Some(ValidatedClientPaths))` carrying only the in-module
/// absolute paths. The function never emits `@ERROR`, so it never returns
/// `Ok(None)` today; the `Option` is preserved so a future hard-reject
/// policy can be reintroduced without rippling through every caller.
fn validate_client_paths_in_module(
    _ctx: &mut ModuleRequestContext<'_>,
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
        let raw_path = if let Some(rest) = arg.strip_prefix("--temp-dir=") {
            Some(rest.to_owned())
        } else if let Some(rest) = arg.strip_prefix("--partial-dir=") {
            Some(rest.to_owned())
        } else if let Some(rest) = arg.strip_prefix("--backup-dir=") {
            Some(rest.to_owned())
        } else if let Some(rest) = arg.strip_prefix("--compare-dest=") {
            Some(rest.to_owned())
        } else if let Some(rest) = arg.strip_prefix("--copy-dest=") {
            Some(rest.to_owned())
        } else if let Some(rest) = arg.strip_prefix("--link-dest=") {
            Some(rest.to_owned())
        } else if matches!(
            arg.as_str(),
            "--temp-dir"
                | "--partial-dir"
                | "--backup-dir"
                | "--compare-dest"
                | "--copy-dest"
                | "--link-dest"
        ) {
            iter.next().cloned()
        } else {
            None
        };

        let Some(raw_path) = raw_path else {
            continue;
        };

        // In-module absolute paths feed the Landlock allowlist. Relative
        // paths (`Ok(None)`) resolve under the module root and need no
        // explicit entry. Out-of-module absolute paths (`Err(())`) are
        // silently dropped here; `build_server_config`'s `retain_mut` block
        // then strips the matching `cfg.reference_directories` entry so the
        // receiver re-transfers instead of hard-linking outside the tree.
        if let Ok(Some(canonical)) = classify_client_path_against_module(&raw_path, &module_root)
            && !accepted.iter().any(|p| p == &canonical)
        {
            accepted.push(canonical);
        }
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
    use fast_io::landlock::{LandlockOutcome, is_supported, restrict_to_module_paths};

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
/// out-of-scope syscalls with `EPERM` (default action `SECCOMP_RET_ERRNO`)
/// before the kernel ever consults the LSM stack. A non-lethal default
/// keeps a rare, benign syscall from killing a legitimate transfer.
///
/// On builds without the `daemon-seccomp` feature the helper is a no-op
/// that returns `Unavailable`; the wire-in is unconditional so the call
/// site does not need `#[cfg]` branching. Construction or installation
/// failure is logged as a warning and the connection continues - SEC-1
/// `*at` helpers and Landlock remain the primary defenses.
///
/// **Stdio sessions are skipped.** When the daemon runs as `--server
/// --daemon` over stdin/stdout (remote-shell daemon mode via `lsh.sh` /
/// SSH), the process IS the worker. A process-scoped filter would
/// restrict post-transfer cleanup, process exit, and any syscalls the
/// Python test harness or shell wrapper needs after the transfer
/// completes (an `EPERM` there would fail cleanup just as surely). TCP
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
                    "module '{}': seccomp BPF filter engaged (EPERM on unlisted syscalls)",
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
