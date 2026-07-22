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
/// Caches timezone data via `tzset()` before the chroot syscall so that
/// any subsequent `localtime`/`strftime` call inside the jail still resolves
/// the local offset. glibc reads `/etc/localtime` lazily on the first
/// conversion; after chroot the file is no longer reachable and timestamps
/// silently fall back to UTC.
///
/// No-op on non-Unix platforms.
///
/// upstream: clientserver.c:979-980 (3.4.2) - `tzset()` called immediately
/// before `chroot(module_chdir)`; same fix at clientserver.c:1306 before
/// the daemon-level `chroot(lp_daemon_chroot())`.
#[cfg(unix)]
#[allow(unsafe_code)]
pub fn apply_chroot(path: &Path) -> io::Result<()> {
    // Declared locally because the `libc` crate's `tzset` export varies across
    // versions and feature gates; `tzset()` is a POSIX-mandated C function with
    // a stable ABI present in every Unix libc.
    unsafe extern "C" {
        fn tzset();
    }
    // SAFETY: `tzset` is a thread-safe POSIX call with no parameters and no
    // pointer arguments. It reads `/etc/localtime` (or `$TZ`) and updates the
    // process-wide timezone state guarded by libc's internal lock.
    unsafe {
        tzset();
    }
    nix::unistd::chroot(path).map_err(nix_to_io)?;
    std::env::set_current_dir("/")?;
    Ok(())
}

/// No-op chroot on non-Unix platforms.
///
/// Windows does not support chroot. Logs a warning via stderr so daemon
/// operators know the `use chroot` directive has no effect.
///
/// upstream: clientserver.c - chroot is Unix-only; Windows daemon skips it.
#[cfg(not(unix))]
pub fn apply_chroot(_path: &Path) -> io::Result<()> {
    eprintln!("WARNING: chroot is not supported on this platform - skipping");
    Ok(())
}

/// Probes chroot capability with a no-op `chroot("/")`, without touching any
/// module path.
///
/// Used only when a module's `use chroot` directive is unset: the daemon
/// tries this harmless self-chroot to determine whether the process has
/// `CAP_SYS_CHROOT` before deciding the tri-state default. Success leaves
/// the process root unchanged (chrooting to `/` is a no-op); failure (almost
/// always `EPERM`) means the daemon is unprivileged.
///
/// upstream: clientserver.c:834 `rsync_module()` - `chroot("/") < 0` probes
/// capability before the real `chroot(module_chdir)` later in the function.
#[cfg(unix)]
pub fn probe_chroot_capability() -> io::Result<()> {
    nix::unistd::chroot("/").map_err(nix_to_io)?;
    std::env::set_current_dir("/")?;
    Ok(())
}

/// No-op chroot probe on non-Unix platforms: chroot never applies there, so
/// the tri-state always resolves to "enabled" (harmlessly unused).
#[cfg(not(unix))]
pub fn probe_chroot_capability() -> io::Result<()> {
    Ok(())
}

/// Drops process privileges to the specified uid and group list.
///
/// `gids` is the complete group set to install, primary group first (as
/// resolved by the daemon from the module's `gid` directive, or the
/// `nobody` default). An empty slice leaves the group identity untouched.
///
/// The call sequence follows upstream's security-critical ordering:
/// 1. `setgid(gids[0])` - drop the primary group (clientserver.c:1022)
/// 2. `setgroups(gids)` - install the group set, clearing every inherited
///    supplementary group (clientserver.c:1029)
/// 3. `setuid()` - drop user privileges (irreversible, must be last;
///    clientserver.c:1046)
///
/// upstream: `clientserver.c:rsync_module()` - setgid/setgroups/setuid after
/// chroot.
#[cfg(unix)]
pub fn drop_privileges(uid: Option<u32>, gids: &[u32]) -> io::Result<()> {
    if let Some(&primary) = gids.first() {
        let nix_gid = nix::unistd::Gid::from_raw(primary);
        nix::unistd::setgid(nix_gid).map_err(nix_to_io)?;

        set_supplementary_groups(gids)?;
    }

    if let Some(uid_val) = uid {
        let nix_uid = nix::unistd::Uid::from_raw(uid_val);
        nix::unistd::setuid(nix_uid).map_err(nix_to_io)?;
    }

    Ok(())
}

