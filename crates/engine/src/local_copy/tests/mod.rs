use super::*;
use crate::signature::SignatureAlgorithm;
use filetime::{FileTime, set_file_mtime, set_file_times};
use rsync_bandwidth::BandwidthLimiter;
use rsync_compress::zlib::CompressionLevel;
use rsync_filters::{FilterRule, FilterSet};
use rsync_meta::{ChmodModifiers, MetadataOptions};
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{self, Seek, SeekFrom, Write};
use std::num::{NonZeroU8, NonZeroU64};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;
use tempfile::tempdir;

#[cfg(feature = "xattr")]
use xattr;

#[cfg(all(
    unix,
    not(any(
        target_os = "ios",
        target_os = "macos",
        target_os = "tvos",
        target_os = "watchos"
    ))
))]
fn mkfifo_for_tests(path: &Path, mode: u32) -> io::Result<()> {
    use rustix::fs::{CWD, FileType, Mode, makedev, mknodat};
    use std::convert::TryInto;

    let bits: u16 = (mode & 0o177_777)
        .try_into()
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "mode out of range"))?;
    let mode = Mode::from_bits_truncate(bits.into());
    mknodat(CWD, path, FileType::Fifo, mode, makedev(0, 0)).map_err(io::Error::from)
}

#[cfg(all(
    unix,
    any(
        target_os = "ios",
        target_os = "macos",
        target_os = "tvos",
        target_os = "watchos"
    )
))]
fn mkfifo_for_tests(path: &Path, mode: u32) -> io::Result<()> {
    use std::convert::TryInto;
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let bits: libc::mode_t = (mode & 0o177_777)
        .try_into()
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "mode out of range"))?;
    let path_c = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains interior NUL"))?;
    let result = unsafe { libc::mkfifo(path_c.as_ptr(), bits) };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(all(unix, feature = "acl"))]
use std::os::unix::ffi::OsStrExt;

#[cfg(all(unix, feature = "acl"))]
mod acl_sys {
    #![allow(unsafe_code)]

    use libc::{c_char, c_int, c_void, ssize_t};

    pub type AclHandle = *mut c_void;
    pub type AclType = c_int;

    pub const ACL_TYPE_ACCESS: AclType = 0x8000;

    #[link(name = "acl")]
    unsafe extern "C" {
        pub fn acl_get_file(path_p: *const c_char, ty: AclType) -> AclHandle;
        pub fn acl_set_file(path_p: *const c_char, ty: AclType, acl: AclHandle) -> c_int;
        pub fn acl_to_text(acl: AclHandle, len_p: *mut ssize_t) -> *mut c_char;
        pub fn acl_from_text(buf_p: *const c_char) -> AclHandle;
        pub fn acl_free(obj_p: *mut c_void) -> c_int;
    }

    pub unsafe fn free(handle: AclHandle) {
        // Safety: callers ensure the pointer originates from libacl.
        let _ = unsafe { acl_free(handle) };
    }

    pub unsafe fn free_text(handle: *mut c_char) {
        // Safety: callers ensure the pointer originates from libacl.
        let _ = unsafe { acl_free(handle.cast()) };
    }
}

#[cfg(all(unix, feature = "acl"))]
fn acl_to_text(path: &Path, ty: acl_sys::AclType) -> Option<String> {
    let c_path = std::ffi::CString::new(path.as_os_str().as_bytes()).expect("cstring");
    let acl = unsafe { acl_sys::acl_get_file(c_path.as_ptr(), ty) };
    if acl.is_null() {
        return None;
    }
    let mut len = 0;
    let text_ptr = unsafe { acl_sys::acl_to_text(acl, &mut len) };
    if text_ptr.is_null() {
        unsafe { acl_sys::free(acl) };
        return None;
    }
    let slice = unsafe { std::slice::from_raw_parts(text_ptr.cast::<u8>(), len as usize) };
    let text = String::from_utf8_lossy(slice).trim().to_string();
    unsafe {
        acl_sys::free_text(text_ptr);
        acl_sys::free(acl);
    }
    Some(text)
}

#[cfg(all(unix, feature = "acl"))]
fn set_acl_from_text(path: &Path, text: &str, ty: acl_sys::AclType) {
    let c_path = std::ffi::CString::new(path.as_os_str().as_bytes()).expect("cstring");
    let c_text = std::ffi::CString::new(text).expect("text");
    let acl = unsafe { acl_sys::acl_from_text(c_text.as_ptr()) };
    assert!(!acl.is_null(), "acl_from_text");
    let result = unsafe { acl_sys::acl_set_file(c_path.as_ptr(), ty, acl) };
    unsafe {
        acl_sys::free(acl);
    }
    assert_eq!(result, 0, "acl_set_file");
}

#[cfg(unix)]
mod unix_ids {
    #![allow(unsafe_code)]

    pub(super) fn uid(raw: u32) -> rustix::fs::Uid {
        // Safety: constructing `Uid` from a raw value is how rustix exposes platform IDs.
        unsafe { rustix::fs::Uid::from_raw(raw) }
    }

    pub(super) fn gid(raw: u32) -> rustix::fs::Gid {
        // Safety: constructing `Gid` from a raw value is how rustix exposes platform IDs.
        unsafe { rustix::fs::Gid::from_raw(raw) }
    }
}

include!("options.rs");
include!("filters.rs");
include!("relative.rs");
include!("plan.rs");
include!("execute_basic.rs");
include!("execute_skip.rs");
include!("execute_delta.rs");
include!("execute_symlinks.rs");
include!("execute_dirlinks.rs");
include!("execute_metadata.rs");
include!("execute_special.rs");
include!("execute_directories.rs");
include!("execute_hardlinks.rs");
include!("execute_sparse.rs");
include!("bandwidth.rs");
include!("filters_runtime.rs");
include!("delete.rs");
include!("backups.rs");
include!("delete_protect.rs");
include!("dest_guard.rs");
