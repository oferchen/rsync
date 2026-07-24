/// Applies a chroot jail.
///
/// Delegates to `platform::privilege::apply_chroot()`. Failure is propagated to
/// the caller so the daemon refuses to serve rather than continuing without the
/// requested isolation.
///
/// On success, fires the `--debug=CHDIR` emission to mirror upstream
/// `util1.c:1168-1169` (`[%s] change_dir(%s)`). Upstream's
/// `clientserver.c:987` calls `change_dir(module_chdir, CD_NORMAL)`
/// immediately after `chroot(module_chdir)`; in oc-rsync the
/// `platform::privilege::apply_chroot` call performs both the chroot and the
/// follow-up `chdir("/")`, so the post-syscall `curr_dir` is `"/"` (the new
/// root). The emission carries the upstream `who_am_i()` role string for the
/// daemon's pre-fork code path (`"Receiver"`, see `rsync.c:823-830`).
///
/// Caveat: chroot is process-wide, so in our thread-per-connection model this
/// affects every concurrent session. Per-module chroot only works correctly
/// when the daemon serves a single module or all modules share the same root.
/// See `docs/DAEMON_PROCESS_MODEL.md`.
///
/// upstream: `clientserver.c:978-987` `rsync_module()` - `chroot(module_chdir)`
/// then `change_dir(module_chdir, CD_NORMAL)` after sanitising the module
/// path; `@ERROR: chroot failed` returned to the client when chroot fails.
#[cfg(unix)]
fn apply_chroot(module_path: &Path, log_sink: &SharedLogSink) -> io::Result<()> {
    if let Err(err) = platform::privilege::apply_chroot(module_path) {
        let text = format!("chroot to '{}' failed: {}", module_path.display(), err);
        let message = rsync_error!(1, text).with_role(Role::Daemon);
        log_message(log_sink, &message);
        return Err(err);
    }

    protocol::chdir::trace_change_dir(protocol::chdir::ChdirRole::PreForkReceiver, "/");

    let text = format!("chroot applied: '{}'", module_path.display());
    let message = rsync_info!(text).with_role(Role::Daemon);
    log_message(log_sink, &message);

    Ok(())
}

/// No-op chroot stub for non-Unix platforms.
#[cfg(not(unix))]
fn apply_chroot(_module_path: &Path, _log_sink: &SharedLogSink) -> io::Result<()> {
    Ok(())
}

/// Drops process privileges to the specified uid and gid.
///
/// Delegates to `platform::privilege::drop_privileges()`. The underlying call
/// sequence is `setgid` -> `setgroups` -> `setuid` (upstream
/// clientserver.c:1022-1047), with the irreversible `setuid` last. Any failure
/// is propagated; the
/// daemon must refuse to serve rather than continue as root.
///
/// Caveat: on Linux the setuid system call is propagated to every thread of
/// the process, so a per-module privilege drop affects all concurrent sessions
/// in our thread-per-connection model. Per-module `uid`/`gid` directives only
/// work correctly when the daemon serves a single module. Operators that need
/// privilege separation across modules should run a separate daemon per
/// identity.
///
/// upstream: `clientserver.c:1006-1044` `rsync_module()` - `setgid`/
/// `setgroups`/`setuid` after chroot. Each failure returns `@ERROR: setgid
/// failed`, `@ERROR: setgroups failed`, or `@ERROR: setuid failed` and the
/// connection is dropped.
fn drop_privileges(
    uid: Option<u32>,
    gids: &[u32],
    log_sink: &SharedLogSink,
) -> io::Result<()> {
    if let Err(err) = platform::privilege::drop_privileges(uid, gids) {
        let text = format!("drop_privileges(uid={uid:?}, gids={gids:?}) failed: {err}");
        let message = rsync_error!(1, text).with_role(Role::Daemon);
        log_message(log_sink, &message);
        return Err(err);
    }

    if let Some(&primary) = gids.first() {
        let text = format!("dropped group privileges to gid {primary} (group set: {gids:?})");
        let message = rsync_info!(text).with_role(Role::Daemon);
        log_message(log_sink, &message);
    }

    if let Some(uid_val) = uid {
        let text = format!("dropped user privileges to uid {uid_val}");
        let message = rsync_info!(text).with_role(Role::Daemon);
        log_message(log_sink, &message);
    }

    Ok(())
}

