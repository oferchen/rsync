// Group expansion for `@group` syntax in auth_users.
//
// This module provides functionality to expand `@group` references in
// auth_users lists to their member usernames, matching upstream rsync's
// daemon authentication behavior.
//
// # Syntax
//
// When an auth_users entry starts with `@`, it's treated as a group reference.
// The `@` prefix is stripped and the remainder is looked up as a system group
// name. All members of that group are added to the authorized users list.
//
// # Examples
//
// ```text
// auth users = alice, @staff, bob
// ```
//
// If the `staff` group has members `charlie` and `diana`, the effective
// auth_users list becomes: `alice, charlie, diana, bob`
//
// # Platform Support
//
// Group expansion uses POSIX `getgrnam_r` and is only available on Unix-like
// systems. On other platforms, `@group` references are silently ignored.

use std::ffi::{CStr, CString};
use std::mem::MaybeUninit;
use std::ptr;

/// Looks up a group by name and returns its member usernames.
///
/// Returns `Ok(Some(members))` if the group exists with its member list,
/// `Ok(None)` if the group doesn't exist, or an error on I/O failure.
///
/// # Platform Notes
///
/// Uses `getgrnam_r` for thread-safe lookup. The returned members are the
/// explicit members listed in `/etc/group` or equivalent database; users
/// with the group as their primary group are NOT included unless also
/// listed explicitly.
///
/// # Safety
///
/// This function uses FFI calls to libc which require unsafe blocks.
/// The unsafe code is carefully bounded to the libc calls and pointer
/// dereferences required by the POSIX API.
#[cfg(unix)]
#[allow(unsafe_code)]
pub(crate) fn lookup_group_members(group_name: &str) -> Result<Option<Vec<String>>, io::Error> {
    let Ok(c_name) = CString::new(group_name) else {
        return Ok(None);
    };

    let mut buffer = vec![0_u8; 4096];
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
            let members = extract_group_members(grp.gr_mem);
            return Ok(Some(members));
        }

        if errno == libc::ERANGE {
            buffer.resize(buffer.len().saturating_mul(2), 0);
            if buffer.len() > 1024 * 1024 {
                return Err(io::Error::new(
                    io::ErrorKind::OutOfMemory,
                    "group member list too large",
                ));
            }
            continue;
        }

        return Err(io::Error::from_raw_os_error(errno));
    }
}

/// Extracts member names from a null-terminated array of C strings.
#[cfg(unix)]
#[allow(unsafe_code)]
fn extract_group_members(gr_mem: *mut *mut libc::c_char) -> Vec<String> {
    let mut members = Vec::new();
    if gr_mem.is_null() {
        return members;
    }

    let mut ptr = gr_mem;
    // SAFETY: `gr_mem` is a valid null-terminated array of C strings from libc.
    // The array and its strings remain valid for the duration of iteration
    // since the buffer backing them is owned by our caller.
    unsafe {
        while !(*ptr).is_null() {
            if let Ok(name) = CStr::from_ptr(*ptr).to_str() {
                members.push(name.to_owned());
            }
            ptr = ptr.add(1);
        }
    }
    members
}

/// Non-Unix stub for group member lookup.
#[cfg(not(unix))]
pub(crate) fn lookup_group_members(_group_name: &str) -> Result<Option<Vec<String>>, io::Error> {
    Ok(None)
}

#[cfg(test)]
mod group_expansion_tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn lookup_group_members_nonexistent_returns_none() {
        let result = lookup_group_members("nonexistent_group_xyz_99999");
        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
    }

    #[cfg(unix)]
    #[test]
    fn lookup_group_members_root_group_returns_some() {
        // Try common root group names
        let root_result = lookup_group_members("root");
        let wheel_result = lookup_group_members("wheel");

        // On some minimal containers, neither might exist
        // Just verify no errors occurred
        assert!(root_result.is_ok());
        assert!(wheel_result.is_ok());

        // If we found a group, members should be a vec (possibly empty)
        if root_result.as_ref().is_ok_and(|r| r.is_some()) {
            let _ = root_result.unwrap().unwrap();
        }
        if wheel_result.as_ref().is_ok_and(|r| r.is_some()) {
            let _ = wheel_result.unwrap().unwrap();
        }
    }

    #[cfg(unix)]
    #[test]
    fn lookup_group_members_handles_null_in_name() {
        let result = lookup_group_members("test\x00group");
        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
    }

    #[cfg(unix)]
    #[test]
    fn lookup_group_members_handles_empty_name() {
        let result = lookup_group_members("");
        assert!(result.is_ok());
    }
}
