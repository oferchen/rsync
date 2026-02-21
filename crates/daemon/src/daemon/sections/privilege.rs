/// Applies `chroot(2)` to the module path on Unix platforms.
///
/// After a successful chroot, the working directory is changed to `"/"` so
/// that all subsequent path operations are relative to the new root. This
/// mirrors upstream rsync's `clientserver.c` behaviour where `chroot(lp_path(i))`
/// is followed by `chdir("/")`.
///
/// On non-Unix platforms, a warning is logged and the function succeeds without
/// changing the filesystem root.
#[cfg(unix)]
#[allow(unsafe_code)]
fn apply_chroot(module_path: &Path, log_sink: Option<&SharedLogSink>) -> io::Result<()> {
    let c_path = to_c_path(module_path)?;
    // SAFETY: `c_path` is a valid nul-terminated CString whose lifetime spans this call.
    // upstream: clientserver.c — chroot(lp_path(i))
    let result = unsafe { libc::chroot(c_path.as_ptr()) };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }

    env::set_current_dir("/")?;

    if let Some(log) = log_sink {
        let text = format!("chroot {}", module_path.display());
        let message = rsync_info!(text).with_role(Role::Daemon);
        log_message(log, &message);
    }

    Ok(())
}

/// No-op chroot stub for non-Unix platforms.
///
/// Logs a warning that chroot is not supported and returns success.
#[cfg(not(unix))]
fn apply_chroot(module_path: &Path, log_sink: Option<&SharedLogSink>) -> io::Result<()> {
    if let Some(log) = log_sink {
        let text = format!(
            "chroot not supported on this platform; skipping chroot to {}",
            module_path.display()
        );
        let message = rsync_warning!(text).with_role(Role::Daemon);
        log_message(log, &message);
    }
    Ok(())
}

/// Drops daemon process privileges by switching to the configured uid/gid.
///
/// The call order follows POSIX requirements and upstream rsync's
/// `clientserver.c` behaviour:
/// 1. `setgroups([gid])` — drop supplementary groups (requires current uid 0)
/// 2. `setgid(gid)` — must happen *before* `setuid` because `setuid` may
///    irrevocably drop the ability to change the group
/// 3. `setuid(uid)` — irrevocable on most systems when dropping from root
///
/// When only `gid` is configured, only group-related calls are made. When only
/// `uid` is configured, only `setuid` is called.
///
/// On non-Unix platforms, a warning is logged and the function returns success.
#[cfg(unix)]
#[allow(unsafe_code)]
fn drop_privileges(
    uid: Option<u32>,
    gid: Option<u32>,
    log_sink: Option<&SharedLogSink>,
) -> io::Result<()> {
    if uid.is_none() && gid.is_none() {
        return Ok(());
    }

    // upstream: clientserver.c — setgid() before setuid()
    if let Some(gid_val) = gid {
        let gid_t = gid_val as libc::gid_t;
        // SAFETY: `gid_t` is a valid gid value and `&gid_t` points to a single-element
        // array with sufficient lifetime for the syscall.
        let result = unsafe { libc::setgroups(1, &gid_t) };
        if result != 0 {
            return Err(io::Error::last_os_error());
        }

        // SAFETY: `gid_t` is a valid gid value.
        let result = unsafe { libc::setgid(gid_t) };
        if result != 0 {
            return Err(io::Error::last_os_error());
        }

        if let Some(log) = log_sink {
            let text = format!("set gid {gid_val}");
            let message = rsync_info!(text).with_role(Role::Daemon);
            log_message(log, &message);
        }
    }

    if let Some(uid_val) = uid {
        // SAFETY: `uid_val` is a valid uid value cast to `uid_t`.
        let result = unsafe { libc::setuid(uid_val as libc::uid_t) };
        if result != 0 {
            return Err(io::Error::last_os_error());
        }

        if let Some(log) = log_sink {
            let text = format!("set uid {uid_val}");
            let message = rsync_info!(text).with_role(Role::Daemon);
            log_message(log, &message);
        }
    }

    Ok(())
}

/// No-op privilege-dropping stub for non-Unix platforms.
///
/// Logs a warning when privilege dropping was requested and returns success.
#[cfg(not(unix))]
fn drop_privileges(
    uid: Option<u32>,
    gid: Option<u32>,
    log_sink: Option<&SharedLogSink>,
) -> io::Result<()> {
    if uid.is_some() || gid.is_some() {
        if let Some(log) = log_sink {
            let text =
                "privilege dropping not supported on this platform; skipping setuid/setgid"
                    .to_owned();
            let message = rsync_warning!(text).with_role(Role::Daemon);
            log_message(log, &message);
        }
    }
    Ok(())
}

/// Applies chroot and privilege dropping for a daemon module.
///
/// Called after authentication succeeds but before the transfer begins. This is
/// the single entry point used by the module access flow to sandbox the daemon
/// worker process, mirroring upstream rsync's `rsync_module()` in
/// `clientserver.c`.
///
/// Chroot is applied first (if `use_chroot` is true), then privileges are
/// dropped (if `uid` or `gid` are configured). The ordering is critical because
/// chroot requires root privileges on most systems.
fn apply_module_privilege_restrictions(
    module: &ModuleDefinition,
    log_sink: Option<&SharedLogSink>,
) -> io::Result<()> {
    if module.use_chroot {
        apply_chroot(&module.path, log_sink)?;
    }

    drop_privileges(module.uid, module.gid, log_sink)?;

    Ok(())
}

/// Converts a [`Path`] to a nul-terminated C string suitable for syscall arguments.
#[cfg(unix)]
fn to_c_path(path: &Path) -> io::Result<std::ffi::CString> {
    use std::os::unix::ffi::OsStrExt;
    std::ffi::CString::new(path.as_os_str().as_bytes()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("path contains nul byte: {}", path.display()),
        )
    })
}

#[cfg(test)]
mod privilege_tests {
    use super::*;

    #[test]
    fn apply_module_privilege_restrictions_no_ops_when_disabled() {
        let module = ModuleDefinition {
            use_chroot: false,
            uid: None,
            gid: None,
            ..Default::default()
        };
        // Should succeed without attempting any syscalls
        let result = apply_module_privilege_restrictions(&module, None);
        assert!(result.is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn to_c_path_converts_valid_path() {
        let path = Path::new("/tmp/test");
        let c_path = to_c_path(path).unwrap();
        assert_eq!(c_path.as_bytes(), b"/tmp/test");
    }

    #[cfg(unix)]
    #[test]
    fn to_c_path_rejects_nul_in_path() {
        let path = Path::new("/tmp/te\0st");
        let result = to_c_path(path);
        assert!(result.is_err());
    }

    #[test]
    fn drop_privileges_no_ops_when_both_none() {
        let result = drop_privileges(None, None, None);
        assert!(result.is_ok());
    }

    #[cfg(not(unix))]
    #[test]
    fn apply_chroot_succeeds_on_non_unix() {
        let path = Path::new("/tmp/test");
        let result = apply_chroot(path, None);
        assert!(result.is_ok());
    }

    #[cfg(not(unix))]
    #[test]
    fn drop_privileges_succeeds_on_non_unix() {
        let result = drop_privileges(Some(1000), Some(1000), None);
        assert!(result.is_ok());
    }
}
