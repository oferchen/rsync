use super::*;
use crate::signature::SignatureAlgorithm;
use ::metadata::ChmodModifiers;
use bandwidth::BandwidthLimiter;
use compress::algorithm::CompressionAlgorithm;
use compress::zlib::CompressionLevel;
use filetime::{FileTime, set_file_mtime, set_file_times};
use filters::{FilterRule, FilterSet};
use std::ffi::{OsStr, OsString};
use std::fs;
#[cfg(unix)]
use std::io::Read;
use std::io::{self, Write};
#[cfg(unix)]
use std::io::{Seek, SeekFrom};
use std::num::{NonZeroU8, NonZeroU64};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;
use tempfile::tempdir;

#[cfg(all(unix, feature = "xattr"))]
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

    let bits: libc::mode_t = (mode & 0o177_777)
        .try_into()
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "mode out of range"))?;
    apple_fs::mkfifo(path, bits)
}

#[cfg(all(unix, feature = "acl", not(target_vendor = "apple")))]
use std::os::unix::ffi::OsStrExt;

#[cfg(all(unix, feature = "acl"))]
mod acl_sys {
    #![allow(unsafe_code, dead_code)]

    use libc::{c_char, c_int, c_void, ssize_t};

    pub type AclHandle = *mut c_void;
    pub type AclType = c_int;
    pub const ACL_TYPE_ACCESS: AclType = 0x8000;

    #[cfg_attr(not(target_vendor = "apple"), link(name = "acl"))]
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
trait AclStrategy {
    /// Return a textual representation of the ACL for `path`, or `None` if
    /// no ACL is present or ACLs are effectively unsupported.
    fn get_text(&self, path: &Path, ty: acl_sys::AclType) -> Option<String>;

    /// Apply an ACL described by `text` to `path`. Implementations may be
    /// no-ops on platforms where ACLs are stubbed.
    fn set_from_text(&self, path: &Path, text: &str, ty: acl_sys::AclType);
}

#[cfg(all(unix, feature = "acl", not(target_vendor = "apple")))]
struct LibAclStrategy;

#[cfg(all(unix, feature = "acl", not(target_vendor = "apple")))]
impl AclStrategy for LibAclStrategy {
    fn get_text(&self, path: &Path, ty: acl_sys::AclType) -> Option<String> {
        use std::ffi::CString;
        use std::slice;

        let c_path = CString::new(path.as_os_str().as_bytes()).expect("cstring path");
        // Safety: `c_path` is a valid, NUL-terminated C string.
        let acl = unsafe { acl_sys::acl_get_file(c_path.as_ptr(), ty) };
        if acl.is_null() {
            return None;
        }

        let mut len = 0;
        // Safety: `acl` comes from `acl_get_file` and remains valid.
        let text_ptr = unsafe { acl_sys::acl_to_text(acl, &mut len) };
        if text_ptr.is_null() {
            unsafe { acl_sys::free(acl) };
            return None;
        }

        // Safety: `text_ptr` and `len` are provided by libacl.
        let slice = unsafe { slice::from_raw_parts(text_ptr.cast::<u8>(), len as usize) };
        let text = String::from_utf8_lossy(slice).trim().to_owned();

        unsafe {
            acl_sys::free_text(text_ptr);
            acl_sys::free(acl);
        }

        if text.is_empty() { None } else { Some(text) }
    }

    fn set_from_text(&self, path: &Path, text: &str, ty: acl_sys::AclType) {
        use std::ffi::CString;

        let c_path = CString::new(path.as_os_str().as_bytes()).expect("cstring path");
        let c_text = CString::new(text).expect("cstring text");

        // Safety: both CStrings are valid, NUL-terminated byte strings.
        let acl = unsafe { acl_sys::acl_from_text(c_text.as_ptr()) };
        // On non-Apple Unix we require the text representation to be valid.
        assert!(!acl.is_null(), "acl_from_text");

        // Safety: `acl` comes from libacl and remains valid during the call.
        let result = unsafe { acl_sys::acl_set_file(c_path.as_ptr(), ty, acl) };
        unsafe {
            acl_sys::free(acl);
        }
        assert_eq!(result, 0, "acl_set_file");
    }
}

#[cfg(all(unix, feature = "acl", target_vendor = "apple"))]
struct NoOpAclStrategy;

#[cfg(all(unix, feature = "acl", target_vendor = "apple"))]
impl AclStrategy for NoOpAclStrategy {
    fn get_text(&self, _path: &Path, _ty: acl_sys::AclType) -> Option<String> {
        // Apple platforms follow the metadata crate's ACL stub: ACL
        // support is effectively unavailable, but the pipeline must not
        // crash or fail when ACLs are requested.
        None
    }

