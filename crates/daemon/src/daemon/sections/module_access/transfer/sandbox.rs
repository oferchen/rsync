// Privilege drop, chroot, path validation, and LSM sandbox engagement
// (Landlock + seccomp) applied before the transfer engine runs.
/// Applies chroot and privilege restrictions, sending upstream-compatible
/// `@ERROR` messages on failure.
///
/// Upstream sends distinct error strings for each failure type:
/// - `@ERROR: chroot failed` (clientserver.c:985)
/// - `@ERROR: setgid failed` (clientserver.c:1024)
/// - `@ERROR: setgroups failed` (clientserver.c:1031)
/// - `@ERROR: setuid failed` (clientserver.c:1053)
///
/// Returns `Ok(Some(outcome))` when restrictions applied successfully or were
/// not configured; `outcome.chroot_applied` records whether the process is
/// actually chrooted (false after a rootless auto-fallback). Returns `Ok(None)`
/// after sending an error to the client.
///
/// A `chroot()`/`setgid()`/`setuid()` syscall failure runs the module's
/// `post-xfer exec` hook before returning: upstream: clientserver.c:964-1040 -
/// these syscalls execute after the post-xfer-exec fork point (908-933), so
/// the waiting parent still observes the child's exit and still fires the
/// hook. A `resolve_drop_target` uid/gid *name* resolution failure does not:
/// upstream: clientserver.c:784-786/657-659 - that lookup runs before the
/// fork point, so the waiting parent never sees a child at all.
/// Publishes this module's `insecure links` opt-out for the ownership walk.
///
/// upstream: `syscall.c:117-126` `symlink_optout_allowed()` - a daemon reads
/// ONLY `lp_insecure_links(module_id)`. A client that sends `--insecure-links`
/// cannot relax a daemon's confinement, which is why this takes the served
/// module and never a peer-supplied flag.
///
/// Called on EVERY success exit below, including the early return taken when
/// neither chroot nor a privilege drop is configured - the commonest rootless
/// deployment, and precisely the path an install placed after the chroot would
/// skip.
fn publish_module_confinement(module: &ModuleRuntime, root: &Path, chrooted: bool) {
    fast_io::confinement::install_daemon_session(fast_io::confinement::ModuleState {
        root: Some(root.to_path_buf()),
        chrooted,
        selected: true,
        insecure_links: fast_io::confinement::ModuleInsecureLinks::from_module_config(
            module.insecure_links,
        ),
    });
}

