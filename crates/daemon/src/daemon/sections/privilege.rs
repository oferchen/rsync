/// Converts a filesystem path to a null-terminated C string for libc calls.
///
/// Returns an error if the path contains interior null bytes, which would
/// silently truncate the path and cause incorrect chroot targets.
#[cfg(unix)]
fn to_c_path(path: &Path) -> io::Result<std::ffi::CString> {
    use std::os::unix::ffi::OsStrExt;
    std::ffi::CString::new(path.as_os_str().as_bytes()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "path contains interior null byte: '{}'",
                path.display()
            ),
        )
    })
}

/// Applies a chroot jail to the given module path.
///
/// After this call the process root directory changes to `module_path` and the
/// working directory is set to `/`. All subsequent path operations resolve
/// relative to the new root. This mirrors upstream rsync's `chdir(lp_path())`
/// followed by `chroot(".")` sequence in `clientserver.c:rsync_module()`.
///
/// # Errors
///
/// Returns an error if the path contains a null byte, or if the `chroot(2)` or
/// `chdir(2)` system calls fail (e.g., insufficient privileges, nonexistent path).
#[cfg(unix)]
#[allow(unsafe_code)]
fn apply_chroot(module_path: &Path, log_sink: &SharedLogSink) -> io::Result<()> {
    let c_path = to_c_path(module_path)?;

    // upstream: clientserver.c -- chroot(lp_path(i)) after sanitising the module path.
    // SAFETY: `c_path` is a valid null-terminated string pointing to an existing
    // directory. The `chroot` call changes the process root; no memory safety
    // invariant is violated.
    let ret = unsafe { libc::chroot(c_path.as_ptr()) };
    if ret != 0 {
        let err = io::Error::last_os_error();
        let text = format!(
            "chroot to '{}' failed: {}",
            module_path.display(),
            err
        );
        let message = rsync_error!(1, text).with_role(Role::Daemon);
        log_message(log_sink, &message);
        return Err(err);
    }

    env::set_current_dir("/")?;

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
/// The call sequence follows the security-critical ordering mandated by POSIX:
/// 1. `setgroups()` -- clear supplementary groups (must happen while still root)
/// 2. `setgid()` -- drop group privileges
/// 3. `setuid()` -- drop user privileges (irreversible, must be last)
///
/// This matches upstream rsync's privilege-drop sequence in
/// `clientserver.c:rsync_module()` where `setgid`/`setuid` are called after
/// chroot.
///
/// # Errors
///
/// Returns an error if any of the underlying system calls fail. Common causes
/// include insufficient privileges (not running as root) or invalid uid/gid values.
#[cfg(unix)]
#[allow(unsafe_code)]
fn drop_privileges(
    uid: Option<u32>,
    gid: Option<u32>,
    log_sink: &SharedLogSink,
) -> io::Result<()> {
    if let Some(gid_val) = gid {
        let gid_t = libc::gid_t::from(gid_val);

        // SAFETY: `setgroups` with a single-element array is a standard POSIX call.
        // The array lives on the stack for the duration of the call.
        let ret = unsafe { libc::setgroups(1, [gid_t].as_ptr()) };
        if ret != 0 {
            let err = io::Error::last_os_error();
            let text = format!("setgroups([{gid_val}]) failed: {err}");
            let message = rsync_error!(1, text).with_role(Role::Daemon);
            log_message(log_sink, &message);
            return Err(err);
        }

        // SAFETY: `setgid` is a standard POSIX call. The gid_t value is validated
        // by the kernel; invalid values return EINVAL without memory corruption.
        let ret = unsafe { libc::setgid(gid_t) };
        if ret != 0 {
            let err = io::Error::last_os_error();
            let text = format!("setgid({gid_val}) failed: {err}");
            let message = rsync_error!(1, text).with_role(Role::Daemon);
            log_message(log_sink, &message);
            return Err(err);
        }

        let text = format!("dropped group privileges to gid {gid_val}");
        let message = rsync_info!(text).with_role(Role::Daemon);
        log_message(log_sink, &message);
    }

    if let Some(uid_val) = uid {
        // SAFETY: `setuid` is a standard POSIX call. Once successful the process
        // cannot regain root privileges. The uid_t value is validated by the kernel.
        let ret = unsafe { libc::setuid(libc::uid_t::from(uid_val)) };
        if ret != 0 {
            let err = io::Error::last_os_error();
            let text = format!("setuid({uid_val}) failed: {err}");
            let message = rsync_error!(1, text).with_role(Role::Daemon);
            log_message(log_sink, &message);
            return Err(err);
        }

        let text = format!("dropped user privileges to uid {uid_val}");
        let message = rsync_info!(text).with_role(Role::Daemon);
        log_message(log_sink, &message);
    }

    Ok(())
}

/// No-op privilege-drop stub for non-Unix platforms.
#[cfg(not(unix))]
fn drop_privileges(
    _uid: Option<u32>,
    _gid: Option<u32>,
    _log_sink: &SharedLogSink,
) -> io::Result<()> {
    Ok(())
}

/// Applies chroot and privilege restrictions for a daemon module.
///
/// This is the main entry point called from the module access flow after
/// authentication succeeds but before the transfer begins. It:
/// 1. Calls `chroot(2)` into the module path when `use_chroot` is enabled
/// 2. Drops supplementary groups, then group, then user privileges
///
/// After chroot the effective module path becomes `"/"` since the process root
/// is now the module directory. Callers must adjust the server config path
/// accordingly.
///
/// When both `use_chroot` and uid/gid are disabled this function is a no-op.
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
///
/// Privilege operations require a log sink for error reporting. When the daemon
/// runs without a log file, this function creates a sink backed by `/dev/null`
/// (Unix) or `NUL` (Windows) so log messages are silently discarded.
fn open_privilege_fallback_sink() -> SharedLogSink {
    let devnull = if cfg!(windows) { "NUL" } else { "/dev/null" };
    let file = OpenOptions::new()
        .write(true)
        .open(devnull)
        .unwrap_or_else(|_| {
            // Last resort: use a temporary file if /dev/null is unavailable
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

    #[cfg(unix)]
    #[test]
    fn to_c_path_valid_path() {
        let path = Path::new("/srv/rsync/data");
        let result = to_c_path(path);
        assert!(result.is_ok());
        let c_str = result.unwrap();
        assert_eq!(c_str.as_bytes(), b"/srv/rsync/data");
    }

    #[cfg(unix)]
    #[test]
    fn to_c_path_rejects_interior_null() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;
        let os_str = OsStr::from_bytes(b"/srv/rsync\0/data");
        let path = Path::new(os_str);
        let result = to_c_path(path);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert!(err.to_string().contains("interior null byte"));
    }

    #[cfg(unix)]
    #[test]
    fn to_c_path_root_path() {
        let path = Path::new("/");
        let result = to_c_path(path);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().as_bytes(), b"/");
    }

    #[cfg(unix)]
    #[test]
    fn to_c_path_empty_path() {
        let path = Path::new("");
        let result = to_c_path(path);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().as_bytes(), b"");
    }

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
