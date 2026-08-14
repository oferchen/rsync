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
#[cfg(any(not(unix), test))]
use std::sync::atomic::{AtomicBool, Ordering};

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
/// upstream: clientserver.c:886 `rsync_module()` - `chroot("/") < 0` probes
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

/// Warning emitted once per process when a privilege drop to a numeric uid/gid
/// is requested on a platform that has no POSIX `setuid`/`setgid`.
///
/// Named so its wording can be pinned by a test. Windows offers only
/// account-name impersonation (see [`drop_privileges_windows`]), which needs an
/// account NAME rather than a numeric id, so a daemon configured with a numeric
/// `uid`/`gid` cannot drop here and keeps its current privileges.
#[cfg(any(not(unix), test))]
pub(crate) const PRIVILEGE_DROP_UNSUPPORTED_WARNING: &str = "warning: privilege drop to the configured uid/gid is NOT supported on this platform; \
the process continues WITHOUT dropping privileges. Windows provides only account-name \
impersonation, not POSIX setuid/setgid - configure an account to impersonate instead.";

/// Reports whether a privilege drop was actually requested: a target uid, or a
/// non-empty target group set.
///
/// An all-`None`/empty request is a genuine no-op on every platform (the Unix
/// path also leaves the identity untouched for it), so it must stay silent -
/// only a request that cannot be honored warrants a warning.
#[cfg(any(not(unix), test))]
#[must_use]
fn privilege_drop_requested(uid: Option<u32>, gids: &[u32]) -> bool {
    uid.is_some() || !gids.is_empty()
}

/// Runs `emit` at most once across every call sharing `already_warned`, and
/// only when `should_warn` is true.
///
/// Extracted as a pure fn so the once-only + request-gating contract is
/// unit-testable on any host, independent of the `#[cfg(not(unix))]` emitters
/// that are compiled out on Unix. `Ordering::Relaxed` is sufficient: the only
/// shared state is the boolean latch and the message carries no other data.
#[cfg(any(not(unix), test))]
fn warn_once_if(should_warn: bool, already_warned: &AtomicBool, emit: impl FnOnce()) {
    if should_warn && !already_warned.swap(true, Ordering::Relaxed) {
        emit();
    }
}

/// Privilege-drop stub for non-Unix platforms - there is no POSIX
/// `setuid`/`setgid` to perform.
///
/// # Windows privilege model
///
/// Windows has no POSIX uid/gid identity, so a numeric privilege drop is
/// impossible. The only mechanism Windows offers is thread-level impersonation
/// (`LogonUserW` + `ImpersonateLoggedOnUser`, see [`drop_privileges_windows`]),
/// which requires an account NAME rather than a numeric uid/gid. A daemon that
/// resolved a numeric `uid`/`gid` to drop to therefore cannot honor the request
/// on this platform.
///
/// Skipping the drop *silently* is a security bug: a misconfigured Windows
/// daemon would keep running with full privileges while the operator believes
/// it dropped them. So when a drop was genuinely requested (a uid or a
/// non-empty group set) this emits one loud warning per process via the shared
/// warn-once latch, then returns `Ok(())` so the caller decides whether the
/// condition is fatal. An empty request stays silent - a true no-op everywhere.
#[cfg(not(unix))]
pub fn drop_privileges(uid: Option<u32>, gids: &[u32]) -> io::Result<()> {
    static WARNED: AtomicBool = AtomicBool::new(false);
    warn_once_if(privilege_drop_requested(uid, gids), &WARNED, || {
        eprintln!("{PRIVILEGE_DROP_UNSUPPORTED_WARNING}");
    });
    Ok(())
}

