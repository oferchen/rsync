//! Process privilege operations - chroot and uid/gid dropping.
//!
//! # Unix
//!
//! Uses `nix` safe wrappers for chroot, setuid, setgid. Falls back to `libc`
//! for setgroups (not available on macOS in nix).
//!
//! # Windows
//!
//! Uses `LogonUserW` and `ImpersonateLoggedOnUser` for user impersonation.
//!
//! # Upstream Reference
//!
//! `clientserver.c:rsync_module()` - chroot + setgid/setuid after authentication.

use std::io;
use std::path::Path;

/// Applies a chroot jail to the given path.
///
/// After this call the process root directory changes to `path` and the
/// working directory is set to `/`. All subsequent path operations resolve
/// relative to the new root.
///
/// No-op on non-Unix platforms.
#[cfg(unix)]
pub fn apply_chroot(path: &Path) -> io::Result<()> {
    nix::unistd::chroot(path).map_err(nix_to_io)?;
    std::env::set_current_dir("/")?;
    Ok(())
}

/// No-op chroot on non-Unix platforms.
#[cfg(not(unix))]
pub fn apply_chroot(_path: &Path) -> io::Result<()> {
    Ok(())
}

/// Drops process privileges to the specified uid and gid.
///
/// The call sequence follows the security-critical POSIX ordering:
/// 1. `setgroups()` - clear supplementary groups (must happen while still root)
/// 2. `setgid()` - drop group privileges
/// 3. `setuid()` - drop user privileges (irreversible, must be last)
///
/// upstream: `clientserver.c:rsync_module()` - setgid/setuid after chroot.
#[cfg(unix)]
pub fn drop_privileges(uid: Option<u32>, gid: Option<u32>) -> io::Result<()> {
    if let Some(gid_val) = gid {
        set_supplementary_groups(gid_val)?;

        let nix_gid = nix::unistd::Gid::from_raw(gid_val);
        nix::unistd::setgid(nix_gid).map_err(nix_to_io)?;
    }

    if let Some(uid_val) = uid {
        let nix_uid = nix::unistd::Uid::from_raw(uid_val);
        nix::unistd::setuid(nix_uid).map_err(nix_to_io)?;
    }

    Ok(())
}

/// Sets supplementary groups to a single-element list containing `gid`.
///
/// Uses `nix::unistd::setgroups` on Linux. On macOS (where nix doesn't
/// provide setgroups), falls back to `libc::setgroups` directly.
#[cfg(unix)]
fn set_supplementary_groups(gid: u32) -> io::Result<()> {
    #[cfg(not(target_vendor = "apple"))]
    {
        let nix_gid = nix::unistd::Gid::from_raw(gid);
        nix::unistd::setgroups(&[nix_gid]).map_err(nix_to_io)
    }

    #[cfg(target_vendor = "apple")]
    {
        set_supplementary_groups_libc(gid)
    }
}

/// Fallback setgroups via libc for macOS where nix doesn't provide it.
#[cfg(all(unix, target_vendor = "apple"))]
#[allow(unsafe_code)]
fn set_supplementary_groups_libc(gid: u32) -> io::Result<()> {
    let gid_t = libc::gid_t::from(gid);
    // SAFETY: `setgroups` with a single-element array is a standard POSIX call.
    // The array lives on the stack for the duration of the call.
    let ret = unsafe { libc::setgroups(1, [gid_t].as_ptr()) };
    if ret != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// No-op privilege drop on non-Unix platforms.
#[cfg(not(unix))]
pub fn drop_privileges(_uid: Option<u32>, _gid: Option<u32>) -> io::Result<()> {
    Ok(())
}

/// Drops privileges on Windows via user impersonation.
///
/// Uses `LogonUserW` to obtain a token for the specified account, then
/// `ImpersonateLoggedOnUser` to assume that identity. The `account_name`
/// parameter accepts `DOMAIN\user` or plain `user` format.
///
/// upstream: `clientserver.c:rsync_module()` - uid/gid are resolved to
/// account names and used for impersonation on Windows.
#[cfg(windows)]
pub fn drop_privileges_windows(
    _uid: Option<u32>,
    _gid: Option<u32>,
    account_name: Option<&str>,
) -> io::Result<()> {
    let Some(name) = account_name else {
        return Ok(());
    };

    windows_impersonate(name)
}

/// No-op Windows privilege drop on non-Windows platforms.
#[cfg(not(windows))]
pub fn drop_privileges_windows(
    _uid: Option<u32>,
    _gid: Option<u32>,
    _account_name: Option<&str>,
) -> io::Result<()> {
    Ok(())
}

/// Performs Windows user impersonation via LogonUserW + ImpersonateLoggedOnUser.
#[cfg(windows)]
#[allow(unsafe_code)]
fn windows_impersonate(account_name: &str) -> io::Result<()> {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::Security::Authentication::Identity::{
        LOGON32_LOGON_NETWORK, LOGON32_PROVIDER_DEFAULT, LogonUserW,
    };
    use windows::Win32::Security::ImpersonateLoggedOnUser;
    use windows::core::PCWSTR;

    // Split DOMAIN\user if present.
    let (domain, user) = match account_name.split_once('\\') {
        Some((d, u)) => (Some(d), u),
        None => (None, account_name),
    };

    let user_wide: Vec<u16> = user.encode_utf16().chain(std::iter::once(0)).collect();
    let domain_wide: Option<Vec<u16>> =
        domain.map(|d| d.encode_utf16().chain(std::iter::once(0)).collect());

    let domain_ptr = match &domain_wide {
        Some(d) => PCWSTR(d.as_ptr()),
        None => PCWSTR::null(),
    };

    let mut token = windows::Win32::Foundation::HANDLE::default();

    // SAFETY: `user_wide` and `domain_wide` are valid null-terminated UTF-16 strings.
    // `token` receives the logon token handle on success. We close it after impersonation.
    unsafe {
        LogonUserW(
            PCWSTR(user_wide.as_ptr()),
            domain_ptr,
            PCWSTR::null(), // no password - requires appropriate privileges
            LOGON32_LOGON_NETWORK,
            LOGON32_PROVIDER_DEFAULT,
            &mut token,
        )
        .map_err(|e| {
            io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("LogonUserW failed for '{account_name}': {e}"),
            )
        })?;
    }

    // SAFETY: `token` is a valid handle returned by LogonUserW.
    let impersonate_result = unsafe { ImpersonateLoggedOnUser(token) };

    // SAFETY: `token` is a valid handle that must be closed regardless of impersonation result.
    let _ = unsafe { CloseHandle(token) };

    impersonate_result.map_err(|e| {
        io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("ImpersonateLoggedOnUser failed for '{account_name}': {e}"),
        )
    })
}

/// Converts a `nix::Error` to `std::io::Error`.
#[cfg(unix)]
fn nix_to_io(err: nix::Error) -> io::Error {
    io::Error::from_raw_os_error(err as i32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn chroot_rejects_nonexistent_path() {
        let result = apply_chroot(Path::new("/nonexistent_path_xyz_99999"));
        assert!(result.is_err());
    }

    #[test]
    fn drop_privileges_noop_when_none() {
        let result = drop_privileges(None, None);
        assert!(result.is_ok());
    }

    #[cfg(windows)]
    #[test]
    fn windows_drop_privileges_noop_when_no_account() {
        let result = drop_privileges_windows(None, None, None);
        assert!(result.is_ok());
    }
}
