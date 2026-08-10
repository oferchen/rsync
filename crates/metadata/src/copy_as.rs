#![allow(unsafe_code)]

//! Privilege switching for `--copy-as=USER[:GROUP]`.
//!
//! This module provides effective UID/GID switching so that file I/O on the
//! receiver/generator side executes under the specified user (and optionally
//! group) identity. The switch is reversible: [`CopyAsGuard`] restores the
//! original effective identifiers when dropped, mirroring upstream rsync's
//! `do_as_root` / `undo_as_root` bracket in `rsync.c`.
//!
//! # Platform Support
//!
//! - **Unix**: Uses `seteuid(2)` / `setegid(2)` via libc. Requires the
//!   process to be running as root (euid 0) or to possess `CAP_SETUID` /
//!   `CAP_SETGID`.
//! - **Windows**: Returns a descriptive `Unsupported` error. The POSIX
//!   `--copy-as=USER` flow takes no password and assumes the process can
//!   change effective identity without one (root or a `CAP_SETUID`
//!   capability). The Win32 equivalent is `LogonUserW` +
//!   `ImpersonateLoggedOnUser`, which generally requires either a
//!   password (not exposed by `--copy-as`) or `SeTcbPrivilege` /
//!   `SeImpersonatePrivilege` plus an S4U logon. We surface a clear
//!   error rather than silently dropping the option; full token
//!   impersonation is tracked as a follow-up.
//! - **Other non-Unix**: Returns `Unsupported`.
//!
//! # Upstream Reference
//!
//! - `rsync.c:do_as_root()` - switches euid/egid before privileged operations
//! - `rsync.c:undo_as_root()` - restores original euid/egid
//! - `main.c` - `--copy-as` parsing and initial identity resolution

use std::ffi::OsStr;
use std::io;

/// Resolved numeric identifiers from a `--copy-as=USER[:GROUP]` specification.
///
/// After parsing the user and optional group strings into numeric IDs,
/// this struct holds the resolved values ready for `seteuid` / `setegid`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CopyAsIds {
    /// Target effective user ID.
    pub uid: u32,
    /// Target effective group ID, if a group was specified.
    pub gid: Option<u32>,
}

/// Parses a `USER[:GROUP]` specification into resolved numeric identifiers.
///
/// Resolution order for each component:
/// 1. Try parsing as a numeric ID.
/// 2. Fall back to NSS lookup (`getpwnam` / `getgrnam`).
///
/// # Errors
///
/// Returns an error if the user or group cannot be resolved to a numeric ID.
pub fn parse_copy_as_spec(spec: &OsStr) -> io::Result<CopyAsIds> {
    let spec_str = spec.to_string_lossy();
    let (user_part, group_part) = match spec_str.find(':') {
        Some(pos) => {
            let (u, g) = spec_str.split_at(pos);
            (u, Some(&g[1..]))
        }
        None => (spec_str.as_ref(), None),
    };

    if user_part.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "empty user name in --copy-as specification",
        ));
    }

    let uid = resolve_user(user_part)?;
    let gid = match group_part {
        Some(g) if !g.is_empty() => Some(resolve_group(g)?),
        _ => None,
    };

    Ok(CopyAsIds { uid, gid })
}

/// Resolves a user string to a numeric UID.
///
/// Tries numeric parsing first, then falls back to NSS lookup.
fn resolve_user(user: &str) -> io::Result<u32> {
    if let Ok(uid) = user.parse::<u32>() {
        return Ok(uid);
    }

    resolve_user_by_name(user)
}

/// Resolves a group string to a numeric GID.
///
/// Tries numeric parsing first, then falls back to NSS lookup.
fn resolve_group(group: &str) -> io::Result<u32> {
    if let Ok(gid) = group.parse::<u32>() {
        return Ok(gid);
    }

    resolve_group_by_name(group)
}

#[cfg(unix)]
fn resolve_user_by_name(name: &str) -> io::Result<u32> {
    match crate::id_lookup::lookup_user_by_name(name.as_bytes())? {
        Some(uid) => Ok(uid),
        None => Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("unknown user '{name}' in --copy-as"),
        )),
    }
}

#[cfg(unix)]
fn resolve_group_by_name(name: &str) -> io::Result<u32> {
    match crate::id_lookup::lookup_group_by_name(name.as_bytes())? {
        Some(gid) => Ok(gid),
        None => Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("unknown group '{name}' in --copy-as"),
        )),
    }
}

