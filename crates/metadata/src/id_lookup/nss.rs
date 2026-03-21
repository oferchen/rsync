//! POSIX NSS lookup functions for UID/GID name resolution.
//!
//! Wraps the thread-safe `_r` variants of `getpwuid`, `getpwnam`, `getgrgid`,
//! and `getgrnam`. Each function checks the thread-local name converter first
//! and falls back to the libc call only when no converter is installed.
//!
//! upstream: uidlist.c - getpwuid_r / getpwnam_r / getgrgid_r / getgrnam_r usage

#![allow(unsafe_code)]

use super::converter::NAME_CONVERTER_SLOT;
use rustix::process::{RawGid, RawUid};
use std::ffi::{CStr, CString};
use std::io;
use std::mem::MaybeUninit;
use std::ptr;

/// Looks up the username for a given UID.
///
/// Returns `Ok(Some(name))` if the user exists, `Ok(None)` if not found.
/// Uses `getpwuid_r` for thread-safe lookup. When a name converter is
/// installed via [`super::set_name_converter`], delegates to it instead.
pub fn lookup_user_name(uid: RawUid) -> Result<Option<Vec<u8>>, io::Error> {
    // upstream: uidlist.c:110-116 - name_converter replaces getpwuid
    let converted = NAME_CONVERTER_SLOT.with(|slot| {
        slot.borrow_mut()
            .as_mut()
            .and_then(|nc| nc.uid_to_name(uid))
    });
    if let Some(name) = converted {
        return Ok(Some(name.into_bytes()));
    }

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
/// Uses `getpwnam_r` for thread-safe lookup. When a name converter is
/// installed via [`super::set_name_converter`], delegates to it instead.
pub fn lookup_user_by_name(name: &[u8]) -> Result<Option<RawUid>, io::Error> {
    // upstream: uidlist.c:138-144 - name_converter replaces getpwnam
    if let Ok(name_str) = std::str::from_utf8(name) {
        let converted = NAME_CONVERTER_SLOT.with(|slot| {
            slot.borrow_mut()
                .as_mut()
                .and_then(|nc| nc.name_to_uid(name_str))
        });
        if let Some(uid) = converted {
            return Ok(Some(uid as RawUid));
        }
    }

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
/// Uses `getgrgid_r` for thread-safe lookup. When a name converter is
/// installed via [`super::set_name_converter`], delegates to it instead.
pub fn lookup_group_name(gid: RawGid) -> Result<Option<Vec<u8>>, io::Error> {
    // upstream: uidlist.c:153-159 - name_converter replaces getgrgid
    let converted = NAME_CONVERTER_SLOT.with(|slot| {
        slot.borrow_mut()
            .as_mut()
            .and_then(|nc| nc.gid_to_name(gid))
    });
    if let Some(name) = converted {
        return Ok(Some(name.into_bytes()));
    }

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
/// Uses `getgrnam_r` for thread-safe lookup. When a name converter is
/// installed via [`super::set_name_converter`], delegates to it instead.
pub fn lookup_group_by_name(name: &[u8]) -> Result<Option<RawGid>, io::Error> {
    // upstream: uidlist.c:175-181 - name_converter replaces getgrnam
    if let Ok(name_str) = std::str::from_utf8(name) {
        let converted = NAME_CONVERTER_SLOT.with(|slot| {
            slot.borrow_mut()
                .as_mut()
                .and_then(|nc| nc.name_to_gid(name_str))
        });
        if let Some(gid) = converted {
            return Ok(Some(gid as RawGid));
        }
    }

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
