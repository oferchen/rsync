#![allow(unsafe_code)]

//! Privilege switching for `--copy-as=USER[:GROUP]`.
//!
//! This module provides a permanent, irreversible privilege drop so that file
//! I/O on the receiver/generator side executes under the specified user (and
//! optionally group) identity, mirroring upstream rsync's
//! `become_copy_as_user()` (main.c). Unlike a reversible `seteuid` switch, the
//! drop sets the real, effective, AND saved-set uid and clears root's
//! supplementary groups, so the process can never regain root and never
//! retains privileged group memberships while writing files.
//!
//! # Platform Support
//!
//! - **Unix**: `setgid` -> `setgroups`/`initgroups` -> `setuid` -> `seteuid`
//!   via `nix` safe wrappers (with a `libc` `setgroups` fallback on Apple
//!   targets). Requires the process to be running as root (euid 0) or to
//!   possess `CAP_SETUID` / `CAP_SETGID`.
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
//! - `main.c:become_copy_as_user()` (rsync 3.2.0+) - the permanent
//!   `setgid`/`setgroups`/`setuid` drop this module mirrors
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

/// Maps a `nix` errno into a `std::io::Error`.
#[cfg(unix)]
fn nix_to_io(err: nix::errno::Errno) -> io::Error {
    io::Error::from_raw_os_error(err as i32)
}

/// Replaces the process supplementary group set with exactly `gids`.
///
/// Uses `nix::unistd::setgroups` where available; falls back to `libc` on
/// Apple targets (where `nix` does not expose `setgroups`), mirroring
/// `platform::privilege::set_supplementary_groups`.
#[cfg(all(unix, not(any(target_os = "macos", target_os = "ios"))))]
fn set_supplementary_groups(gids: &[nix::unistd::Gid]) -> io::Result<()> {
    nix::unistd::setgroups(gids).map_err(nix_to_io)
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
#[allow(unsafe_code)]
fn set_supplementary_groups(gids: &[nix::unistd::Gid]) -> io::Result<()> {
    let raw: Vec<libc::gid_t> = gids.iter().map(|gid| gid.as_raw()).collect();
    // SAFETY: `setgroups` reads exactly `raw.len()` entries from `raw`, which
    // is a live, correctly-sized slice for the duration of the call.
    let rc = unsafe { libc::setgroups(raw.len() as libc::c_int, raw.as_ptr()) };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Installs the user's full supplementary group set (upstream `initgroups`).
///
/// Uses `nix::unistd::initgroups` where available; falls back to `libc` on
/// Apple targets (where `nix` does not expose `initgroups`), mirroring the
/// `set_supplementary_groups` split.
#[cfg(all(unix, not(any(target_os = "macos", target_os = "ios"))))]
fn install_user_groups(user: &std::ffi::CStr, gid: nix::unistd::Gid) -> io::Result<()> {
    nix::unistd::initgroups(user, gid).map_err(nix_to_io)
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
#[allow(unsafe_code)]
fn install_user_groups(user: &std::ffi::CStr, gid: nix::unistd::Gid) -> io::Result<()> {
    // SAFETY: `user` is a live null-terminated C string for the duration of the
    // call; `initgroups` reads it and installs the caller's group set. The
    // `basegid` argument is `c_int` on Apple targets, hence the `as _` cast.
    let rc = unsafe { libc::initgroups(user.as_ptr(), gid.as_raw() as _) };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Permanently drops the process to the `--copy-as` user (and group)
/// identity, mirroring upstream rsync's `become_copy_as_user()`.
///
/// The drop is COMPLETE and IRREVERSIBLE, in the canonical secure order
/// ("Setuid Demystified"): supplementary groups are cleared/replaced, the gid
/// is set, then the uid is set LAST via `setuid(2)`, which sets the real,
/// effective, AND saved-set uid. Consequences that match upstream and that the
/// prior reversible `seteuid`-only switch did NOT provide:
///
/// - the process cannot regain root (saved-set uid is no longer 0), so a
///   compromised transfer has no re-elevation path, and
/// - root's supplementary group memberships are dropped, so files are never
///   created with groups the target user does not hold.
///
/// # Errors
///
/// Returns an error if the target uid cannot be resolved to a passwd entry, or
/// if any of `setgid`/`setgroups`/`initgroups`/`setuid`/`seteuid` fails
/// (typically insufficient privilege: the process must be root or hold
/// `CAP_SETUID`/`CAP_SETGID`).
///
/// # Upstream Reference
///
/// - `main.c:become_copy_as_user()` (rsync 3.2.0+): `setgid` ->
///   `setgroups(1,&gid)`/`initgroups(user,gid)` -> `setuid` -> `seteuid`, then
///   re-cache `our_uid`/`our_gid`/`am_root`.
#[cfg(unix)]
pub fn become_copy_as_user(ids: &CopyAsIds) -> io::Result<()> {
    use nix::unistd::{Gid, Uid, User};

    // Resolve the target user's passwd entry once: its login gid (used when no
    // explicit `--copy-as` group is given) and name (needed for initgroups).
    let user = User::from_uid(Uid::from_raw(ids.uid))
        .map_err(nix_to_io)?
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("unknown uid {} for --copy-as", ids.uid),
            )
        })?;

    // Explicit `--copy-as=user:group` gid, else the user's login gid
    // (upstream uses `pw->pw_gid` when no group is specified).
    let gid = Gid::from_raw(ids.gid.unwrap_or_else(|| user.gid.as_raw()));

    // Order is load-bearing and every call must run while still privileged:
    //   1. setgid, 2. drop/replace supplementary groups, 3. setuid LAST.
    nix::unistd::setgid(gid).map_err(nix_to_io)?;

    if ids.gid.is_some() {
        // Explicit group: restrict the supplementary set to just it
        // (upstream `setgroups(1, &gid)`).
        set_supplementary_groups(&[gid])?;
    } else {
        // No explicit group: install the user's full group set
        // (upstream `initgroups(user, gid)`), which also clears root's groups.
        let cname = std::ffi::CString::new(user.name.as_bytes())
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, err))?;
        install_user_groups(&cname, gid)?;
    }

    // `setuid` sets the real, effective, and SAVED uid, making the drop
    // irreversible; the trailing `seteuid` mirrors upstream's belt-and-braces.
    let uid = Uid::from_raw(ids.uid);
    nix::unistd::setuid(uid).map_err(nix_to_io)?;
    nix::unistd::seteuid(uid).map_err(nix_to_io)?;

    // Re-cache the process identity so the metadata ownership gates observe the
    // new uid/gid (upstream re-runs `our_uid = MY_UID(); am_root = ...`).
    crate::identity::set_process_identity(ids.uid, gid.as_raw());
    Ok(())
}