/// RAII guard that restores original effective UID/GID on drop.
///
/// Created by [`switch_effective_ids`]. When dropped, restores the process's
/// effective user and group IDs to their values at the time of the switch.
/// Drop failures are silently ignored since there is no mechanism to propagate
/// errors from `Drop`; however, if the original identity cannot be restored,
/// subsequent file operations will use the switched identity.
#[cfg(unix)]
#[derive(Debug)]
pub struct CopyAsGuard {
    original_euid: u32,
    original_egid: u32,
    switched_egid: bool,
}

#[cfg(unix)]
impl CopyAsGuard {
    /// Returns the original effective UID that will be restored on drop.
    #[cfg(test)]
    pub fn original_euid(&self) -> u32 {
        self.original_euid
    }

    /// Returns the original effective GID that will be restored on drop.
    #[cfg(test)]
    pub fn original_egid(&self) -> u32 {
        self.original_egid
    }
}

#[cfg(unix)]
#[allow(unsafe_code)]
impl Drop for CopyAsGuard {
    fn drop(&mut self) {
        // upstream: rsync.c:undo_as_root() -- restore egid first, then euid.
        // Restoring euid first could fail if the target egid requires root.
        if self.switched_egid {
            // SAFETY: `setegid` is a standard POSIX call. The gid_t value was
            // previously our effective GID, so it is known-valid.
            unsafe {
                libc::setegid(self.original_egid);
            }
        }
        // SAFETY: `seteuid` is a standard POSIX call. The uid_t value was
        // previously our effective UID, so it is known-valid.
        unsafe {
            libc::seteuid(self.original_euid);
        }
    }
}

/// Switches the process effective UID and optionally GID to the specified values.
///
/// Returns a [`CopyAsGuard`] whose `Drop` implementation restores the original
/// effective identifiers. The switch sequence follows upstream rsync's ordering:
/// 1. `setegid` (if a group was specified) -- must happen while still root
/// 2. `seteuid` -- dropping to the target user
///
/// # Errors
///
/// Returns an error if `seteuid(2)` or `setegid(2)` fails. Common causes
/// include insufficient privileges (not running as root or lacking capabilities).
///
/// # Upstream Reference
///
/// - `rsync.c:do_as_root()` - same ordering: egid first, then euid
#[cfg(unix)]
#[allow(unsafe_code)]
pub fn switch_effective_ids(ids: &CopyAsIds) -> io::Result<CopyAsGuard> {
    // SAFETY: `geteuid` and `getegid` are standard POSIX calls with no side effects.
    let original_euid = unsafe { libc::geteuid() };
    let original_egid = unsafe { libc::getegid() };

    let mut switched_egid = false;

    // upstream: rsync.c:do_as_root() - egid first while still root
    if let Some(gid) = ids.gid {
        // SAFETY: `setegid` is a standard POSIX call. The gid_t value comes from
        // a resolved group specification and is validated by the kernel.
        let ret = unsafe { libc::setegid(gid) };
        if ret != 0 {
            return Err(io::Error::last_os_error());
        }
        switched_egid = true;
    }

    // upstream: rsync.c:do_as_root() - euid last (may drop root)
    // SAFETY: `seteuid` is a standard POSIX call. The uid_t value comes from
    // a resolved user specification and is validated by the kernel.
    let ret = unsafe { libc::seteuid(ids.uid) };
    if ret != 0 {
        // upstream: rsync.c:do_as_root() - restore egid on euid failure
        if switched_egid {
            unsafe {
                libc::setegid(original_egid);
            }
        }
        return Err(io::Error::last_os_error());
    }

    Ok(CopyAsGuard {
        original_euid,
        original_egid,
        switched_egid,
    })
}

/// Placeholder guard for non-Unix platforms.
///
/// On Windows and other non-Unix targets, [`switch_effective_ids`] currently
/// fails before any switch occurs, so this struct is never constructed at
/// runtime. It exists so the public type alias compiles on every platform
/// and downstream callers can hold an `Option<CopyAsGuard>` without
/// `cfg`-gating their own code.
#[cfg(not(unix))]
#[derive(Debug)]
pub struct CopyAsGuard {
    _private: (),
}

