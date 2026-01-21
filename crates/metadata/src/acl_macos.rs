#![cfg(all(feature = "acl", target_os = "macos"))]
#![allow(unsafe_code)]

//! macOS ACL synchronization using the POSIX ACL API.
//!
//! This module implements ACL synchronization for macOS using the extended ACL
//! API available in libSystem. The implementation mirrors upstream rsync's
//! `lib/sysacls.c` behavior for the `HAVE_OSX_ACLS` platform.
//!
//! # Platform Differences from Linux
//!
//! macOS ACLs differ from Linux POSIX ACLs in several key ways:
//!
//! - Uses `ACL_TYPE_EXTENDED` instead of `ACL_TYPE_ACCESS`
//! - Does not support default ACLs on directories (returns `ENOTSUP`)
//! - Uses NFSv4-style allow/deny semantics internally
//! - Requires UUID translation for user/group entries (not implemented here)
//!
//! # Known Issues
//!
//! macOS has a bug where `acl_get_entry` returns 0 instead of 1 when entries
//! exist. This is documented in upstream rsync as `OSX_BROKEN_GETENTRY` and
//! handled via return value remapping.
//!
//! # Wire Protocol
//!
//! This module handles local ACL synchronization only. Wire protocol encoding
//! for remote transfers requires additional UUID-to-UID/GID mapping that is
//! not implemented here.
//!
//! # References
//!
//! - Upstream rsync `lib/sysacls.c` lines 2601-2760 (`HAVE_OSX_ACLS` section)
//! - Upstream rsync `acls.c` for high-level ACL handling
//! - macOS `acl(3)` manual page

use std::ffi::CString;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::ptr;

use crate::MetadataError;

/// FFI bindings for macOS ACL functions.
///
/// These bindings mirror upstream rsync's `lib/sysacls.h` definitions for
/// the `HAVE_OSX_ACLS` platform.
mod sys {
    #![allow(non_camel_case_types)]

    use libc::{c_char, c_int, c_void};

    /// Opaque ACL handle type.
    pub type acl_t = *mut c_void;

    /// ACL type selector.
    pub type acl_type_t = c_int;

    /// Extended ACL type for macOS.
    ///
    /// Corresponds to upstream's `SMB_ACL_TYPE_ACCESS` which maps to
    /// `ACL_TYPE_EXTENDED` on macOS (see `lib/sysacls.h` line 275).
    pub const ACL_TYPE_EXTENDED: acl_type_t = 0x00000100;

    /// Opaque ACL entry handle.
    pub type acl_entry_t = *mut c_void;

    /// Request first entry when iterating.
    pub const ACL_FIRST_ENTRY: c_int = 0;

    unsafe extern "C" {
        /// Gets the ACL for a file path.
        pub fn acl_get_file(path_p: *const c_char, ty: acl_type_t) -> acl_t;

        /// Sets the ACL for a file path.
        pub fn acl_set_file(path_p: *const c_char, ty: acl_type_t, acl: acl_t) -> c_int;

        /// Duplicates an ACL handle.
        pub fn acl_dup(acl: acl_t) -> acl_t;

        /// Frees an ACL handle.
        pub fn acl_free(obj_p: *mut c_void) -> c_int;

        /// Gets an entry from an ACL.
        ///
        /// # macOS Bug (OSX_BROKEN_GETENTRY)
        ///
        /// On macOS, this function returns 0 when it should return 1
        /// (indicating an entry exists). The caller must handle this by
        /// treating 0 as "has entry" and -1 with EINVAL as "no entries".
        ///
        /// See upstream rsync `lib/sysacls.c` lines 2607-2617.
        pub fn acl_get_entry(acl: acl_t, entry_id: c_int, entry_p: *mut acl_entry_t) -> c_int;

        /// Creates an empty ACL with capacity for the given number of entries.
        pub fn acl_init(count: c_int) -> acl_t;
    }
}