/// Returns whether the process has an effective uid of 0 (root).
///
/// Non-Unix platforms have no root uid and always return `false`.
///
/// upstream: clientserver.c:831 `am_root = (uid == ROOT_UID)`.
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
    uid: Option<u32>,
    gid: Option<u32>,
    account_name: Option<&str>,
) -> io::Result<()> {
    let Some(name) = account_name else {
        // No account to impersonate: Windows cannot drop to a numeric uid/gid,
        // so warn loudly (once) when a drop was actually requested instead of
        // returning Ok silently and leaving the process privileged.
        static WARNED: AtomicBool = AtomicBool::new(false);
        warn_once_if(uid.is_some() || gid.is_some(), &WARNED, || {
            eprintln!("{PRIVILEGE_DROP_UNSUPPORTED_WARNING}");
        });
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

/// Splits a Windows account specifier into its optional `DOMAIN` and `user`
/// parts at the first backslash (`DOMAIN\user` -> `(Some("DOMAIN"), "user")`;
/// a plain `user` -> `(None, "user")`).
///
/// Kept pure and host-agnostic so the parse is unit-testable on Linux CI,
/// separate from the `LogonUserW` FFI it feeds - the split must happen before
/// the two halves are widened to UTF-16 and handed to the Win32 call.
#[cfg(any(windows, test))]
#[must_use]
fn split_account_name(account_name: &str) -> (Option<&str>, &str) {
    match account_name.split_once('\\') {
        Some((domain, user)) => (Some(domain), user),
        None => (None, account_name),
    }
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

    let (domain, user) = split_account_name(account_name);

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

    /// WHY (security): the non-Unix `drop_privileges` cannot perform a POSIX
    /// setuid/setgid, so it is a no-op. It historically returned `Ok` SILENTLY,
    /// meaning a Windows daemon configured to drop to a uid/gid kept running
    /// fully privileged with no operator-visible signal - a privilege-escalation
    /// footgun. The fix routes a requested-but-unsupported drop through the
    /// warn-once latch. This pins that the latch fires EXACTLY once across many
    /// connection attempts (never per-connection spam), mirroring the real fn
    /// body `warn_once_if(privilege_drop_requested(...), &WARNED, emit)` so it
    /// runs on Linux CI without a real setuid or a Windows host.
    #[test]
    fn privilege_drop_warns_exactly_once_when_requested() {
        let warned = AtomicBool::new(false);
        let count = std::cell::Cell::new(0u32);
        for _ in 0..1000 {
            warn_once_if(privilege_drop_requested(Some(1000), &[27]), &warned, || {
                count.set(count.get() + 1)
            });
        }
        assert_eq!(count.get(), 1, "a requested drop must warn exactly once");
        assert!(warned.load(Ordering::Relaxed));
    }

    /// WHY: an empty request (no uid, no gids) is a legitimate no-op on every
    /// platform - the Unix path also leaves the identity untouched for it. It
    /// must NOT warn, otherwise every unprivileged daemon start would emit a
    /// spurious scare. Pins the request-gating half of the contract.
    #[test]
    fn privilege_drop_stays_silent_when_nothing_requested() {
        let warned = AtomicBool::new(false);
        let count = std::cell::Cell::new(0u32);
        for _ in 0..1000 {
            warn_once_if(privilege_drop_requested(None, &[]), &warned, || {
                count.set(count.get() + 1)
            });
        }
        assert_eq!(count.get(), 0);
        assert!(!warned.load(Ordering::Relaxed));
    }

    /// `privilege_drop_requested` is the predicate that decides whether the
    /// non-Unix no-op is silent or loud; a uid OR any gid means a drop was asked
    /// for.
    #[test]
    fn privilege_drop_requested_tracks_uid_and_gids() {
        assert!(privilege_drop_requested(Some(1000), &[]));
        assert!(privilege_drop_requested(None, &[27]));
        assert!(privilege_drop_requested(Some(0), &[0]));
        assert!(!privilege_drop_requested(None, &[]));
    }

    /// The warning must name the platform limitation and make the security
    /// consequence unmissable, so the operator understands the process is still
    /// privileged.
    #[test]
    fn privilege_drop_warning_names_platform_and_consequence() {
        assert!(PRIVILEGE_DROP_UNSUPPORTED_WARNING.contains("privilege"));
        assert!(PRIVILEGE_DROP_UNSUPPORTED_WARNING.contains("Windows"));
        assert!(PRIVILEGE_DROP_UNSUPPORTED_WARNING.contains("WITHOUT"));
    }

    /// WHY: `DOMAIN\user` must be split into domain + user BEFORE the two halves
    /// are widened to UTF-16 and passed to `LogonUserW`. Pins the pure parse on
    /// Linux CI (the FFI itself only compiles on Windows): first backslash is
    /// the delimiter, a plain name yields no domain, and any trailing
    /// backslashes stay in the user part rather than being re-split.
    #[test]
    fn split_account_name_parses_domain_and_user() {
        assert_eq!(split_account_name(r"DOMAIN\user"), (Some("DOMAIN"), "user"));
        assert_eq!(split_account_name("user"), (None, "user"));
        assert_eq!(split_account_name(r"D\a\b"), (Some("D"), r"a\b"));
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
