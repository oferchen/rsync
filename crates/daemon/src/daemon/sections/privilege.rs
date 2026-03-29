/// Applies a chroot jail to the given module path.
///
/// Delegates to `platform::privilege::apply_chroot()`.
///
/// upstream: clientserver.c - chroot(lp_path(i)) after sanitising the module path.
#[cfg(unix)]
fn apply_chroot(module_path: &Path, log_sink: &SharedLogSink) -> io::Result<()> {
    if let Err(err) = platform::privilege::apply_chroot(module_path) {
        let text = format!("chroot to '{}' failed: {}", module_path.display(), err);
        let message = rsync_error!(1, text).with_role(Role::Daemon);
        log_message(log_sink, &message);
        return Err(err);
    }

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
/// Delegates to `platform::privilege::drop_privileges()`.
///
/// upstream: clientserver.c:rsync_module() - setgid/setuid after chroot.
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
/// This is the main entry point called from the module access flow after
/// authentication succeeds but before the transfer begins.
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
}