/// Attempts to switch the calling process identity on Windows.
///
/// The POSIX `--copy-as` contract is "drop to USER[:GROUP] without a
/// password". On Windows this maps to `LogonUserW` +
/// `ImpersonateLoggedOnUser`, which either needs the target user's
/// password (not exposed by the CLI flag) or `SeTcbPrivilege` /
/// `SeImpersonatePrivilege` plus an S4U logon. Until the
/// `CopyAsIds`-driven impersonation flow is wired through to the
/// `platform::privilege::drop_privileges_windows` helper, this function
/// probes the calling process token for `SeImpersonatePrivilege` and
/// returns a descriptive error in both branches so users see a loud
/// failure instead of a silent no-op.
///
/// # Errors
///
/// - `ErrorKind::PermissionDenied` when the process token lacks
///   `SeImpersonatePrivilege`.
/// - `ErrorKind::Unsupported` when the privilege is present (token
///   impersonation flow is not yet wired to this entry point).
/// - Lower-level `io::Error::other` if the probe itself fails
///   (for example, `OpenProcessToken` returns an error).
///
/// # Upstream Reference
///
/// - `rsync.c:do_as_root()` - POSIX equivalent that this routine mirrors.
#[cfg(windows)]
#[allow(unsafe_code)]
pub fn switch_effective_ids(_ids: &CopyAsIds) -> io::Result<CopyAsGuard> {
    if has_impersonate_privilege()? {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "--copy-as on Windows requires LogonUserW + ImpersonateLoggedOnUser \
             token impersonation, which is not yet wired. The calling process \
             has SeImpersonatePrivilege; rerun the transfer on a Unix host or \
             track the follow-up for S4U logon support.",
        ))
    } else {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "--copy-as on Windows requires SeImpersonatePrivilege (or a \
             password-based LogonUserW flow, which --copy-as does not \
             accept). Grant the privilege via secpol.msc -> Local Policies \
             -> User Rights Assignment -> 'Impersonate a client after \
             authentication', or rerun the transfer on a Unix host.",
        ))
    }
}

/// Probes the calling process token for `SeImpersonatePrivilege`.
///
/// Returns `Ok(true)` when the privilege is present and enabled or
/// enabled-by-default, `Ok(false)` when absent, or an `io::Error`
/// wrapping the last OS error when the Win32 calls fail.
///
/// # Safety
///
/// Each `unsafe` block here calls a well-documented Win32 API exactly
/// once with locally owned, correctly aligned out-pointers. The token
/// handle is closed via the `CloseHandleGuard` RAII helper so the
/// privilege probe is leak-free even on error paths.
#[cfg(windows)]
#[allow(unsafe_code)]
fn has_impersonate_privilege() -> io::Result<bool> {
    use windows::Win32::Foundation::{CloseHandle, HANDLE, LUID};
    use windows::Win32::Security::{
        LUID_AND_ATTRIBUTES, LookupPrivilegeValueW, PRIVILEGE_SET, PrivilegeCheck,
        SE_IMPERSONATE_NAME, SE_PRIVILEGE_ENABLED, TOKEN_QUERY,
    };
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    use windows::core::{BOOL, PCWSTR};

    // Win32 `PRIVILEGE_SET.Control` flag: require every listed privilege
    // to be held. The constant is not exported as a named symbol by the
    // `windows` crate (winnt.h `PRIVILEGE_SET_ALL_NECESSARY` == 1).
    const PRIVILEGE_SET_ALL_NECESSARY: u32 = 1;

    // RAII wrapper that closes the process token on drop, including
    // when an early `?` propagates an error.
    struct CloseHandleGuard(HANDLE);
    impl Drop for CloseHandleGuard {
        fn drop(&mut self) {
            if !self.0.is_invalid() {
                // SAFETY: the handle came from OpenProcessToken and is
                // still valid until this Drop runs.
                unsafe {
                    let _ = CloseHandle(self.0);
                }
            }
        }
    }

    let mut token = HANDLE::default();
    // SAFETY: GetCurrentProcess returns a pseudo-handle that does not
    // require closing. OpenProcessToken writes the real token handle
    // into `token`, which we then own and close via the RAII guard.
    unsafe {
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token)
            .map_err(|err| io::Error::other(format!("OpenProcessToken failed: {err}")))?;
    }
    let _token_guard = CloseHandleGuard(token);

    // Resolve the SeImpersonatePrivilege LUID on the local system.
    let mut luid = LUID::default();
    // SAFETY: SE_IMPERSONATE_NAME is a static null-terminated wide
    // string from the `windows` crate; PCWSTR::null() requests the
    // local system; `luid` is a local out-parameter we own.
    unsafe {
        LookupPrivilegeValueW(PCWSTR::null(), SE_IMPERSONATE_NAME, &mut luid)
            .map_err(|err| io::Error::other(format!("LookupPrivilegeValueW failed: {err}")))?;
    }

    let mut privilege_set = PRIVILEGE_SET {
        PrivilegeCount: 1,
        Control: PRIVILEGE_SET_ALL_NECESSARY,
        Privilege: [LUID_AND_ATTRIBUTES {
            Luid: luid,
            Attributes: SE_PRIVILEGE_ENABLED,
        }],
    };
    let mut result = BOOL(0);
    // SAFETY: `token` is a valid TOKEN_QUERY handle (kept alive by the
    // RAII guard); `privilege_set` is a properly initialised
    // single-entry structure we own; `result` is a local BOOL we own.
    unsafe {
        PrivilegeCheck(token, &mut privilege_set, &mut result)
            .map_err(|err| io::Error::other(format!("PrivilegeCheck failed: {err}")))?;
    }

    Ok(result.0 != 0)
}