fn apply_privilege_restrictions_with_upstream_errors(
    ctx: &mut ModuleRequestContext<'_>,
    module: &ModuleRuntime,
    auth_user: Option<&str>,
    client_args: &[String],
) -> io::Result<Option<PrivilegeOutcome>> {
    // upstream: clientserver.c:778-779 - `uid = MY_UID(); am_root = (uid ==
    // ROOT_UID)`. A root daemon drops to `nobody:nobody` by default even when
    // the module sets no explicit uid/gid.
    let am_root = daemon_is_root();
    let needs_chroot = module.use_chroot;
    let needs_privdrop = am_root || module.uid.is_some() || module.gid.is_some();

    if !needs_chroot && !needs_privdrop {
        publish_module_confinement(module, &module.path, false);
        return Ok(Some(PrivilegeOutcome::not_chrooted()));
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

    // Resolve the identity BEFORE the chroot, because resolving it is a name
    // lookup and the chroot is what makes names unresolvable.
    //
    // upstream: clientserver.c:831-880 resolves the uid (`user_to_uid`, :833)
    // and the whole group set (`want_all_groups` :849, `add_a_group` :698-707,
    // the `NOBODY_GROUP` default :873) while still outside any jail; the
    // `chroot()` does not happen until :1050. Every impure arm of
    // `resolve_drop_target` goes through NSS - the `nobody` user, the
    // `nobody`/`nogroup` group, and the `gid = *` supplementary-group
    // enumeration - so a jail that does not contain `/etc/passwd`, `/etc/group`
    // and the NSS modules cannot answer them. Resolving after the chroot turns
    // the `nobody` default that a root daemon applies to every module without
    // an explicit numeric `uid` into `@ERROR: invalid uid nobody`, and the
    // module never serves.
    //
    // Only the `setgid`/`setgroups`/`setuid` SYSCALLS need to run after the
    // chroot (they need the root privileges the chroot also needs); those stay
    // below. upstream: clientserver.c:1024/1031/1053.
    let drop_target = if needs_privdrop {
        match resolve_drop_target(module, am_root) {
            Ok(target) => Some(target),
            Err(err) => {
                // A uid/gid NAME that fails to resolve (here the `nobody`
                // default) is a distinct failure point from the setgid/setuid
                // SYSCALL failures handled below: it yields `@ERROR: invalid
                // uid <name>` / `@ERROR: invalid gid <name>` with a matching
                // FLOG `Invalid uid/gid <name>`.
                // upstream: clientserver.c:836-838 (uid) / 702-703 (gid). This
                // lookup runs before the post-xfer-exec fork point (908), so
                // no `post-xfer exec` hook fires here.
                let (flog, error) = err.upstream_reply();
                let message = rsync_error!(1, flog).with_role(Role::Daemon);
                log_message(log_sink, &message);
                send_error(ctx.reader.get_mut(), ctx.limiter, &error)?;
                return Ok(None);
            }
        }
    } else {
        None
    };

    // upstream: clientserver.c:1040-1057 - chroot, then chdir into the served
    // root. Only the privilege-drop SYSCALLS follow it; the identity they drop
    // to was resolved above.
    // A rootless auto-fallback (unset `use chroot` + failing probe) yields
    // `Ok(false)`; an explicit `use chroot = yes` that fails is fatal.
    let mut chroot_applied = false;
    let mut inner_module_path = None;
    if needs_chroot {
        match chroot_or_fallback(module, log_sink) {
            Ok((applied, inner)) => {
                chroot_applied = applied;
                if applied {
                    inner_module_path = Some(inner);
                }
            }
            Err(err) => {
                // Operator demanded chroot explicitly: a failure is fatal.
                // upstream: clientserver.c:1052 - `@ERROR: chroot failed\n`
                // upstream: clientserver.c:694 - `@ERROR: chdir failed\n`
                let text = err.to_string();
                let error = if text.contains("chdir") {
                    AtError::ChdirFailed
                } else {
                    AtError::ChrootFailed
                };
                send_error(ctx.reader.get_mut(), ctx.limiter, &error)?;
                let host_owned = ctx.host_display().to_owned();
                run_post_xfer_finalizer(
                    ctx,
                    module,
                    &host_owned,
                    auth_user,
                    client_args,
                    MODULE_ABORT_EXIT_CODE,
                );
                return Ok(None);
            }
        }
    }

    if let Some(target) = drop_target {
        if target.uid.is_some() || !target.gids.is_empty() {
            if let Err(err) = drop_privileges(target.uid, &target.gids, log_sink) {
                // Distinguish upstream error messages based on the error text.
                // upstream: clientserver.c:1024/1031/1053
                let text = err.to_string();
                let error = if text.contains("setgroups") {
                    AtError::SetgroupsFailed
                } else if text.contains("setuid") {
                    AtError::SetuidFailed
                } else {
                    AtError::SetgidFailed
                };
                send_error(ctx.reader.get_mut(), ctx.limiter, &error)?;
                let host_owned = ctx.host_display().to_owned();
                run_post_xfer_finalizer(
                    ctx,
                    module,
                    &host_owned,
                    auth_user,
                    client_args,
                    MODULE_ABORT_EXIT_CODE,
                );
                return Ok(None);
            }
        }
    }

    publish_module_confinement(
        module,
        inner_module_path.as_deref().unwrap_or(&module.path),
        chroot_applied,
    );

    Ok(Some(PrivilegeOutcome {
        chroot_applied,
        inner_module_path,
    }))
}

/// Result of applying a module's chroot and privilege restrictions.
struct PrivilegeOutcome {
    /// Whether `chroot()` was actually applied. `false` when the module runs
    /// without chroot, or when `use chroot` was unset and the runtime probe
    /// failed (rootless fallback) - downstream path handling must then treat
    /// the module as non-chrooted.
    ///
    /// upstream: clientserver.c:833-864 - the effective `use_chroot` decides
    /// whether the module path is rewritten to the post-chroot inner path.
    chroot_applied: bool,
    /// The post-chroot working directory to serve from, when `chroot_applied`
    /// is `true`. `/` unless the module path carried a `/./` inner/outer
    /// marker, in which case it is the normalized remainder after the
    /// marker (e.g. `/module` for `path = /var/data/./module`).
    ///
    /// upstream: clientserver.c:847-864 - `module_dir` after the `/./` split.
    inner_module_path: Option<PathBuf>,
}

impl PrivilegeOutcome {
    /// Outcome for a module served without chroot.
    const fn not_chrooted() -> Self {
        Self {
            chroot_applied: false,
            inner_module_path: None,
        }
    }
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

    let error = AtError::message(format!(
        "module '{}' path does not exist: {}",
        sanitize_module_identifier(ctx.request),
        module.path.display()
    ));
    send_error(ctx.reader.get_mut(), ctx.limiter, &error)?;

    if let Some(log) = ctx.log_sink {
        let text = format!(
            "module '{}' path validation failed for {} ({}): path does not exist: {}",
            ctx.request,
            ctx.host_display(),
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
/// upstream: util1.c:1138 `sanitize_path` collapses `..` against the
/// module root depth; main.c:867 `check_alt_basis_dirs` warns but does not
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
    privilege_outcome: &PrivilegeOutcome,
) -> io::Result<Option<ValidatedClientPaths>> {
    // chroot + the privilege drop already ran pre-OK. After a chroot the process
    // root IS the module dir, so a client's absolute --link-dest/--temp-dir path
    // resolves under the post-chroot root ("/" or the `/./` inner remainder);
    // validate against that (the kernel chroot already contains every in-module
    // path). Without chroot (unset + rootless fallback, `use chroot = no`, or a
    // non-Linux build) validate against the real module path so SEC-1.p still
    // rejects out-of-tree basis dirs.
    let root_source = if privilege_outcome.chroot_applied {
        privilege_outcome
            .inner_module_path
            .as_deref()
            .unwrap_or_else(|| Path::new("/"))
    } else {
        module.path.as_path()
    };
    let Ok(module_root) = root_source.canonicalize() else {
        // Root path failed to canonicalize - the existence check above already
        // succeeded, so this is a race or a permission problem; let the transfer
        // continue and fail with a more precise error later.
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

/// Why this module's Landlock sandbox is skipped, or `None` to engage it.
///
/// Both arms are per-module operator configuration, and both are skips rather
/// than downgrades because a Landlock ruleset cannot be relaxed once applied -
/// it is inherited by children and only ever narrows. The process-wide
/// operator opt-out ([`SandboxLayer::Landlock`]) is decided by the caller
/// before this runs, so it is deliberately not an arm here.
///
/// - **exec hooks**: rulesets are inherited across `exec()`, so an allowlist
///   pinned to the module path would block hook scripts that live outside it
///   (the common case, e.g. `/usr/local/bin/notify.sh`).
/// - **`insecure links = yes`**: the directive exists to restore the legacy
///   follow-any-symlink behaviour for a module, which necessarily means reading
///   through an in-module symlink that leaves the module tree. A kernel
///   allowlist pinned to `module.path` refuses exactly that, so leaving
///   Landlock engaged does not harden the module - it makes the directive
///   silently inoperative while the daemon still logs that it accepted it.
///   The ownership walk consults the same opt-out
///   (`fast_io::confinement::session_optout_allowed`), so this keeps the two
///   confinement layers agreeing on one operator decision.
///
/// upstream: rsync has no Landlock layer; `syscall.c:123-127`
/// `symlink_optout_allowed()` is the whole of its daemon-side rule, and it is
/// read as `module_id >= 0 && lp_insecure_links(module_id)`.
fn landlock_skip_reason(module: &ModuleRuntime) -> Option<&'static str> {
    if let Some(reason) = exec_hook_skip_reason(module) {
        return Some(reason);
    }
    if module.insecure_links {
        return Some("insecure links = yes (operator opted this module out of path confinement)");
    }
    None
}

/// Where the Landlock allowlist must be pinned once chroot has already run,
/// or why the layer deliberately stands aside.
///
/// `restrict_to_module_paths` opens every root it is handed, and it runs
/// **after** `chroot()`. A pre-chroot absolute path is therefore not a path
/// this process can name any more, which is why the root has to be re-derived
/// from the privilege outcome rather than read off `module.path`.
#[derive(Debug, PartialEq, Eq)]
enum LandlockRoot<'a> {
    /// Pin the allowlist beneath this (currently resolvable) directory.
    Confine(&'a Path),
    /// The chroot root *is* the module root, so the kernel already denies
    /// exactly what the allowlist would deny. Skip, deliberately.
    SubsumedByChroot,
}

/// Selects the Landlock root for a connection whose chroot and privilege drop
/// have already been applied.
///
/// Three cases, and the split mirrors upstream's own predicate for "does the
/// kernel chroot already confine this module?":
///
/// - **Not chrooted** (`use chroot = no`, or the unset rootless auto-fallback):
///   confine to the real module path, as before.
/// - **Chrooted at the module root** (no `/./` marker): upstream sets
///   `module_dir = "/"` and `module_dirlen = 1`, then immediately zeroes that
///   length - clientserver.c:912-925 - so its own
///   `use_secure_symlinks = am_daemon && (!am_chrooted || module_dirlen)`
///   (clientserver.c:1093) evaluates false: with no inner boundary the chroot
///   is the whole confinement. Landlock over the post-chroot `/` would deny
///   nothing extra, and it would actively *narrow* the module: the
///   `READONLY_SYSTEM_PATHS` rules (`/etc`, `/usr`, `/var`, ...) are resolved
///   inside the jail, and a Landlock rule on a deeper directory overrides a
///   shallower one - so a module tree that happens to contain `etc/` or `var/`
///   (a system backup, say) would become read-only. Stand aside.
/// - **Chrooted with a `/./` inner boundary**: the chroot lands on the OUTER
///   path, leaving the inner module boundary unguarded - upstream keeps its
///   secure-symlink path active for exactly this shape (clientserver.c:1084-1093
///   `the kernel chroot confines the outer path but not the inner module`).
///   Confine to the post-chroot inner directory, which is the one root that is
///   both resolvable now and narrower than the jail.
///
/// upstream: rsync has no Landlock layer at all; only the
/// chrooted-vs-inner-boundary predicate above is mirrored from it.
fn landlock_root<'a>(
    module: &'a ModuleRuntime,
    privilege_outcome: &'a PrivilegeOutcome,
) -> LandlockRoot<'a> {
    if !privilege_outcome.chroot_applied {
        return LandlockRoot::Confine(module.path.as_path());
    }
    match privilege_outcome.inner_module_path.as_deref() {
        Some(inner) if inner != Path::new("/") => LandlockRoot::Confine(inner),
        _ => LandlockRoot::SubsumedByChroot,
    }
}

/// Why a kernel sandbox layer must be skipped for a module that runs an
/// `exec()` hook, or `None` when no hook is configured.
///
/// Shared by BOTH layers because both are inherited across `exec()` and
/// neither can be relaxed for the child:
///
/// - **Landlock**: the ruleset is inherited, so an allowlist pinned to
///   `module.path` blocks a hook script living outside it.
/// - **seccomp**: the filter is inherited too, and the worker allowlist
///   deliberately omits `execve` / `fork` / `wait4` - the exact syscalls a
///   hook needs. MEASURED on Linux 7.0 aarch64: with the filter engaged the
///   daemon logs `failed to run post-xfer exec command for module 'data':
///   Operation not permitted (os error 1)` and the hook never runs, while the
///   same config with the filter disabled runs it.
///
/// Keeping one predicate is the point: the layers previously disagreed, so
/// Landlock stood aside for the operator's declared hook while seccomp
/// silently `EPERM`ed it. Widening the allowlist instead would hand `execve`
/// to anything that hijacks the worker, which is the attack the filter exists
/// to stop - so the layer stands aside per module, exactly as Landlock does.
fn exec_hook_skip_reason(module: &ModuleRuntime) -> Option<&'static str> {
    (module.pre_xfer_exec.is_some() || module.post_xfer_exec.is_some())
        .then_some("pre-xfer-exec or post-xfer-exec configured (would block hook exec)")
}

/// Engages the SEC-1.p Landlock LSM allowlist for the receiver path.
///
/// Called immediately after `apply_module_privilege_restrictions` has
/// applied chroot + uid/gid drop so the kernel allowlist covers exactly the
/// writable surface the remainder of the connection needs. The stub on
/// non-Linux targets short-circuits to `Unavailable` so the wire-in does
/// not need `#[cfg]` branching.
///
/// Because it runs post-chroot, the root comes from [`landlock_root`] and not
/// from `module.path`, which by then names nothing this process can open. That
/// helper also decides the one case where the layer stands aside on purpose: a
/// chroot whose root is already the module root.
///
/// `extra_allowed_paths` carries absolute, in-module paths that
/// `validate_client_paths_in_module` admitted from the client args
/// (`--temp-dir` / `--partial-dir` / `--backup-dir` / `--compare-dest` /
/// `--copy-dest` / `--link-dest`). The caller is responsible for the
/// containment check; this helper only forwards the slice to the kernel.
/// Closing URV-5.b.REOPEN: without the widening, a default-on Landlock
/// flip would EACCES the very paths the operator's configuration permits.
///
/// Always returns `Ok(true)`: no Landlock outcome aborts the connection,
/// because SEC-1 `*at` helpers remain the primary defense in every one of
/// them. The `Ok(false)` arm is kept in the signature so a future default-on
/// flip can make a failed install fatal without rippling through the caller.
/// A ruleset the kernel advertised but refused is logged as a WARNING naming
/// the root and the OS error - it is a regression, and after the post-chroot
/// root fix it can no longer be produced by our own path handling.
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
    privilege_outcome: &PrivilegeOutcome,
    extra_allowed_paths: &[&Path],
) -> io::Result<bool> {
    use fast_io::landlock::{
        EnforcementStatus, LandlockOutcome, best_effort_fs_downgrade, is_supported,
        restrict_to_module_paths,
    };

    // Operator opt-out first: it is the most explicit signal and it is
    // process-wide, so it outranks the per-module skips below. Landlock had
    // no escape hatch at all while seccomp had two, which made a Landlock
    // denial the one sandbox failure an operator could not A/B in the field.
    if let Some(var) = SandboxLayer::Landlock.operator_optout_var() {
        if let Some(log) = ctx.log_sink {
            let text = SandboxLayer::Landlock.optout_log_text(ctx.request, var);
            let message = rsync_info!(text).with_role(Role::Daemon);
            log_message(log, &message);
        }
        return Ok(true);
    }

    if let Some(reason) = landlock_skip_reason(module) {
        if let Some(log) = ctx.log_sink {
            let text = format!("module '{}': landlock=skipped reason={reason}", ctx.request);
            let message = rsync_info!(text).with_role(Role::Daemon);
            log_message(log, &message);
        }
        return Ok(true);
    }

    // Chroot already ran, so the root must be re-derived: `module.path` is a
    // pre-chroot absolute path this process can no longer name, and handing it
    // to the kernel produced `landlock setup failed: failed to open <path>: No
    // such file or directory` on every chrooted module - an attempted-and-
    // failed layer reported as a warning rather than as a decision. MEASURED
    // on Linux 7.0 x86_64 before this change, for both `path = /srv/data` and
    // `path = /srv/outer/./inner`.
    let root = match landlock_root(module, privilege_outcome) {
        LandlockRoot::Confine(path) => path,
        LandlockRoot::SubsumedByChroot => {
            if let Some(log) = ctx.log_sink {
                let text = format!(
                    "module '{}': landlock=skipped reason=chroot root is the module root (the kernel chroot already confines this module; see clientserver.c:1093)",
                    ctx.request,
                );
                let message = rsync_info!(text).with_role(Role::Daemon);
                log_message(log, &message);
            }
            return Ok(true);
        }
    };

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

    // Roots: the module root selected above is the always-present writable
    // surface plus any client-supplied alt-basis (`--compare-dest` /
    // `--copy-dest` / `--link-dest`) or relocation (`--temp-dir` /
    // `--partial-dir` / `--backup-dir`) paths that
    // `validate_client_paths_in_module` has already confirmed to resolve
    // beneath that same root (URV-5.b.1) - it derives the root from the very
    // same privilege outcome, so both halves of the allowlist live in the same
    // (post-chroot) namespace. Widening the allowlist to those paths is safe
    // because the containment check already proved they cannot escape the
    // module tree; without the widening, a default-on Landlock flip
    // (URV-5.c.5) would EACCES legitimate writes the operator's configuration
    // permits.
    let mut roots: Vec<&Path> = Vec::with_capacity(1 + extra_allowed_paths.len());
    roots.push(root);
    roots.extend_from_slice(extra_allowed_paths);

    match restrict_to_module_paths(&roots) {
        LandlockOutcome::Enforced(status) => {
            if let Some(log) = ctx.log_sink {
                let message = match status {
                    // Full confinement: routine, log at info.
                    EnforcementStatus::FullyEnforced => {
                        let text = format!(
                            "module '{}': landlock fully enforced over {} root(s)",
                            ctx.request,
                            roots.len(),
                        );
                        rsync_info!(text).with_role(Role::Daemon)
                    }
                    // Best-effort downgrade silently dropped rights because the
                    // kernel is too old. Do NOT bury this at info: name exactly
                    // what is missing so the operator understands the sandbox is
                    // weaker than intended - the lost `refer` right breaks
                    // cross-directory renames (--delay-updates / --backup-dir).
                    EnforcementStatus::PartiallyEnforced => {
                        let dropped = best_effort_fs_downgrade()
                            .unwrap_or_else(|| "some requested access rights".to_owned());
                        let text = format!(
                            "module '{}': landlock PARTIALLY enforced over {} root(s) - this kernel's Landlock ABI is missing {}. The sandbox is weaker than requested; upgrade to Linux 5.19+ (6.2+ for truncate, 6.10+ for ioctl_dev) for the full allowlist.",
                            ctx.request,
                            roots.len(),
                            dropped,
                        );
                        rsync_warning!(text).with_role(Role::Daemon)
                    }
                    // The kernel accepted the ruleset but applied nothing:
                    // equivalent to no sandbox. Warn - SEC-1 *at* helpers are
                    // now the only defense.
                    EnforcementStatus::NotEnforced => {
                        let text = format!(
                            "module '{}': landlock NOT enforced - the kernel accepted the ruleset but applied no confinement; SEC-1 *at* helpers remain the sole defense.",
                            ctx.request,
                        );
                        rsync_warning!(text).with_role(Role::Daemon)
                    }
                };
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
            // still provides the primary defense. Name the root we asked the
            // kernel to pin: the previous message reported only the OS error,
            // which read as an environment problem when it was in fact a
            // pre-chroot path handed to a post-chroot process.
            if let Some(log) = ctx.log_sink {
                let text = format!(
                    "module '{}': landlock setup failed for root {}: {err}; relying on SEC-1 *at* defense",
                    ctx.request,
                    root.display(),
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
/// site does not need `#[cfg]` branching. The operator opt-out
/// (`OC_RSYNC_NO_SECCOMP` / `OC_RSYNC_DAEMON_SECCOMP=0`) is decided here
/// rather than read out of `Unavailable`, which cannot distinguish a build
/// without seccomp from a build whose operator turned it off. Construction or installation
/// failure is logged as a warning and the connection continues - SEC-1
/// `*at` helpers and Landlock remain the primary defenses.
///
/// **Modules with an exec hook are skipped**, for the same reason Landlock
/// skips them: the filter is inherited across `exec()` and the worker
/// allowlist omits `execve` / `fork` / `wait4`, so an engaged filter turns
/// the operator's configured `pre-xfer exec` / `post-xfer exec` into an
/// `EPERM` failure instead of hardening anything.
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
fn engage_seccomp_sandbox(
    ctx: &mut ModuleRequestContext<'_>,
    module: &ModuleRuntime,
) -> io::Result<()> {
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

    // Operator opt-out. Checked here as well as inside
    // `apply_worker_seccomp_filter` because the filter can only answer
    // `Unavailable`, which conflates "this build has no seccomp" with "the
    // operator turned it off" - two facts an operator reading the log needs
    // to tell apart.
    if let Some(var) = SandboxLayer::Seccomp.operator_optout_var() {
        if let Some(log) = ctx.log_sink {
            let text = SandboxLayer::Seccomp.optout_log_text(ctx.request, var);
            let message = rsync_info!(text).with_role(Role::Daemon);
            log_message(log, &message);
        }
        return Ok(());
    }

    // Exec hooks: the filter is inherited across `exec()` and the worker
    // allowlist has no `execve` / `fork` / `wait4`, so leaving it engaged
    // does not harden the module - it makes the operator's configured hook
    // fail with EPERM. Same predicate, same decision as Landlock.
    if let Some(reason) = exec_hook_skip_reason(module) {
        if let Some(log) = ctx.log_sink {
            let text = format!("module '{}': seccomp=skipped reason={reason}", ctx.request);
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