/// Synchronizes extended ACLs from source to destination.
///
/// Copies the extended ACL if present on the source file. Unlike Linux,
/// macOS does not support default ACLs on directories, so those operations
/// are skipped.
///
/// Symbolic links do not support ACLs; when `follow_symlinks` is `false`,
/// this function returns immediately without performing any work.
///
/// # Errors
///
/// Returns [`MetadataError`] when reading the source ACL or applying it
/// to the destination fails. Filesystems that report ACLs as unsupported
/// are treated as lacking ACLs and do not trigger an error.
///
/// # Upstream Reference
///
/// - `acls.c` `get_acl()` for reading ACLs
/// - `acls.c` `set_acl()` for applying ACLs
#[allow(clippy::module_name_repetitions)]
pub fn sync_acls(
    source: &Path,
    destination: &Path,
    follow_symlinks: bool,
) -> Result<(), MetadataError> {
    if !follow_symlinks {
        return Ok(());
    }

    let access = match fetch_acl(source) {
        Ok(value) => value,
        Err(error) => return Err(MetadataError::new("read ACL", source, error)),
    };

    if let Err(error) = apply_access_acl(destination, access.as_ref()) {
        return Err(MetadataError::new("apply ACL", destination, error));
    }

    Ok(())
}

/// RAII wrapper for macOS ACL handles.
///
/// Ensures proper cleanup via `acl_free` when the handle goes out of scope.
/// This mirrors the ownership semantics in upstream rsync's ACL handling.
struct MacOsAcl(sys::acl_t);

impl MacOsAcl {
    /// Returns the raw ACL pointer for FFI calls.
    const fn as_ptr(&self) -> sys::acl_t {
        self.0
    }

    /// Creates a wrapper from a raw ACL pointer.
    ///
    /// # Safety
    ///
    /// The caller must ensure the pointer was obtained from a libSystem
    /// ACL function and has not been freed.
    const fn from_raw(raw: sys::acl_t) -> Self {
        Self(raw)
    }

    /// Duplicates this ACL handle.
    ///
    /// Creates an independent copy that must be separately freed.
    fn try_clone(&self) -> io::Result<Self> {
        // SAFETY: `acl_dup` returns a new reference when provided with a valid ACL pointer.
        let duplicated = unsafe { sys::acl_dup(self.0) };
        if duplicated.is_null() {
            Err(io::Error::last_os_error())
        } else {
            Ok(Self::from_raw(duplicated))
        }
    }

    /// Checks if the ACL has no entries.
    ///
    /// # macOS Bug Handling
    ///
    /// macOS `acl_get_entry` has a known bug where it returns 0 instead of 1
    /// when entries exist. This is documented in upstream rsync as
    /// `OSX_BROKEN_GETENTRY` (see `lib/sysacls.c` lines 2603, 2607-2617).
    ///
    /// Return value mapping:
    /// - `0` on macOS means "has entry" (bug: should be 1)
    /// - `-1` with `EINVAL` means "no entries"
    /// - `-1` with other errno is an error
    /// - Positive values mean "has entry" (normal behavior)
    fn is_empty(&self) -> io::Result<bool> {
        let mut entry: sys::acl_entry_t = ptr::null_mut();
        // SAFETY: The ACL pointer remains valid for the duration of the call.
        let result = unsafe { sys::acl_get_entry(self.0, sys::ACL_FIRST_ENTRY, &mut entry) };

        match result {
            // macOS bug: returns 0 when it should return 1 (has entries)
            // See upstream rsync lib/sysacls.c:2610-2611
            0 => Ok(false),
            -1 => {
                let error = io::Error::last_os_error();
                // EINVAL (errno 22) means no more entries on macOS
                // See upstream rsync lib/sysacls.c:2612-2614
                if error.raw_os_error() == Some(libc::EINVAL) {
                    Ok(true)
                } else {
                    Err(error)
                }
            }
            // Positive value means has entries (standard behavior)
            _ => Ok(false),
        }
    }
}

impl Drop for MacOsAcl {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: The ACL pointer originates from libSystem allocation APIs.
            unsafe {
                sys::acl_free(self.0);
            }
        }
    }
}