/// Splits a module path at the first `/./` marker into the outer chroot
/// root and the inner post-chroot working directory.
///
/// A path without `/./` chroots into the whole path, starting the session
/// at the new root (inner = `/`). A path with `/./` chroots into only the
/// portion before the marker and starts the session at the (still-absolute)
/// portion after it - upstream's inner/outer split, which leaves siblings
/// of the inner directory reachable inside the jail for modules that need
/// to share a parent directory with other served paths.
///
/// upstream: clientserver.c:847-864 `rsync_module()` - `strstr(module_dir,
/// "/./")` locates the marker; the outer half becomes `module_chdir` (the
/// chroot target) and the normalized remainder becomes `module_dir` (the
/// post-chroot `change_dir` target).
fn split_chroot_path(path: &Path) -> (PathBuf, PathBuf) {
    let Some((outer, inner)) = path.to_str().and_then(|s| s.split_once("/./")) else {
        return (path.to_path_buf(), PathBuf::from("/"));
    };
    let outer = if outer.is_empty() { "/" } else { outer };
    let inner = inner.trim_start_matches('/');
    let inner_path = if inner.is_empty() {
        PathBuf::from("/")
    } else {
        PathBuf::from(format!("/{inner}"))
    };
    (PathBuf::from(outer), inner_path)
}

/// Resolves the effective per-module chroot decision, mirroring upstream's
/// tri-state `use_chroot` (`0`/`1`/unset).
///
/// - An explicit `use chroot = yes|no` always wins.
/// - Unset with a `/./` inner/outer marker in the path is treated as an
///   implicit `yes`: the path itself signals chroot intent, so no probe runs.
/// - Unset otherwise probes chroot capability with a no-op `chroot("/")`;
///   success resolves to `yes`, failure (typically `EPERM`, the common
///   rootless-daemon case) resolves to `no` with a log notice matching
///   upstream's wording.
///
/// upstream: clientserver.c:706,831-844 `rsync_module()`.
fn resolve_use_chroot(module: &ModuleDefinition, log_sink: &SharedLogSink) -> bool {
    if module.use_chroot_explicit {
        return module.use_chroot;
    }
    if module.path.to_str().is_some_and(|s| s.contains("/./")) {
        return true;
    }
    match platform::privilege::probe_chroot_capability() {
        Ok(()) => true,
        Err(err) => {
            let notice =
                format!("chroot test failed: {err}. Switching 'use chroot' from unset to false.");
            let message = rsync_warning!(notice).with_role(Role::Daemon);
            log_message(log_sink, &message);
            false
        }
    }
}

/// Applies chroot for a module, resolving the tri-state `use chroot`
/// decision and (when enabled) chrooting into the possibly `/./`-split
/// module path.
///
/// Returns `Ok((chroot_applied, inner_dir))`. `chroot_applied` is `false`
/// when chroot is disabled outright (`use chroot = no`) or was unset and the
/// capability probe failed; `inner_dir` is the post-chroot working directory
/// the caller must serve from (`/` unless the path carried a `/./`
/// inner/outer marker). Once `use_chroot` resolves true - explicitly, via
/// `/./`, or via a successful probe - a failure of the real `chroot(2)` call
/// is always fatal, matching upstream's unconditional `@ERROR: chroot
/// failed` once the tri-state is settled.
///
/// upstream: clientserver.c:831-990 `rsync_module()`.
fn chroot_or_fallback(
    module: &ModuleDefinition,
    log_sink: &SharedLogSink,
) -> io::Result<(bool, PathBuf)> {
    if !resolve_use_chroot(module, log_sink) {
        return Ok((false, module.path.clone()));
    }

    let (outer, inner) = split_chroot_path(&module.path);
    apply_chroot(&outer, log_sink)?;
    Ok((true, inner))
}

