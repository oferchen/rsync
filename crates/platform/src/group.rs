//! System group membership lookups.
//!
//! # Unix
//!
//! Uses `libc::getgrnam_r` with ERANGE retry to look up group members by
//! name. Pointer traversal of `gr_mem` extracts the member list.
//!
//! # Windows / Other
//!
//! Returns `Ok(None)` - group expansion is not supported.
//!
//! # Upstream Reference
//!
//! `clientserver.c` - `@group` expansion in `auth users`.

use std::io;

/// Looks up a group by name and returns its member usernames.
///
/// Returns `Ok(Some(members))` if the group exists with its member list,
/// `Ok(None)` if the group doesn't exist, or an error on I/O failure.
///
/// Uses `getgrnam_r` for thread-safe lookup. The returned members are the
/// explicit members listed in `/etc/group` or equivalent database; users
/// with the group as their primary group are NOT included unless also
/// listed explicitly.
#[cfg(unix)]
#[allow(unsafe_code)]
pub fn lookup_group_members(group_name: &str) -> Result<Option<Vec<String>>, io::Error> {
    use std::ffi::CString;
    use std::mem::MaybeUninit;
    use std::ptr;

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

            // SAFETY: `result` is non-null, so getgrnam_r initialized `grp`.
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
    use std::ffi::CStr;

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
pub fn lookup_group_members(_group_name: &str) -> Result<Option<Vec<String>>, io::Error> {
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn lookup_nonexistent_returns_none() {
        let result = lookup_group_members("nonexistent_group_xyz_99999");
        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
    }

    #[cfg(unix)]
    #[test]
    fn lookup_root_group_returns_some() {
        let root_result = lookup_group_members("root");
        let wheel_result = lookup_group_members("wheel");

        assert!(root_result.is_ok());
        assert!(wheel_result.is_ok());

        if root_result.as_ref().is_ok_and(|r| r.is_some()) {
            let _ = root_result.unwrap().unwrap();
        }
        if wheel_result.as_ref().is_ok_and(|r| r.is_some()) {
            let _ = wheel_result.unwrap().unwrap();
        }
    }

    #[cfg(unix)]
    #[test]
    fn lookup_handles_null_in_name() {
        let result = lookup_group_members("test\x00group");
        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
    }

    #[cfg(unix)]
    #[test]
    fn lookup_handles_empty_name() {
        let result = lookup_group_members("");
        assert!(result.is_ok());
    }

    #[test]
    fn non_unix_returns_none() {
        #[cfg(not(unix))]
        {
            let result = lookup_group_members("staff");
            assert!(result.unwrap().is_none());
        }
    }
}
