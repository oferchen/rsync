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
//! - **Non-Unix**: Provides no-op stubs that log a warning and succeed.
//!
//! # Upstream Reference
//!
//! - `rsync.c:do_as_root()` — switches euid/egid before privileged operations
//! - `rsync.c:undo_as_root()` — restores original euid/egid
//! - `main.c` — `--copy-as` parsing and initial identity resolution

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
            // Skip the ':' separator
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

// ==================== Unix Implementation ====================

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
/// - `rsync.c:do_as_root()` — same ordering: egid first, then euid
#[cfg(unix)]
#[allow(unsafe_code)]
pub fn switch_effective_ids(ids: &CopyAsIds) -> io::Result<CopyAsGuard> {
    // SAFETY: `geteuid` and `getegid` are standard POSIX calls with no side effects.
    let original_euid = unsafe { libc::geteuid() };
    let original_egid = unsafe { libc::getegid() };

    let mut switched_egid = false;

    // Switch group first while we still have root privileges
    if let Some(gid) = ids.gid {
        // SAFETY: `setegid` is a standard POSIX call. The gid_t value comes from
        // a resolved group specification and is validated by the kernel.
        let ret = unsafe { libc::setegid(gid) };
        if ret != 0 {
            return Err(io::Error::last_os_error());
        }
        switched_egid = true;
    }

    // Switch user (this may drop root privileges)
    // SAFETY: `seteuid` is a standard POSIX call. The uid_t value comes from
    // a resolved user specification and is validated by the kernel.
    let ret = unsafe { libc::seteuid(ids.uid) };
    if ret != 0 {
        // Restore egid if we changed it before failing
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

// ==================== Non-Unix Implementation ====================

/// No-op guard for non-Unix platforms.
///
/// Since effective UID/GID switching is not supported outside Unix, this
/// guard does nothing on construction or drop.
#[cfg(not(unix))]
pub struct CopyAsGuard {
    _private: (),
}

/// No-op privilege switch for non-Unix platforms.
///
/// Always succeeds and returns a no-op guard. The `--copy-as` option has
/// no effect on non-Unix platforms.
#[cfg(not(unix))]
pub fn switch_effective_ids(_ids: &CopyAsIds) -> io::Result<CopyAsGuard> {
    Ok(CopyAsGuard { _private: () })
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
        // Verify we are back to the original identity
        assert_eq!(unsafe { libc::geteuid() }, euid);
        assert_eq!(unsafe { libc::getegid() }, egid);
    }

    #[cfg(unix)]
    #[test]
    fn switch_without_group_preserves_egid() {
        let euid = unsafe { libc::geteuid() };
        let egid = unsafe { libc::getegid() };
        let ids = CopyAsIds {
            uid: euid,
            gid: None,
        };
        let guard = switch_effective_ids(&ids).unwrap();
        assert!(!guard.switched_egid);
        drop(guard);
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

    #[cfg(not(unix))]
    #[test]
    fn non_unix_switch_returns_noop_guard() {
        let ids = CopyAsIds {
            uid: 1000,
            gid: Some(1000),
        };
        let guard = switch_effective_ids(&ids);
        assert!(guard.is_ok());
    }
}