/// The uid and complete group set the daemon will drop to for a connection.
///
/// upstream: clientserver.c:779-822 `rsync_module()` resolves the effective
/// identity before `setgid`/`setgroups`/`setuid`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct DropTarget {
    /// Target uid, or `None` to leave the user identity unchanged.
    uid: Option<u32>,
    /// Complete group set (primary first). Empty leaves groups unchanged.
    gids: Vec<u32>,
}

/// A module identity that could not be resolved during connection setup,
/// tagged with the exact upstream `@ERROR` wording and the offending NAME.
///
/// Upstream distinguishes two failure points with two different strings: a
/// `user_to_uid()`/`group_to_gid()` NAME lookup that returns no match yields
/// `@ERROR: invalid uid <name>` / `@ERROR: invalid gid <name>`
/// (clientserver.c:783-786 / 656-658), which is separate from the later
/// `setuid`/`setgid` SYSCALL failures (clientserver.c:1053 / 1024). oc-rsync
/// resolves configured `uid`/`gid` names at parse time, so the only NAME that
/// reaches connection-time resolution is the `nobody` default a root daemon
/// falls back to when the module sets no explicit identity.
#[derive(Debug)]
enum DropResolutionError {
    /// A user NAME (the `nobody` default) failed to resolve.
    /// upstream: clientserver.c:785 `@ERROR: invalid uid %s`.
    InvalidUid(String),
    /// A group NAME (the `nobody` default) failed to resolve.
    /// upstream: clientserver.c:658 `@ERROR: invalid gid %s`.
    InvalidGid(String),
    /// A `gid = *` group enumeration failed - not a name lookup.
    /// upstream: clientserver.c:797 `want_all_groups`.
    GroupEnumeration(io::Error),
}

impl DropResolutionError {
    /// Maps the resolution failure to upstream's FLOG log text and the exact
    /// `@ERROR:` payload sent to the client, keeping both byte-identical to
    /// upstream so the client sees the same greeting-phase reply. This is the
    /// single point that decides resolution-vs-syscall wording, so the
    /// `invalid uid/gid` and `setuid/setgid failed` strings can never be
    /// collapsed again.
    ///
    /// upstream: clientserver.c:784-786 (`Invalid uid %s` / `@ERROR: invalid
    /// uid %s`) and clientserver.c:657-658 (`Invalid gid %s` / `@ERROR: invalid
    /// gid %s`).
    fn upstream_reply(&self) -> (String, String) {
        match self {
            Self::InvalidUid(name) => (
                format!("Invalid uid {name}"),
                INVALID_UID_PAYLOAD.replace("{uid}", name),
            ),
            Self::InvalidGid(name) => (
                format!("Invalid gid {name}"),
                INVALID_GID_PAYLOAD.replace("{gid}", name),
            ),
            Self::GroupEnumeration(err) => (
                format!("group enumeration failed: {err}"),
                SETUID_FAILED_PAYLOAD.to_owned(),
            ),
        }
    }
}

impl From<DropResolutionError> for io::Error {
    fn from(err: DropResolutionError) -> Self {
        match err {
            DropResolutionError::GroupEnumeration(inner) => inner,
            DropResolutionError::InvalidUid(name) => {
                io::Error::new(io::ErrorKind::NotFound, format!("invalid uid {name}"))
            }
            DropResolutionError::InvalidGid(name) => {
                io::Error::new(io::ErrorKind::NotFound, format!("invalid gid {name}"))
            }
        }
    }
}

/// Returns whether the daemon process currently has an effective uid of 0.
///
/// Delegates to the `platform` crate, which owns the `nix` dependency on every
/// Unix target (the daemon crate links `nix` only on Linux).
///
/// upstream: clientserver.c:779 `am_root = (uid == ROOT_UID)`. Non-Unix
/// platforms have no root uid and use the impersonation path instead.
fn daemon_is_root() -> bool {
    platform::privilege::is_effective_root()
}

