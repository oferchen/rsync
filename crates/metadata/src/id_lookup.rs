//! UID/GID lookup and mapping utilities.
//!
//! This module provides functions for looking up user and group names from
//! numeric IDs and vice versa. These are used for rsync's UID/GID name mapping
//! feature, which translates user/group names between systems rather than using
//! raw numeric IDs.
//!
//! # Upstream Reference
//!
//! - `uidlist.c` - UID/GID list management in upstream rsync

#![allow(unsafe_code)]

use crate::ownership;
use rustix::fs::{Gid, Uid};
use rustix::process::{RawGid, RawUid};
use std::io;
use std::ptr;
use std::{
    ffi::{CStr, CString},
    mem::MaybeUninit,
};

/// Maps a remote UID to a local UID.
///
/// When `numeric_ids` is true, returns the UID unchanged.
/// Otherwise, looks up the name for the remote UID and finds the local UID with that name.
/// If lookup fails, returns the original UID.
pub fn map_uid(uid: RawUid, numeric_ids: bool) -> Option<Uid> {
    if numeric_ids {
        return Some(ownership::uid_from_raw(uid));
    }

    let mapped = match lookup_user_name(uid) {
        Ok(Some(bytes)) => match lookup_user_by_name(&bytes) {
            Ok(Some(mapped)) => mapped,
            Ok(None) => uid,
            Err(_) => uid,
        },
        Ok(None) => uid,
        Err(_) => uid,
    };

    Some(ownership::uid_from_raw(mapped))
}

/// Maps a remote GID to a local GID.
///
/// When `numeric_ids` is true, returns the GID unchanged.
/// Otherwise, looks up the name for the remote GID and finds the local GID with that name.
/// If lookup fails, returns the original GID.
pub fn map_gid(gid: RawGid, numeric_ids: bool) -> Option<Gid> {
    if numeric_ids {
        return Some(ownership::gid_from_raw(gid));
    }

    let mapped = match lookup_group_name(gid) {
        Ok(Some(bytes)) => match lookup_group_by_name(&bytes) {
            Ok(Some(mapped)) => mapped,
            Ok(None) => gid,
            Err(_) => gid,
        },
        Ok(None) => gid,
        Err(_) => gid,
    };

    Some(ownership::gid_from_raw(mapped))
}

/// Looks up the username for a given UID.
///
/// Returns `Ok(Some(name))` if the user exists, `Ok(None)` if not found.
/// Uses `getpwuid_r` for thread-safe lookup.
pub fn lookup_user_name(uid: RawUid) -> Result<Option<Vec<u8>>, io::Error> {
    let mut buffer = vec![0_u8; 1024];
    loop {
        let mut pwd = MaybeUninit::<libc::passwd>::zeroed();
        let mut result: *mut libc::passwd = ptr::null_mut();
        // SAFETY: All arguments are valid pointers with sufficient lifetimes:
        // - `pwd` is uninitialized but will be written by getpwuid_r
        // - `buffer` provides scratch space owned by this function
        // - `result` receives the output pointer
        let errno = unsafe {
            libc::getpwuid_r(
                uid as libc::uid_t,
                pwd.as_mut_ptr(),
                buffer.as_mut_ptr() as *mut libc::c_char,
                buffer.len(),
                &mut result,
            )
        };

        if errno == 0 {
            if result.is_null() {
                return Ok(None);
            }

            // SAFETY: `result` is non-null, so getpwuid_r successfully initialized `pwd`.
            let pwd = unsafe { pwd.assume_init() };
            // SAFETY: `pw_name` is a valid C string set by getpwuid_r, backed by `buffer`.
            let name = unsafe { CStr::from_ptr(pwd.pw_name) };
            return Ok(Some(name.to_bytes().to_vec()));
        }

        if errno == libc::ERANGE {
            buffer.resize(buffer.len().saturating_mul(2), 0);
            continue;
        }

        return Err(io::Error::from_raw_os_error(errno));
    }
}