/// Attempts to drop to the `--copy-as` identity on Windows.
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
pub fn become_copy_as_user(_ids: &CopyAsIds) -> io::Result<()> {
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
pub fn become_copy_as_user(_ids: &CopyAsIds) -> io::Result<()> {
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
    fn become_copy_as_user_unknown_uid_fails_before_dropping() {
        // `become_copy_as_user` performs a PERMANENT, irreversible privilege
        // drop, so it cannot be exercised for real inside the shared test
        // process without corrupting every later test. The one branch that is
        // safe to unit-test is the passwd lookup, which runs BEFORE any
        // privileged syscall: an unresolvable uid must fail with NotFound and
        // leave the process identity untouched. The real drop is validated by
        // an as-root integration run against upstream, not here.
        //
        // SAFETY: `geteuid`/`getegid` are POSIX accessors with no inputs and
        // no side effects beyond returning the calling process's IDs.
        let euid_before = unsafe { libc::geteuid() };
        let egid_before = unsafe { libc::getegid() };

        // No single uid constant is portably unallocated: macOS maps `nobody`
        // to `(uid_t)-2` == 0xFFFF_FFFE, Linux maps it to 65534. Scan down from
        // the top of the range until we find a uid with no passwd entry, so the
        // lookup inside become_copy_as_user misses BEFORE any privileged syscall.
        let uid = {
            use nix::unistd::{Uid, User};
            let mut raw = 0xFFFF_FFFEu32;
            loop {
                if User::from_uid(Uid::from_raw(raw)).ok().flatten().is_none() {
                    break raw;
                }
                raw -= 1;
            }
        };
        let ids = CopyAsIds { uid, gid: None };
        let err = become_copy_as_user(&ids).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);

        // SAFETY: see above; pure read of the calling process's effective IDs.
        assert_eq!(unsafe { libc::geteuid() }, euid_before);
        assert_eq!(unsafe { libc::getegid() }, egid_before);
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
        let err = become_copy_as_user(&ids).unwrap_err();
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
        let err = become_copy_as_user(&ids).unwrap_err();
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