/// Resolves the effective uid/gid drop for a module, applying upstream's
/// default-to-`nobody` policy when the daemon runs as root.
///
/// - An explicit module `uid` always wins; otherwise a root daemon defaults to
///   the `nobody` user (upstream clientserver.c:781
///   `am_root ? NOBODY_USER : NULL`).
/// - An explicit module `gid` list wins; `gid = *` expands to the target user's
///   full group set (clientserver.c:797 `want_all_groups`); otherwise a root
///   daemon defaults to the `nobody` group (clientserver.c:820-821).
/// - When the daemon is not root and the module sets no `uid`/`gid`, nothing is
///   dropped, matching `set_uid = 0` with an empty `gid_list`.
///
/// upstream: clientserver.c:779-822 `rsync_module()`.
fn resolve_drop_target(
    module: &ModuleDefinition,
    am_root: bool,
) -> Result<DropTarget, DropResolutionError> {
    let uid = match module.uid {
        Some(explicit) => Some(explicit),
        None if am_root => Some(
            resolve_nobody_uid()
                .map_err(|_| DropResolutionError::InvalidUid("nobody".to_owned()))?,
        ),
        None => None,
    };

    let gids = match module.gid.as_ref() {
        Some(GidSetting::List(list)) => list.clone(),
        Some(GidSetting::AllUserGroups { extra }) => {
            let mut all =
                resolve_all_user_groups(uid).map_err(DropResolutionError::GroupEnumeration)?;
            for gid in extra {
                if !all.contains(gid) {
                    all.push(*gid);
                }
            }
            all
        }
        None if am_root => vec![
            resolve_nobody_gid()
                .map_err(|_| DropResolutionError::InvalidGid("nobody".to_owned()))?,
        ],
        None => Vec::new(),
    };

    Ok(DropTarget { uid, gids })
}

/// Resolves the `nobody` user to its uid via NSS.
///
/// upstream: clientserver.c:783 `user_to_uid(NOBODY_USER, ...)`; NOBODY_USER is
/// `"nobody"` (config.h). Errors when the account is absent, mirroring
/// upstream's `@ERROR: invalid uid nobody`.
#[cfg(unix)]
fn resolve_nobody_uid() -> io::Result<u32> {
    metadata::id_lookup::lookup_user_by_name(b"nobody")?.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "cannot drop to default user: no 'nobody' account (set an explicit uid)",
        )
    })
}

/// Non-Unix stub: only reachable via `am_root`, which is always false off Unix.
#[cfg(not(unix))]
fn resolve_nobody_uid() -> io::Result<u32> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "default 'nobody' user drop is Unix-only",
    ))
}

/// Resolves the `nobody` group to its gid via NSS.
///
/// upstream: clientserver.c:821 `add_a_group(f_out, NOBODY_GROUP)`; NOBODY_GROUP
/// is `"nobody"` (config.h).
#[cfg(unix)]
fn resolve_nobody_gid() -> io::Result<u32> {
    metadata::id_lookup::lookup_group_by_name(b"nobody")?.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "cannot drop to default group: no 'nobody' group (set an explicit gid)",
        )
    })
}

/// Non-Unix stub: only reachable via `am_root`, which is always false off Unix.
#[cfg(not(unix))]
fn resolve_nobody_gid() -> io::Result<u32> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "default 'nobody' group drop is Unix-only",
    ))
}

/// Resolves every group of the target uid for a `gid = *` directive.
///
/// Falls back to the current effective uid when no target uid is known (the
/// non-root, no-explicit-uid case), matching upstream's use of the resolved
/// `uid` variable in `want_all_groups`.
///
/// upstream: clientserver.c:797 `want_all_groups(f_out, uid)` ->
/// uidlist.c:576 `getallgroups`.
#[cfg(unix)]
fn resolve_all_user_groups(uid: Option<u32>) -> io::Result<Vec<u32>> {
    let target = uid.unwrap_or_else(platform::privilege::effective_uid);
    metadata::id_lookup::supplementary_gids_for_uid(target)
}