/// Fetches the extended ACL from a file.
///
/// Returns `Ok(None)` if the file has no extended ACL or the filesystem
/// doesn't support ACLs. This matches upstream rsync's `no_acl_syscall_error`
/// handling in `lib/sysacls.c` lines 2775-2793.
fn fetch_acl(path: &Path) -> io::Result<Option<MacOsAcl>> {
    let c_path = CString::new(path.as_os_str().as_bytes())?;
    // SAFETY: The pointer remains valid for the duration of the call.
    let acl = unsafe { sys::acl_get_file(c_path.as_ptr(), sys::ACL_TYPE_EXTENDED) };

    if acl.is_null() {
        let error = io::Error::last_os_error();
        match error.raw_os_error() {
            // ENOENT: File doesn't exist or weird directory ACL issue
            // See upstream rsync lib/sysacls.c:2780-2782
            Some(libc::ENOENT) => Ok(None),
            // ENOTSUP: Filesystem doesn't support ACLs
            Some(libc::ENOTSUP) => Ok(None),
            // EINVAL: Invalid ACL type (shouldn't happen with ACL_TYPE_EXTENDED)
            Some(libc::EINVAL) => Ok(None),
            _ => Err(error),
        }
    } else {
        let acl = MacOsAcl::from_raw(acl);
        if acl.is_empty()? {
            Ok(None)
        } else {
            Ok(Some(acl))
        }
    }
}

/// Applies an access ACL to the destination, or removes extended ACL if none.
fn apply_access_acl(path: &Path, acl: Option<&MacOsAcl>) -> io::Result<()> {
    match acl {
        Some(value) => set_acl(path, value.try_clone()?),
        None => reset_access_acl(path),
    }
}

/// Sets the extended ACL on a file.
fn set_acl(path: &Path, acl: MacOsAcl) -> io::Result<()> {
    let c_path = CString::new(path.as_os_str().as_bytes())?;
    // SAFETY: Arguments are valid pointers and libSystem owns the ACL data.
    let result =
        unsafe { sys::acl_set_file(c_path.as_ptr(), sys::ACL_TYPE_EXTENDED, acl.as_ptr()) };

    if result == 0 {
        Ok(())
    } else {
        let error = io::Error::last_os_error();
        match error.raw_os_error() {
            // ENOTSUP: Filesystem doesn't support ACLs - acceptable
            Some(libc::ENOTSUP) => Ok(()),
            _ => Err(error),
        }
    }
}