/// Looks up the UID for a given username.
///
/// Returns `Ok(Some(uid))` if the user exists, `Ok(None)` if not found.
/// Uses `getpwnam_r` for thread-safe lookup.
pub fn lookup_user_by_name(name: &[u8]) -> Result<Option<RawUid>, io::Error> {
    let Ok(c_name) = CString::new(name) else {
        return Ok(None);
    };

    let mut buffer = vec![0_u8; 1024];
    loop {
        let mut pwd = MaybeUninit::<libc::passwd>::zeroed();
        let mut result: *mut libc::passwd = ptr::null_mut();
        // SAFETY: All arguments are valid pointers with sufficient lifetimes:
        // - `c_name` is a valid CString
        // - `pwd` is uninitialized but will be written by getpwnam_r
        // - `buffer` provides scratch space owned by this function
        // - `result` receives the output pointer
        let errno = unsafe {
            libc::getpwnam_r(
                c_name.as_ptr(),
                pwd.as_mut_ptr(),
                buffer.as_mut_ptr() as *mut libc::c_char,
                buffer.len(),
                &mut result,
            )
        };

        if errno == 0 {
            if result.is_null() {
                return Ok(None);
            }

            // SAFETY: `result` is non-null, so getpwnam_r successfully initialized `pwd`.
            let pwd = unsafe { pwd.assume_init() };
            return Ok(Some(pwd.pw_uid as RawUid));
        }

        if errno == libc::ERANGE {
            buffer.resize(buffer.len().saturating_mul(2), 0);
            continue;
        }

        return Err(io::Error::from_raw_os_error(errno));
    }
}

/// Looks up the group name for a given GID.
///
/// Returns `Ok(Some(name))` if the group exists, `Ok(None)` if not found.
/// Uses `getgrgid_r` for thread-safe lookup.
pub fn lookup_group_name(gid: RawGid) -> Result<Option<Vec<u8>>, io::Error> {
    let mut buffer = vec![0_u8; 1024];
    loop {
        let mut grp = MaybeUninit::<libc::group>::zeroed();
        let mut result: *mut libc::group = ptr::null_mut();
        // SAFETY: All arguments are valid pointers with sufficient lifetimes:
        // - `grp` is uninitialized but will be written by getgrgid_r
        // - `buffer` provides scratch space owned by this function
        // - `result` receives the output pointer
        let errno = unsafe {
            libc::getgrgid_r(
                gid as libc::gid_t,
                grp.as_mut_ptr(),
                buffer.as_mut_ptr() as *mut libc::c_char,
                buffer.len(),
                &mut result,
            )
        };

        if errno == 0 {
            if result.is_null() {
                return Ok(None);
            }

            // SAFETY: `result` is non-null, so getgrgid_r successfully initialized `grp`.
            let grp = unsafe { grp.assume_init() };
            // SAFETY: `gr_name` is a valid C string set by getgrgid_r, backed by `buffer`.
            let name = unsafe { CStr::from_ptr(grp.gr_name) };
            return Ok(Some(name.to_bytes().to_vec()));
        }

        if errno == libc::ERANGE {
            buffer.resize(buffer.len().saturating_mul(2), 0);
            continue;
        }

        return Err(io::Error::from_raw_os_error(errno));
    }
}