/// Non-Unix stub: no NSS group list is available, so only the explicit extras
/// (resolved by the caller) apply.
#[cfg(not(unix))]
fn resolve_all_user_groups(_uid: Option<u32>) -> io::Result<Vec<u32>> {
    Ok(Vec::new())
}

/// Applies chroot and privilege restrictions for a daemon module.
///
/// Called from the module access flow after authentication succeeds but before
/// the transfer begins. The order (chroot then privilege drop) matches
/// upstream and is required for security: the chroot needs root privileges,
/// so the uid/gid drop must come after it.
///
/// Errors are propagated unchanged so the caller can send an `@ERROR:` reply
/// to the client and close the connection - the daemon never silently
/// continues with reduced or escalated privileges.
///
/// upstream: `clientserver.c:rsync_module()` lines 978-1044 - chroot, then
/// `setgid`/`setgroups`, then `setuid`. Default uid/gid is `nobody:nobody`
/// when running as root and the module config does not override it
/// (`clientserver.c:779,818`); oc-rsync only drops when the config sets an
/// explicit numeric `uid`/`gid` and otherwise relies on the daemon-level
/// `uid`/`gid` directives applied before the accept loop.
#[cfg(test)]
fn apply_module_privilege_restrictions(
    module: &ModuleDefinition,
    log_sink: &SharedLogSink,
) -> io::Result<()> {
    if module.use_chroot {
        chroot_or_fallback(module, log_sink)?;
    }

    // Test scaffold only: drop for explicitly-configured uid/gid. The
    // root-defaults-to-nobody policy (`am_root`) is exercised by the live
    // `apply_privilege_restrictions_with_upstream_errors` path and by the
    // `resolve_drop_target` unit tests; performing a real setuid here would
    // irreversibly mutate the shared test process when the suite runs as root.
    if module.uid.is_some() || module.gid.is_some() {
        let target = resolve_drop_target(module, false)?;
        drop_privileges(target.uid, &target.gids, log_sink)?;
    }

    Ok(())
}

/// Creates a fallback [`SharedLogSink`] for privilege operations when no log file
/// is configured.
fn open_privilege_fallback_sink() -> SharedLogSink {
    let devnull = if cfg!(windows) { "NUL" } else { "/dev/null" };
    let file = OpenOptions::new()
        .write(true)
        .open(devnull)
        .unwrap_or_else(|_| {
            tempfile::tempfile().expect("open temporary file for privilege log sink")
        });
    Arc::new(Mutex::new(MessageSink::with_brand(file, Brand::Oc)))
}

/// Creates a [`SharedLogSink`] backed by a temporary file for testing.
#[cfg(test)]
fn test_log_sink() -> SharedLogSink {
    let file = tempfile::tempfile().expect("create temp file for test log sink");
    Arc::new(Mutex::new(MessageSink::with_brand(file, Brand::Oc)))
}

#[cfg(test)]
mod privilege_tests {
    use super::*;

    #[test]
    fn apply_module_privilege_restrictions_noop_when_disabled() {
        let module = ModuleDefinition {
            use_chroot: false,
            uid: None,
            gid: None,
            ..Default::default()
        };
        let sink = test_log_sink();
        let result = apply_module_privilege_restrictions(&module, &sink);
        assert!(result.is_ok());
    }

    #[test]
    fn apply_module_privilege_restrictions_noop_when_no_uid_gid_no_chroot() {
        let module = ModuleDefinition {
            name: "test".to_owned(),
            path: PathBuf::from("/tmp/test"),
            use_chroot: false,
            uid: None,
            gid: None,
            ..Default::default()
        };
        let sink = test_log_sink();
        let result = apply_module_privilege_restrictions(&module, &sink);
        assert!(result.is_ok());
    }