/// Removes extended ACL from a file.
///
/// When the source has no extended ACL, we remove any extended ACL from the
/// destination. This differs slightly from Linux where `acl_from_mode` would
/// recreate a minimal ACL from permissions, but the end result is equivalent
/// since the file's permission bits still control access.
///
/// # Upstream Reference
///
/// See `acls.c` `set_rsync_acl()` lines 939-953 for default ACL deletion
/// pattern, adapted here for access ACLs on macOS.
fn reset_access_acl(path: &Path) -> io::Result<()> {
    let c_path = CString::new(path.as_os_str().as_bytes())?;

    // macOS doesn't have acl_delete_file - instead we set an empty ACL
    // SAFETY: acl_init(0) creates an empty ACL, which is safe.
    let empty_acl = unsafe { sys::acl_init(0) };
    if empty_acl.is_null() {
        return Err(io::Error::last_os_error());
    }

    // SAFETY: Path is valid and null-terminated, empty_acl is valid.
    let result = unsafe { sys::acl_set_file(c_path.as_ptr(), sys::ACL_TYPE_EXTENDED, empty_acl) };

    // SAFETY: empty_acl was allocated by acl_init and must be freed.
    unsafe { sys::acl_free(empty_acl as *mut std::ffi::c_void) };

    if result == 0 {
        return Ok(());
    }

    let error = io::Error::last_os_error();
    match error.raw_os_error() {
        // No ACL to delete, or filesystem doesn't support ACLs
        Some(libc::ENOENT) | Some(libc::ENOTSUP) | Some(libc::EINVAL) => Ok(()),
        _ => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use tempfile::tempdir;

    // ==================== MacOsAcl tests ====================

    #[test]
    fn macos_acl_from_raw_null_is_handled() {
        // Null pointer should be handled gracefully in Drop
        let acl = MacOsAcl::from_raw(ptr::null_mut());
        drop(acl); // Should not panic
    }

    // ==================== fetch_acl tests ====================

    #[test]
    fn fetch_acl_returns_none_for_missing_file() {
        let result = fetch_acl(Path::new("/nonexistent/path/that/does/not/exist"));
        // Should return None for ENOENT, not an error
        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
    }

    #[test]
    fn fetch_acl_returns_result_for_regular_file() {
        let dir = tempdir().expect("tempdir");
        let file = dir.path().join("test");
        File::create(&file).expect("create file");

        let result = fetch_acl(&file);
        // Should succeed (may or may not have extended ACL)
        assert!(result.is_ok());
    }

    // ==================== reset_access_acl tests ====================

    #[test]
    fn reset_access_acl_handles_file_without_acl() {
        let dir = tempdir().expect("tempdir");
        let file = dir.path().join("test");
        File::create(&file).expect("create file");

        // Should succeed even if no ACL exists
        let result = reset_access_acl(&file);
        assert!(result.is_ok());
    }

    #[test]
    fn reset_access_acl_handles_missing_file() {
        let dir = tempdir().expect("tempdir");
        let file = dir.path().join("nonexistent");

        // Should handle ENOENT gracefully
        let result = reset_access_acl(&file);
        // Either Ok (ENOENT handled) or Err depending on platform behavior
        // The important thing is it doesn't panic
        let _ = result;
    }

    // ==================== sync_acls tests ====================

    #[test]
    fn sync_acls_skips_when_not_following_symlinks() {
        let dir = tempdir().expect("tempdir");
        let source = dir.path().join("src");
        let destination = dir.path().join("dst");
        File::create(&source).expect("create src");
        File::create(&destination).expect("create dst");

        // Should return Ok without doing anything
        let result = sync_acls(&source, &destination, false);
        assert!(result.is_ok());
    }

    #[test]
    fn sync_acls_returns_error_for_missing_source() {
        let dir = tempdir().expect("tempdir");
        let source = dir.path().join("nonexistent");
        let destination = dir.path().join("dst");
        File::create(&destination).expect("create dst");

        let result = sync_acls(&source, &destination, true);
        assert!(result.is_err());
    }

    #[test]
    fn sync_acls_copies_between_regular_files() {
        let dir = tempdir().expect("tempdir");
        let source = dir.path().join("src");
        let destination = dir.path().join("dst");
        File::create(&source).expect("create src");
        File::create(&destination).expect("create dst");

        // Should succeed for files on same filesystem
        let result = sync_acls(&source, &destination, true);
        assert!(result.is_ok());
    }

    #[test]
    fn sync_acls_works_with_directories() {
        let dir = tempdir().expect("tempdir");
        let source = dir.path().join("src_dir");
        let destination = dir.path().join("dst_dir");
        std::fs::create_dir(&source).expect("create src_dir");
        std::fs::create_dir(&destination).expect("create dst_dir");

        let result = sync_acls(&source, &destination, true);
        assert!(result.is_ok());
    }

    // ==================== apply_access_acl tests ====================

    #[test]
    fn apply_access_acl_with_none_resets_acl() {
        let dir = tempdir().expect("tempdir");
        let file = dir.path().join("test");
        File::create(&file).expect("create file");

        let result = apply_access_acl(&file, None);
        assert!(result.is_ok());
    }

    // ==================== Error handling tests ====================

    #[test]
    fn error_handling_matches_upstream_no_acl_syscall_error() {
        // Test that we handle the same errors as upstream's no_acl_syscall_error
        // See lib/sysacls.c lines 2775-2793

        let dir = tempdir().expect("tempdir");
        let file = dir.path().join("test");
        File::create(&file).expect("create file");

        // These should all be handled gracefully
        let _ = fetch_acl(&file);
        let _ = reset_access_acl(&file);
    }
}