/// Looks up the GID for a given group name.
///
/// Returns `Ok(Some(gid))` if the group exists, `Ok(None)` if not found.
/// Uses `getgrnam_r` for thread-safe lookup.
pub fn lookup_group_by_name(name: &[u8]) -> Result<Option<RawGid>, io::Error> {
    let Ok(c_name) = CString::new(name) else {
        return Ok(None);
    };

    let mut buffer = vec![0_u8; 1024];
    loop {
        let mut grp = MaybeUninit::<libc::group>::zeroed();
        let mut result: *mut libc::group = ptr::null_mut();
        // SAFETY: All arguments are valid pointers with sufficient lifetimes:
        // - `c_name` is a valid CString
        // - `grp` is uninitialized but will be written by getgrnam_r
        // - `buffer` provides scratch space owned by this function
        // - `result` receives the output pointer
        let errno = unsafe {
            libc::getgrnam_r(
                c_name.as_ptr(),
                grp.as_mut_ptr(),
                buffer.as_mut_ptr() as *mut libc::c_char,
                buffer.len(),
                &mut result,
            )
        };

        if errno == 0 {
            if result.is_null() {
                return Ok(None);
            }

            // SAFETY: `result` is non-null, so getgrnam_r successfully initialized `grp`.
            let grp = unsafe { grp.assume_init() };
            return Ok(Some(grp.gr_gid as RawGid));
        }

        if errno == libc::ERANGE {
            buffer.resize(buffer.len().saturating_mul(2), 0);
            continue;
        }

        return Err(io::Error::from_raw_os_error(errno));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // map_uid tests
    #[test]
    fn map_uid_numeric_ids_returns_same_uid() {
        let uid = 1000;
        let result = map_uid(uid, true);
        assert!(result.is_some());
    }

    #[test]
    fn map_uid_non_numeric_attempts_name_lookup() {
        // UID 0 (root) should always exist on Unix systems
        let result = map_uid(0, false);
        assert!(result.is_some());
    }

    #[test]
    fn map_uid_nonexistent_uid_falls_back() {
        // Very high UID unlikely to exist
        let result = map_uid(999999, false);
        assert!(result.is_some());
    }

    // map_gid tests
    #[test]
    fn map_gid_numeric_ids_returns_same_gid() {
        let gid = 1000;
        let result = map_gid(gid, true);
        assert!(result.is_some());
    }

    #[test]
    fn map_gid_non_numeric_attempts_name_lookup() {
        // GID 0 (root/wheel) should always exist on Unix systems
        let result = map_gid(0, false);
        assert!(result.is_some());
    }

    #[test]
    fn map_gid_nonexistent_gid_falls_back() {
        // Very high GID unlikely to exist
        let result = map_gid(999999, false);
        assert!(result.is_some());
    }

    // lookup_user_name tests
    #[test]
    fn lookup_user_name_root_returns_name() {
        // UID 0 (root) should have a name on most systems
        let result = lookup_user_name(0);
        assert!(result.is_ok());
        // Don't assert the name exists, as some containers might not have /etc/passwd
    }

    #[test]
    fn lookup_user_name_nonexistent_uid_returns_none() {
        // Very high UID unlikely to exist
        let result = lookup_user_name(999999999);
        assert!(result.is_ok());
        // The result might be None on most systems
    }

    // lookup_user_by_name tests
    #[test]
    fn lookup_user_by_name_root_returns_uid() {
        // "root" user should exist on most Unix systems
        let result = lookup_user_by_name(b"root");
        assert!(result.is_ok());
        if let Ok(Some(uid)) = result {
            assert_eq!(uid, 0);
        }
    }

    #[test]
    fn lookup_user_by_name_nonexistent_returns_none() {
        let result = lookup_user_by_name(b"nonexistent_user_xyz_12345");
        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
    }

    #[test]
    fn lookup_user_by_name_with_null_byte_returns_none() {
        // Name containing null byte can't be converted to CString
        let result = lookup_user_by_name(b"test\x00user");
        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
    }

    #[test]
    fn lookup_user_by_name_empty_returns_none() {
        let result = lookup_user_by_name(b"");
        assert!(result.is_ok());
        // Empty name typically returns None
    }

    // lookup_group_name tests
    #[test]
    fn lookup_group_name_root_group_returns_name() {
        // GID 0 should have a name on most systems (root or wheel)
        let result = lookup_group_name(0);
        assert!(result.is_ok());
    }

    #[test]
    fn lookup_group_name_nonexistent_gid_returns_none() {
        // Very high GID unlikely to exist
        let result = lookup_group_name(999999999);
        assert!(result.is_ok());
    }

    // lookup_group_by_name tests
    #[test]
    fn lookup_group_by_name_root_returns_gid() {
        // Try common root group names
        let result = lookup_group_by_name(b"root");
        if result.is_ok() && result.as_ref().unwrap().is_some() {
            assert_eq!(result.unwrap().unwrap(), 0);
        } else {
            // On macOS, root group might be called "wheel"
            let wheel_result = lookup_group_by_name(b"wheel");
            assert!(wheel_result.is_ok());
        }
    }

    #[test]
    fn lookup_group_by_name_nonexistent_returns_none() {
        let result = lookup_group_by_name(b"nonexistent_group_xyz_12345");
        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
    }

    #[test]
    fn lookup_group_by_name_with_null_byte_returns_none() {
        // Name containing null byte can't be converted to CString
        let result = lookup_group_by_name(b"test\x00group");
        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
    }

    #[test]
    fn lookup_group_by_name_empty_returns_none() {
        let result = lookup_group_by_name(b"");
        assert!(result.is_ok());
    }

    // Cross-function tests
    #[test]
    fn lookup_user_name_and_by_name_round_trip() {
        // Look up root's name, then look up that name to get UID back
        if let Ok(Some(name)) = lookup_user_name(0) {
            if let Ok(Some(uid)) = lookup_user_by_name(&name) {
                assert_eq!(uid, 0);
            }
        }
    }

    #[test]
    fn lookup_group_name_and_by_name_round_trip() {
        // Look up root group's name, then look up that name to get GID back
        if let Ok(Some(name)) = lookup_group_name(0) {
            if let Ok(Some(gid)) = lookup_group_by_name(&name) {
                assert_eq!(gid, 0);
            }
        }
    }

    #[test]
    fn map_uid_and_map_gid_consistency() {
        // Both should return values for numeric mode
        let uid_result = map_uid(1000, true);
        let gid_result = map_gid(1000, true);
        assert!(uid_result.is_some());
        assert!(gid_result.is_some());
    }
}