/// Privilege switch for non-Unix, non-Windows targets.
///
/// Returns `Unsupported` so callers surface a loud error instead of
/// silently dropping `--copy-as`.
#[cfg(not(any(unix, windows)))]
pub fn switch_effective_ids(_ids: &CopyAsIds) -> io::Result<CopyAsGuard> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "--copy-as is not supported on this platform",
    ))
}

#[cfg(not(unix))]
fn resolve_user_by_name(name: &str) -> io::Result<u32> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        format!("user name resolution not supported on this platform: '{name}'"),
    ))
}

#[cfg(not(unix))]
fn resolve_group_by_name(name: &str) -> io::Result<u32> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        format!("group name resolution not supported on this platform: '{name}'"),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    #[test]
    fn parse_user_only_numeric() {
        let spec = OsString::from("1000");
        let ids = parse_copy_as_spec(&spec).unwrap();
        assert_eq!(ids.uid, 1000);
        assert_eq!(ids.gid, None);
    }

    #[test]
    fn parse_user_and_group_numeric() {
        let spec = OsString::from("1000:1001");
        let ids = parse_copy_as_spec(&spec).unwrap();
        assert_eq!(ids.uid, 1000);
        assert_eq!(ids.gid, Some(1001));
    }

    #[test]
    fn parse_user_with_empty_group() {
        let spec = OsString::from("1000:");
        let ids = parse_copy_as_spec(&spec).unwrap();
        assert_eq!(ids.uid, 1000);
        assert_eq!(ids.gid, None);
    }

    #[test]
    fn parse_empty_user_fails() {
        let spec = OsString::from("");
        assert!(parse_copy_as_spec(&spec).is_err());
    }

    #[test]
    fn parse_colon_only_fails() {
        let spec = OsString::from(":1000");
        let err = parse_copy_as_spec(&spec).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    #[cfg(unix)]
    #[test]
    fn parse_root_user_by_name() {
        let spec = OsString::from("root");
        let ids = parse_copy_as_spec(&spec).unwrap();
        assert_eq!(ids.uid, 0);
        assert_eq!(ids.gid, None);
    }

    #[cfg(unix)]
    #[test]
    fn parse_nonexistent_user_fails() {
        let spec = OsString::from("nonexistent_user_xyz_99999");
        assert!(parse_copy_as_spec(&spec).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn parse_nonexistent_group_fails() {
        let spec = OsString::from("0:nonexistent_group_xyz_99999");
        assert!(parse_copy_as_spec(&spec).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn switch_to_current_user_succeeds() {
        // SAFETY: `geteuid`/`getegid` are POSIX accessors with no inputs and
        // no side effects beyond returning the calling process's IDs.
        let euid = unsafe { libc::geteuid() };
        let egid = unsafe { libc::getegid() };
        let ids = CopyAsIds {
            uid: euid,
            gid: Some(egid),
        };
        let guard = switch_effective_ids(&ids).unwrap();
        assert_eq!(guard.original_euid(), euid);
        assert_eq!(guard.original_egid(), egid);
        drop(guard);
        // SAFETY: see above; pure read of the calling process's effective IDs.
        assert_eq!(unsafe { libc::geteuid() }, euid);
        assert_eq!(unsafe { libc::getegid() }, egid);
    }

    #[cfg(unix)]
    #[test]
    fn switch_without_group_preserves_egid() {
        // SAFETY: `geteuid`/`getegid` are POSIX accessors with no inputs and
        // no side effects beyond returning the calling process's IDs.
        let euid = unsafe { libc::geteuid() };
        let egid = unsafe { libc::getegid() };
        let ids = CopyAsIds {
            uid: euid,
            gid: None,
        };
        let guard = switch_effective_ids(&ids).unwrap();
        assert!(!guard.switched_egid);
        drop(guard);
        // SAFETY: see above; pure read of the calling process's effective gid.
        assert_eq!(unsafe { libc::getegid() }, egid);
    }

    #[test]
    fn copy_as_ids_clone() {
        let ids = CopyAsIds {
            uid: 1000,
            gid: Some(1001),
        };
        let cloned = ids;
        assert_eq!(ids, cloned);
    }

    #[test]
    fn copy_as_ids_debug() {
        let ids = CopyAsIds {
            uid: 1000,
            gid: Some(1001),
        };
        let debug = format!("{ids:?}");
        assert!(debug.contains("1000"));
        assert!(debug.contains("1001"));
    }

    #[test]
    fn resolve_user_numeric() {
        assert_eq!(resolve_user("0").unwrap(), 0);
        assert_eq!(resolve_user("65534").unwrap(), 65534);
    }

    #[test]
    fn resolve_group_numeric() {
        assert_eq!(resolve_group("0").unwrap(), 0);
        assert_eq!(resolve_group("65534").unwrap(), 65534);
    }

    #[cfg(unix)]
    #[test]
    fn resolve_user_by_name_root() {
        assert_eq!(resolve_user("root").unwrap(), 0);
    }

    #[cfg(windows)]
    #[test]
    fn windows_switch_returns_descriptive_error() {
        // The probe inspects the calling process token. CI runners
        // typically do not hold SeImpersonatePrivilege, so the error
        // should be PermissionDenied. Elevated runners may instead
        // hit the Unsupported branch. Accept either outcome and
        // assert the error message names the missing capability.
        let ids = CopyAsIds {
            uid: 1000,
            gid: Some(1000),
        };
        let err = switch_effective_ids(&ids).unwrap_err();
        assert!(
            matches!(
                err.kind(),
                io::ErrorKind::PermissionDenied | io::ErrorKind::Unsupported
            ),
            "unexpected error kind {:?}: {err}",
            err.kind()
        );
        let msg = err.to_string();
        assert!(
            msg.contains("--copy-as"),
            "error message should mention --copy-as: {msg}"
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_switch_without_group_returns_error() {
        let ids = CopyAsIds { uid: 0, gid: None };
        let err = switch_effective_ids(&ids).unwrap_err();
        assert!(matches!(
            err.kind(),
            io::ErrorKind::PermissionDenied | io::ErrorKind::Unsupported
        ));
    }

    #[cfg(windows)]
    #[test]
    fn windows_privilege_probe_does_not_panic() {
        // Whatever the answer, the probe must complete without panicking
        // and without leaking the process token.
        let _ = has_impersonate_privilege().expect("privilege probe should succeed");
    }

    #[cfg(not(unix))]
    #[test]
    fn non_unix_resolve_user_by_name_returns_unsupported() {
        let err = resolve_user_by_name("root").unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::Unsupported);
    }

    #[cfg(not(unix))]
    #[test]
    fn non_unix_resolve_group_by_name_returns_unsupported() {
        let err = resolve_group_by_name("wheel").unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::Unsupported);
    }

    #[cfg(not(unix))]
    #[test]
    fn non_unix_parse_named_user_fails() {
        let spec = OsString::from("root");
        let err = parse_copy_as_spec(&spec).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::Unsupported);
    }

    #[cfg(not(unix))]
    #[test]
    fn non_unix_parse_named_group_fails() {
        let spec = OsString::from("0:wheel");
        let err = parse_copy_as_spec(&spec).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::Unsupported);
    }
}