    /// Chroot failure must propagate so the daemon refuses to serve rather
    /// than continuing as root. Mirrors upstream `clientserver.c:978-982`
    /// where `@ERROR: chroot failed` is sent and the connection returns -1.
    #[cfg(unix)]
    #[test]
    fn apply_chroot_returns_err_for_nonexistent_path() {
        let sink = test_log_sink();
        let result = apply_chroot(
            std::path::Path::new("/nonexistent_oc_rsync_chroot_test_xyz_12345"),
            &sink,
        );
        assert!(result.is_err(), "chroot to missing path must fail");
    }

    /// WHY: upstream clientserver.c:781,820 - a root daemon whose module sets
    /// no `uid`/`gid` MUST drop to `nobody:nobody`, not keep serving as root.
    /// A regression here re-exposes the HIGH-severity default-root-worker gap.
    /// Asserts the resolver picks the `nobody` account, not that we perform a
    /// real setuid (which would corrupt the shared test process).
    #[cfg(unix)]
    #[test]
    fn resolve_drop_target_defaults_root_daemon_to_nobody() {
        let (Ok(Some(nobody_uid)), Ok(Some(nobody_gid))) = (
            metadata::id_lookup::lookup_user_by_name(b"nobody"),
            metadata::id_lookup::lookup_group_by_name(b"nobody"),
        ) else {
            // System without a `nobody` account/group: nothing to assert.
            return;
        };

        let module = ModuleDefinition {
            uid: None,
            gid: None,
            ..Default::default()
        };
        let target = resolve_drop_target(&module, true).expect("resolve nobody target");
        assert_eq!(
            target.uid,
            Some(nobody_uid),
            "root daemon must default uid to nobody"
        );
        assert_eq!(
            target.gids,
            vec![nobody_gid],
            "root daemon must default group set to [nobody]"
        );
    }

    /// WHY: upstream clientserver.c:778-779 recomputes `am_root` from the live
    /// uid; a non-root daemon with an unconfigured module leaves the identity
    /// untouched (`set_uid = 0`, empty `gid_list`). Guarantees we never attempt
    /// a spurious drop that would EPERM and break unprivileged daemons.
    #[test]
    fn resolve_drop_target_non_root_no_config_drops_nothing() {
        let module = ModuleDefinition {
            uid: None,
            gid: None,
            ..Default::default()
        };
        let target = resolve_drop_target(&module, false).expect("resolve empty target");
        assert_eq!(target.uid, None);
        assert!(target.gids.is_empty());
    }

    /// WHY: upstream clientserver.c:1022,1029 - `setgid(gid_array[0])` then
    /// `setgroups(gid_list)` install EXACTLY the configured list, clearing every
    /// inherited supplementary group. The resolver must hand `drop_privileges`
    /// the full list verbatim so the `setgroups` call replaces the group set.
    #[test]
    fn resolve_drop_target_gid_list_is_installed_verbatim() {
        let module = ModuleDefinition {
            uid: None,
            gid: Some(GidSetting::List(vec![4321, 27, 44])),
            ..Default::default()
        };
        let target = resolve_drop_target(&module, false).expect("resolve gid list");
        assert_eq!(
            target.gids,
            vec![4321, 27, 44],
            "the whole gid list must reach setgroups so inherited groups are cleared"
        );
        assert_eq!(target.uid, None);
    }

    /// WHY: upstream clientserver.c:781 - an explicit module `uid` overrides the
    /// nobody default even on a root daemon. Preserves existing deployments.
    #[cfg(unix)]
    #[test]
    fn resolve_drop_target_explicit_uid_overrides_nobody_default() {
        let module = ModuleDefinition {
            uid: Some(1234),
            gid: Some(GidSetting::List(vec![5678])),
            ..Default::default()
        };
        let target = resolve_drop_target(&module, true).expect("resolve explicit target");
        assert_eq!(target.uid, Some(1234));
        assert_eq!(target.gids, vec![5678]);
    }

