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

pub(crate) fn map_uid(uid: RawUid, numeric_ids: bool) -> Option<Uid> {
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

pub(crate) fn map_gid(gid: RawGid, numeric_ids: bool) -> Option<Gid> {
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

pub(crate) fn lookup_user_name(uid: RawUid) -> Result<Option<Vec<u8>>, io::Error> {
    let mut buffer = vec![0_u8; 1024];
    loop {
        let mut pwd = MaybeUninit::<libc::passwd>::zeroed();
        let mut result: *mut libc::passwd = ptr::null_mut();
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

            let pwd = unsafe { pwd.assume_init() };
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

pub(crate) fn lookup_user_by_name(name: &[u8]) -> Result<Option<RawUid>, io::Error> {
    let c_name = match CString::new(name) {
        Ok(name) => name,
        Err(_) => return Ok(None),
    };

    let mut buffer = vec![0_u8; 1024];
    loop {
        let mut pwd = MaybeUninit::<libc::passwd>::zeroed();
        let mut result: *mut libc::passwd = ptr::null_mut();
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

pub(crate) fn lookup_group_name(gid: RawGid) -> Result<Option<Vec<u8>>, io::Error> {
    let mut buffer = vec![0_u8; 1024];
    loop {
        let mut grp = MaybeUninit::<libc::group>::zeroed();
        let mut result: *mut libc::group = ptr::null_mut();
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

            let grp = unsafe { grp.assume_init() };
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

pub(crate) fn lookup_group_by_name(name: &[u8]) -> Result<Option<RawGid>, io::Error> {
    let c_name = match CString::new(name) {
        Ok(name) => name,
        Err(_) => return Ok(None),
    };

    let mut buffer = vec![0_u8; 1024];
    loop {
        let mut grp = MaybeUninit::<libc::group>::zeroed();
        let mut result: *mut libc::group = ptr::null_mut();
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
