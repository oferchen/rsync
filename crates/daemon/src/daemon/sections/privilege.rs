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
/// sequence is `setgroups` -> `setgid` -> `setuid`, matching the POSIX ordering
/// required for setuid to be irreversible. Any failure is propagated; the
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
    gid: Option<u32>,
    log_sink: &SharedLogSink,
) -> io::Result<()> {
    if let Err(err) = platform::privilege::drop_privileges(uid, gid) {
        let text = format!("drop_privileges(uid={uid:?}, gid={gid:?}) failed: {err}");
        let message = rsync_error!(1, text).with_role(Role::Daemon);
        log_message(log_sink, &message);
        return Err(err);
    }

    if let Some(gid_val) = gid {
        let text = format!("dropped group privileges to gid {gid_val}");
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
        apply_chroot(&module.path, log_sink)?;
    }

    if module.uid.is_some() || module.gid.is_some() {
        drop_privileges(module.uid, module.gid, log_sink)?;
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
}