    fn set_from_text(&self, _path: &Path, _text: &str, _ty: acl_sys::AclType) {
        // No-op stub: behave as if ACLs are simply not preserved.
    }
}

#[cfg(all(unix, feature = "acl", not(target_vendor = "apple")))]
static ACTIVE_ACL_STRATEGY: LibAclStrategy = LibAclStrategy;

#[cfg(all(unix, feature = "acl", target_vendor = "apple"))]
static ACTIVE_ACL_STRATEGY: NoOpAclStrategy = NoOpAclStrategy;

#[cfg(all(unix, feature = "acl"))]
fn active_acl_strategy() -> &'static dyn AclStrategy {
    &ACTIVE_ACL_STRATEGY
}

#[cfg(all(unix, feature = "acl"))]
fn acl_to_text(path: &Path, ty: acl_sys::AclType) -> Option<String> {
    active_acl_strategy().get_text(path, ty)
}

#[cfg(all(unix, feature = "acl"))]
fn set_acl_from_text(path: &Path, text: &str, ty: acl_sys::AclType) {
    active_acl_strategy().set_from_text(path, text, ty)
}

mod test_helpers {
    use std::path::{Path, PathBuf};
    use tempfile::TempDir;

    /// Test context for copy operations.
    pub struct CopyTestContext {
        /// Temporary directory that must be kept alive for paths to remain valid.
        #[allow(dead_code)]
        pub temp_dir: TempDir,
        pub source: PathBuf,
        pub dest: PathBuf,
    }

    /// Creates a test context with temporary directory and source/dest paths.
    ///
    /// The source directory is created automatically. The dest directory is not
    /// created to allow tests to control whether it exists.
    pub fn setup_copy_test() -> CopyTestContext {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let source = temp_dir.path().join("source");
        let dest = temp_dir.path().join("dest");
        std::fs::create_dir_all(&source).expect("create source");
        CopyTestContext {
            temp_dir,
            source,
            dest,
        }
    }

    /// Creates a directory tree for testing.
    ///
    /// Takes a base path and a specification of paths to create. Each entry is a
    /// tuple of (path, optional_content):
    /// - If `content` is `Some(data)`, a file is created with that content
    /// - If `content` is `None`, an empty directory is created
    ///
    /// Parent directories are created automatically as needed.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// let temp = tempdir().expect("tempdir");
    /// create_test_tree(temp.path(), &[
    ///     ("dir1/file1.txt", Some(b"content1")),
    ///     ("dir1/file2.txt", Some(b"content2")),
    ///     ("dir2/subdir", None),  // empty directory
    ///     ("dir3/nested/file.txt", Some(b"nested")),
    /// ]);
    /// ```
    #[allow(dead_code)]
    pub fn create_test_tree(base: &Path, spec: &[(&str, Option<&[u8]>)]) {
        for (path, content) in spec {
            let full_path = base.join(path);
            if let Some(data) = content {
                if let Some(parent) = full_path.parent() {
                    std::fs::create_dir_all(parent).expect("create parent directories");
                }
                std::fs::write(&full_path, data).expect("write file");
            } else {
                std::fs::create_dir_all(&full_path).expect("create directory");
            }
        }
    }
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
include!("execute_append.rs");
include!("execute_basic.rs");
include!("execute_skip.rs");
include!("execute_ignore_existing.rs");
include!("execute_existing.rs");
include!("execute_delta.rs");
include!("execute_whole_file.rs");
include!("execute_symlinks.rs");
include!("execute_symlink_edge_cases.rs");
include!("execute_copy_links.rs");
include!("execute_dirlinks.rs");
include!("execute_metadata.rs");
include!("execute_executability.rs");
include!("execute_special.rs");
include!("execute_directories.rs");
include!("execute_prune_empty_dirs.rs");
include!("execute_hardlinks.rs");
include!("execute_link_dest.rs");
include!("execute_copy_dest.rs");
include!("execute_sparse.rs");
include!("execute_min_size.rs");
include!("max_size_filter.rs");
include!("bandwidth.rs");
include!("filters_runtime.rs");
include!("delete.rs");
include!("backups.rs");
include!("delete_protect.rs");
include!("dest_guard.rs");
include!("executor_file_comparison.rs");
include!("executor_file_operations.rs");
include!("execute_modify_window.rs");
include!("execute_one_file_system.rs");
include!("execute_omit_dir_times.rs");

#[cfg(unix)]
include!("execute_fsync.rs");
#[cfg(unix)]
include!("execute_numeric_ids.rs");

// Additional test modules
include!("delete_delay.rs");
#[cfg(unix)]
include!("execute_chown.rs");
include!("execute_compare_dest.rs");
include!("execute_delay_updates.rs");
include!("execute_ignore_times.rs");
include!("execute_inplace.rs");
include!("execute_no_implied_dirs.rs");
include!("execute_permissions.rs");
include!("execute_remove_source.rs");
include!("execute_temp_dir.rs");
include!("execute_update.rs");
#[cfg(unix)]
include!("execute_usermap_groupmap.rs");
include!("execute_zero_length.rs");
include!("execute_max_delete.rs");
include!("itemize_changes.rs");
include!("list_only.rs");
include!("partial_dir.rs");
include!("checksum_algorithm_behavior.rs");
include!("execute_timestamp_preservation.rs");
include!("disk_full_errors.rs");
#[cfg(unix)]
include!("permission_errors.rs");