/// Installs the given group list as the process's active groups, replacing
/// (and thereby clearing) any inherited supplementary groups.
///
/// Uses `nix::unistd::setgroups` on Linux. On macOS (where nix doesn't
/// provide setgroups), falls back to `libc::setgroups` directly.
#[cfg(unix)]
fn set_supplementary_groups(gids: &[u32]) -> io::Result<()> {
    #[cfg(not(target_vendor = "apple"))]
    {
        let nix_gids: Vec<nix::unistd::Gid> = gids
            .iter()
            .copied()
            .map(nix::unistd::Gid::from_raw)
            .collect();
        nix::unistd::setgroups(&nix_gids).map_err(nix_to_io)
    }

    #[cfg(target_vendor = "apple")]
    {
        set_supplementary_groups_libc(gids)
    }
}

/// Fallback setgroups via libc for macOS where nix doesn't provide it.
#[cfg(all(unix, target_vendor = "apple"))]
#[allow(unsafe_code)]
fn set_supplementary_groups_libc(gids: &[u32]) -> io::Result<()> {
    let gid_array: Vec<libc::gid_t> = gids.iter().map(|&gid| gid as libc::gid_t).collect();
    // SAFETY: `setgroups` reads `gid_array.len()` entries from the array, which
    // lives on the heap for the duration of the call.
    let ret = unsafe { libc::setgroups(gid_array.len() as libc::c_int, gid_array.as_ptr()) };
    if ret != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// No-op privilege drop on non-Unix platforms.
#[cfg(not(unix))]
pub fn drop_privileges(_uid: Option<u32>, _gids: &[u32]) -> io::Result<()> {
    Ok(())
}

/// Returns whether the process has an effective uid of 0 (root).
///
/// Non-Unix platforms have no root uid and always return `false`.
///
/// upstream: clientserver.c:780 `am_root = (uid == ROOT_UID)`.
#[cfg(unix)]
pub fn is_effective_root() -> bool {
    nix::unistd::geteuid().is_root()
}

/// Non-Unix stub: there is no root uid.
#[cfg(not(unix))]
pub fn is_effective_root() -> bool {
    false
}

/// Returns the process's current effective uid.
///
/// Non-Unix platforms have no POSIX uid and return `0`.
#[cfg(unix)]
pub fn effective_uid() -> u32 {
    nix::unistd::geteuid().as_raw()
}

/// Non-Unix stub: there is no POSIX effective uid.
#[cfg(not(unix))]
pub fn effective_uid() -> u32 {
    0
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
    use windows::Win32::Security::{
        ImpersonateLoggedOnUser, LOGON32_LOGON_NETWORK, LOGON32_PROVIDER_DEFAULT, LogonUserW,
    };
    use windows::core::PCWSTR;

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
        let result = drop_privileges(None, &[]);
        assert!(result.is_ok());
    }

    #[test]
    fn drop_privileges_windows_noop_when_no_account() {
        let result = drop_privileges_windows(None, None, None);
        assert!(result.is_ok());
    }

    #[test]
    fn drop_privileges_windows_noop_with_uid_gid_but_no_account() {
        let result = drop_privileges_windows(Some(1000), Some(1000), None);
        assert!(result.is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn chroot_error_has_os_error_kind() {
        let err = apply_chroot(Path::new("/nonexistent_path_xyz_99999")).unwrap_err();
        // EPERM or ENOENT depending on whether we are root
        assert!(
            err.kind() == io::ErrorKind::PermissionDenied || err.kind() == io::ErrorKind::NotFound,
            "expected PermissionDenied or NotFound, got {:?}",
            err.kind()
        );
    }

    /// `apply_chroot` invokes `tzset()` before the chroot syscall to cache
    /// `/etc/localtime` while the file is still reachable. The call is
    /// idempotent and side-effect free, so even when the subsequent chroot
    /// fails (e.g., non-existent path, non-root caller) the function must
    /// still surface the original chroot error verbatim.
    ///
    /// upstream: clientserver.c:979-980 (3.4.2) - `tzset()` before chroot.
    #[cfg(unix)]
    #[test]
    fn apply_chroot_tzset_does_not_mask_chroot_failure() {
        let err = apply_chroot(Path::new("/nonexistent_oc_tzset_xyz_42")).unwrap_err();
        assert!(
            err.kind() == io::ErrorKind::PermissionDenied || err.kind() == io::ErrorKind::NotFound,
            "tzset must not alter the surfaced chroot error: got {:?}",
            err.kind()
        );
    }

    /// End-to-end smoke test for the `apply_chroot` -> log-timestamp path:
    /// drives the function with a fixed POSIX `TZ` offset and asserts that
    /// a post-call `localtime_r` resolves the expected local hour. This
    /// pins the contract upstream rsync 3.4.2 added: `tzset()` is invoked
    /// during `apply_chroot` so that timestamps emitted after the chroot
    /// syscall reflect the host timezone instead of UTC.
    ///
    /// Steps (no root required, chroot is allowed to fail):
    ///   1. Set `TZ=EST5` (UTC-5, no DST) under a process-wide mutex.
    ///   2. Call `apply_chroot` with a non-existent path. The inline
    ///      `tzset()` runs before the chroot syscall errors out.
    ///   3. Convert a fixed UTC epoch with `localtime_r` and assert the
    ///      local hour matches EST5.
    ///
    /// upstream: clientserver.c rsync_module / start_accept_loop with tzset
    /// before chroot.
    #[cfg(unix)]
    #[test]
    #[allow(unsafe_code)]
    fn apply_chroot_caches_local_timezone_offset() {
        use std::sync::{Mutex, OnceLock};

        unsafe extern "C" {
            fn tzset();
        }

        // `TZ` is process-wide global state; serialize against any other test
        // in this module that mutates it.
        static TZ_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        let _guard = TZ_LOCK.get_or_init(|| Mutex::new(())).lock().unwrap();

        // Save and restore `TZ` so we never leak the override to siblings.
        let original = std::env::var_os("TZ");
        // SAFETY: writes to `TZ` are protected by `TZ_LOCK` for the test
        // scope. POSIX `setenv` is the supported mechanism for changing the
        // libc-visible timezone.
        unsafe {
            std::env::set_var("TZ", "EST5");
        }

        // Trigger `apply_chroot` - the inner `tzset()` runs even though the
        // chroot syscall fails on a non-existent path.
        let chroot_err = apply_chroot(Path::new("/nonexistent_oc_tzset_cache_probe")).unwrap_err();
        assert!(
            chroot_err.kind() == io::ErrorKind::PermissionDenied
                || chroot_err.kind() == io::ErrorKind::NotFound,
            "unexpected chroot error: {:?}",
            chroot_err.kind()
        );

        // 2026-01-01T00:00:00Z - winter epoch, no DST ambiguity under EST5.
        // `i64` matches the 64-bit `time_t` used by glibc, musl 1.2+, and Apple
        // libc on the targets we build; avoids the deprecated `libc::time_t`
        // alias that triggers a hard error on musl under `-D deprecated`.
        let utc_epoch: i64 = 1_767_225_600;
        // SAFETY: `tm` is a plain-old-data layout; zero-init is valid.
        let mut local_tm: libc::tm = unsafe { std::mem::zeroed() };
        // SAFETY: `localtime_r` writes into a stack-allocated `tm` we own.
        // The returned pointer aliases that buffer; we only read `tm_hour`.
        let ret = unsafe { libc::localtime_r(&utc_epoch, &mut local_tm) };
        assert!(!ret.is_null(), "localtime_r returned null for EST5 epoch");

        // EST5 is UTC-5 with no DST: 00:00 UTC -> 19:00 previous day local.
        assert_eq!(
            local_tm.tm_hour, 19,
            "tzset cache miss: expected hour=19 under TZ=EST5, got {}",
            local_tm.tm_hour
        );

        // Restore `TZ` and re-prime libc so later tests in the same process
        // observe the original timezone.
        // SAFETY: still holding `_guard`; the mutation is exclusive.
        unsafe {
            match original {
                Some(value) => std::env::set_var("TZ", value),
                None => std::env::remove_var("TZ"),
            }
            tzset();
        }
    }

    /// `probe_chroot_capability` must never touch a module path - it always
    /// targets `"/"`. On a non-root test runner it fails with `EPERM`
    /// (lack of `CAP_SYS_CHROOT`); on a root runner it succeeds harmlessly
    /// (chrooting to `/` changes nothing). Either outcome is valid; the
    /// test only pins that the call never panics and returns an `io::Error`
    /// on failure rather than something unexpected.
    #[cfg(unix)]
    #[test]
    fn probe_chroot_capability_succeeds_or_reports_permission_denied() {
        match probe_chroot_capability() {
            Ok(()) => {}
            Err(err) => assert_eq!(
                err.kind(),
                io::ErrorKind::PermissionDenied,
                "unexpected probe failure kind: {:?}",
                err.kind()
            ),
        }
    }

    #[cfg(unix)]
    #[test]
    fn drop_privileges_fails_for_nonexistent_uid_when_root() {
        // Only meaningful when running as root - otherwise setuid fails with EPERM
        // which is the expected non-root behavior. This test verifies the error path.
        if !nix::unistd::getuid().is_root() {
            let result = drop_privileges(Some(99999), &[]);
            assert!(result.is_err(), "non-root should fail to setuid");
        }
    }
}