    /// WHY: upstream clientserver.c:831-838 - when `use chroot` is UNSET and the
    /// runtime `chroot()` probe fails (rootless daemon), the daemon switches to
    /// no-chroot instead of aborting. Non-root callers cannot chroot, so this
    /// reproduces the rootless case and asserts the connection is not refused.
    #[cfg(unix)]
    #[test]
    fn chroot_or_fallback_auto_disables_when_unset_and_probe_fails() {
        if platform::privilege::is_effective_root() {
            // A root tester could actually chroot; the fallback is untestable.
            return;
        }
        let module = ModuleDefinition {
            path: PathBuf::from("/nonexistent_oc_rsync_rootless_xyz_98765"),
            use_chroot: true,
            use_chroot_explicit: false,
            ..Default::default()
        };
        let sink = test_log_sink();
        let (applied, _inner) = chroot_or_fallback(&module, &sink)
            .expect("unset use chroot must fall back, not error");
        assert!(!applied, "rootless fallback must report chroot NOT applied");
    }

    /// WHY: upstream clientserver.c:656-658 / 783-786 - a uid/gid NAME that
    /// fails to resolve is a DISTINCT failure point from the setgid/setuid
    /// SYSCALL failures (clientserver.c:1024 / 1053). The resolution failure
    /// must reply `@ERROR: invalid uid/gid <name>` (FLOG `Invalid uid/gid
    /// <name>`), never `@ERROR: setuid failed`. Collapsing the two hides which
    /// stage failed from the operator and diverges from upstream's wire output.
    /// Pins both strings so they can never be merged again.
    #[test]
    fn drop_resolution_error_maps_to_upstream_invalid_strings() {
        let (flog, payload) = DropResolutionError::InvalidUid("nobody".to_owned()).upstream_reply();
        assert_eq!(flog, "Invalid uid nobody");
        assert_eq!(payload, "@ERROR: invalid uid nobody");
        assert_ne!(
            payload, SETUID_FAILED_PAYLOAD,
            "resolution failure must not reuse the setuid syscall string"
        );

        let (flog, payload) = DropResolutionError::InvalidGid("nobody".to_owned()).upstream_reply();
        assert_eq!(flog, "Invalid gid nobody");
        assert_eq!(payload, "@ERROR: invalid gid nobody");
        assert_ne!(
            payload, SETGID_FAILED_PAYLOAD,
            "resolution failure must not reuse the setgid syscall string"
        );
    }

    /// WHY: upstream clientserver.c:781-786 - when a root daemon's `nobody`
    /// default user does not resolve, `user_to_uid()` fails and the daemon
    /// replies `@ERROR: invalid uid nobody`. This drives the real
    /// `resolve_drop_target` path (not just the mapping helper) on hosts that
    /// lack a `nobody` account; hosts that have one skip, since the resolution
    /// then legitimately succeeds.
    #[cfg(unix)]
    #[test]
    fn resolve_drop_target_missing_nobody_maps_to_invalid_uid() {
        if metadata::id_lookup::lookup_user_by_name(b"nobody")
            .ok()
            .flatten()
            .is_some()
        {
            return;
        }
        let module = ModuleDefinition {
            uid: None,
            gid: None,
            ..Default::default()
        };
        let err = resolve_drop_target(&module, true).expect_err("missing nobody must fail");
        let (flog, payload) = err.upstream_reply();
        assert_eq!(payload, "@ERROR: invalid uid nobody");
        assert_eq!(flog, "Invalid uid nobody");
        assert_ne!(payload, SETUID_FAILED_PAYLOAD);
    }

    /// WHY: upstream clientserver.c:831 - an EXPLICIT `use chroot = yes` has no
    /// unset-fallback escape; a chroot failure must abort so the operator's
    /// isolation guarantee is never silently dropped.
    #[cfg(unix)]
    #[test]
    fn chroot_or_fallback_is_fatal_when_explicitly_requested() {
        if platform::privilege::is_effective_root() {
            return;
        }
        let module = ModuleDefinition {
            path: PathBuf::from("/nonexistent_oc_rsync_explicit_xyz_98765"),
            use_chroot: true,
            use_chroot_explicit: true,
            ..Default::default()
        };
        let sink = test_log_sink();
        assert!(
            chroot_or_fallback(&module, &sink).is_err(),
            "explicit use chroot must not silently fall back"
        );
    }

    /// WHY: upstream clientserver.c:847-864 - a module path without `/./`
    /// chroots into the whole path and starts the session at the new root.
    /// Pure path-string logic, no syscall involved.
    #[test]
    fn split_chroot_path_without_marker_chroots_whole_path() {
        let (outer, inner) = split_chroot_path(Path::new("/var/data/module"));
        assert_eq!(outer, PathBuf::from("/var/data/module"));
        assert_eq!(inner, PathBuf::from("/"));
    }

    /// WHY: upstream clientserver.c:848-857 - a `/./` marker splits the path
    /// into the outer chroot root and the inner post-chroot working
    /// directory, e.g. `path = /var/data/./module` chroots to `/var/data`
    /// then starts the session at `/module` (still reachable: siblings of
    /// `module` under `/var/data` remain inside the jail).
    #[test]
    fn split_chroot_path_splits_at_first_slash_dot_marker() {
        let (outer, inner) = split_chroot_path(Path::new("/var/data/./module"));
        assert_eq!(outer, PathBuf::from("/var/data"));
        assert_eq!(inner, PathBuf::from("/module"));
    }

    /// WHY: a marker at the very end of the path (empty inner segment)
    /// normalizes to the root, matching upstream's `module_dirlen` resetting
    /// to 0 for a trailing `/./`.
    #[test]
    fn split_chroot_path_trailing_marker_normalizes_inner_to_root() {
        let (outer, inner) = split_chroot_path(Path::new("/var/data/./"));
        assert_eq!(outer, PathBuf::from("/var/data"));
        assert_eq!(inner, PathBuf::from("/"));
    }

    /// WHY: only the FIRST `/./` marker is a split point; a nested module
    /// tree that happens to contain a second `/./` further down stays part
    /// of the inner half, matching `strstr`'s first-match semantics.
    #[test]
    fn split_chroot_path_only_splits_at_first_marker() {
        let (outer, inner) = split_chroot_path(Path::new("/a/./b/./c"));
        assert_eq!(outer, PathBuf::from("/a"));
        assert_eq!(inner, PathBuf::from("/b/./c"));
    }

    /// WHY: upstream clientserver.c:832-833 - `strstr(module_dir, "/./") !=
    /// NULL` short-circuits the tri-state straight to enabled, with no
    /// `chroot("/")` probe at all. This must hold even on an unprivileged
    /// runner (no root guard needed), because the marker check happens
    /// before the probe would ever run.
    #[test]
    fn resolve_use_chroot_slash_dot_marker_skips_probe() {
        let module = ModuleDefinition {
            path: PathBuf::from("/some/outer/./inner"),
            use_chroot: true,
            use_chroot_explicit: false,
            ..Default::default()
        };
        let sink = test_log_sink();
        assert!(
            resolve_use_chroot(&module, &sink),
            "a /./ marker must resolve to chroot-enabled without probing"
        );
    }

    /// WHY: an explicit `use chroot = no` always wins, even when the path
    /// carries a `/./` marker - upstream only consults the marker when the
    /// tri-state is unset (`use_chroot < 0`).
    #[test]
    fn resolve_use_chroot_explicit_false_overrides_slash_dot_marker() {
        let module = ModuleDefinition {
            path: PathBuf::from("/some/outer/./inner"),
            use_chroot: false,
            use_chroot_explicit: true,
            ..Default::default()
        };
        let sink = test_log_sink();
        assert!(
            !resolve_use_chroot(&module, &sink),
            "explicit 'use chroot = no' must not be overridden by a /./ marker"
        );
    }
}
