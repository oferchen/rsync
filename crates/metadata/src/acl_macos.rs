#![cfg(all(feature = "acl", target_os = "macos"))]
#![allow(unsafe_code)]

//! # macOS ACL Support
//!
//! This module implements ACL synchronization for macOS using the POSIX ACL API
//! available in libSystem. Unlike Linux, macOS:
//!
//! - Does not have `acl_from_mode()` - we use `acl_from_text()` instead
//! - Does not support default ACLs on directories
//! - Uses NFSv4-style ACLs internally but exposes POSIX ACL compatibility
//!
//! # Design
//!
//! The implementation mirrors the Linux `acl_support.rs` module but with
//! macOS-specific adaptations:
//!
//! - `reset_access_acl()` builds a POSIX ACL text string from permission bits
//!   and parses it with `acl_from_text()`
//! - Default ACL operations are no-ops since macOS doesn't support them
//!
//! # Upstream Reference
//!
//! - macOS `acl(3)` manual page
//! - POSIX.1e ACL specification

use std::ffi::CString;
use std::fs;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::ptr;

use crate::MetadataError;

mod sys {
    #![allow(unsafe_code)]
    #![allow(non_camel_case_types)]

    use libc::{c_char, c_int, c_void};

    pub type acl_t = *mut c_void;
    pub type acl_type_t = c_int;

    /// POSIX ACL type for file access permissions.
    pub const ACL_TYPE_EXTENDED: acl_type_t = 0x00000100;

    pub type acl_entry_t = *mut c_void;

    pub const ACL_FIRST_ENTRY: c_int = 0;

    unsafe extern "C" {
        pub fn acl_get_file(path_p: *const c_char, ty: acl_type_t) -> acl_t;
        pub fn acl_set_file(path_p: *const c_char, ty: acl_type_t, acl: acl_t) -> c_int;
        pub fn acl_dup(acl: acl_t) -> acl_t;
        pub fn acl_free(obj_p: *mut c_void) -> c_int;
        pub fn acl_from_text(buf_p: *const c_char) -> acl_t;
        pub fn acl_get_entry(acl: acl_t, entry_id: c_int, entry_p: *mut acl_entry_t) -> c_int;
        pub fn acl_delete_file(path_p: *const c_char, ty: acl_type_t) -> c_int;
    }
}

/// Synchronises the POSIX ACLs from `source` to `destination`.
///
/// On macOS, this copies the extended ACL if present. Unlike Linux, macOS
/// does not support default ACLs on directories, so those operations are
/// skipped.
///
/// Symbolic links do not support ACLs; when `follow_symlinks` is `false` the
/// helper returns without performing any work.
///
/// # Errors
///
/// Returns [`MetadataError`] when reading the source ACLs or applying them
/// to the destination fails. Filesystems that report ACLs as unsupported
/// are treated as lacking ACLs and do not trigger an error.
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

/// Wrapper around raw macOS ACL pointer with automatic cleanup.
struct MacOsAcl(sys::acl_t);

impl MacOsAcl {
    const fn as_ptr(&self) -> sys::acl_t {
        self.0
    }

    const fn from_raw(raw: sys::acl_t) -> Self {
        Self(raw)
    }

    fn clone(&self) -> io::Result<Self> {
        // Safety: `acl_dup` returns a new reference when provided with a valid ACL pointer.
        let duplicated = unsafe { sys::acl_dup(self.0) };
        if duplicated.is_null() {
            Err(io::Error::last_os_error())
        } else {
            Ok(Self::from_raw(duplicated))
        }
    }

    fn is_empty(&self) -> io::Result<bool> {
        let mut entry: sys::acl_entry_t = ptr::null_mut();
        // Safety: the ACL pointer remains valid for the duration of the call.
        let result = unsafe { sys::acl_get_entry(self.0, sys::ACL_FIRST_ENTRY, &mut entry) };
        match result {
            0 => Ok(true),
            -1 => {
                let error = io::Error::last_os_error();
                // EINVAL means no entries (empty ACL on macOS)
                if error.raw_os_error() == Some(libc::EINVAL) {
                    Ok(true)
                } else {
                    Err(error)
                }
            }
            _ => Ok(false), // Has entries
        }
    }
}

impl Drop for MacOsAcl {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // Safety: the ACL pointer originates from libSystem allocation APIs.
            unsafe {
                sys::acl_free(self.0);
            }
        }
    }
}

/// Fetches the extended ACL from a file.
fn fetch_acl(path: &Path) -> io::Result<Option<MacOsAcl>> {
    let c_path = CString::new(path.as_os_str().as_bytes())?;
    // Safety: the pointer remains valid for the duration of the call.
    let acl = unsafe { sys::acl_get_file(c_path.as_ptr(), sys::ACL_TYPE_EXTENDED) };
    if acl.is_null() {
        let error = io::Error::last_os_error();
        match error.raw_os_error() {
            // ENOENT: file doesn't exist (handled elsewhere)
            // ENOTSUP: filesystem doesn't support ACLs
            // EINVAL: invalid ACL type (shouldn't happen)
            Some(libc::ENOTSUP) | Some(libc::ENOENT) | Some(libc::EINVAL) => Ok(None),
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

/// Applies an access ACL to the destination, or resets to basic permissions.
fn apply_access_acl(path: &Path, acl: Option<&MacOsAcl>) -> io::Result<()> {
    match acl {
        Some(value) => set_acl(path, value.clone()?),
        None => reset_access_acl(path),
    }
}

/// Sets the extended ACL on a file.
fn set_acl(path: &Path, acl: MacOsAcl) -> io::Result<()> {
    let c_path = CString::new(path.as_os_str().as_bytes())?;
    // Safety: arguments are valid pointers and libSystem owns the ACL data.
    let result = unsafe { sys::acl_set_file(c_path.as_ptr(), sys::ACL_TYPE_EXTENDED, acl.as_ptr()) };
    if result == 0 {
        Ok(())
    } else {
        let error = io::Error::last_os_error();
        match error.raw_os_error() {
            // ENOTSUP: filesystem doesn't support ACLs - that's OK
            Some(libc::ENOTSUP) => Ok(()),
            _ => Err(error),
        }
    }
}

/// Resets the file's ACL to match its permission bits.
///
/// On macOS, we use `acl_from_text()` to create a minimal POSIX ACL from
/// the file's permission bits. This removes any extended ACL entries.
fn reset_access_acl(path: &Path) -> io::Result<()> {
    // First, try to delete any existing extended ACL
    let c_path = CString::new(path.as_os_str().as_bytes())?;
    let result = unsafe { sys::acl_delete_file(c_path.as_ptr(), sys::ACL_TYPE_EXTENDED) };

    if result == 0 {
        return Ok(());
    }

    let error = io::Error::last_os_error();
    match error.raw_os_error() {
        // No ACL to delete, or filesystem doesn't support ACLs - both are OK
        Some(libc::ENOENT) | Some(libc::ENOTSUP) | Some(libc::EINVAL) => Ok(()),
        _ => Err(error),
    }
}

/// Converts a mode_t permission value to POSIX ACL text format.
///
/// This is used as an alternative to `acl_from_mode()` which isn't available
/// on macOS.
#[allow(dead_code)]
fn mode_to_acl_text(mode: u32) -> String {
    let owner = (mode >> 6) & 0o7;
    let group = (mode >> 3) & 0o7;
    let other = mode & 0o7;

    fn perm_string(bits: u32) -> String {
        format!(
            "{}{}{}",
            if bits & 4 != 0 { 'r' } else { '-' },
            if bits & 2 != 0 { 'w' } else { '-' },
            if bits & 1 != 0 { 'x' } else { '-' }
        )
    }

    format!(
        "user::{}\ngroup::{}\nother::{}",
        perm_string(owner),
        perm_string(group),
        perm_string(other)
    )
}

/// Creates an ACL from permission bits using text parsing.
#[allow(dead_code)]
fn acl_from_mode(mode: u32) -> io::Result<MacOsAcl> {
    let text = mode_to_acl_text(mode);
    let c_text = CString::new(text)?;
    // Safety: acl_from_text parses the provided string into a new ACL.
    let acl = unsafe { sys::acl_from_text(c_text.as_ptr()) };
    if acl.is_null() {
        Err(io::Error::last_os_error())
    } else {
        Ok(MacOsAcl::from_raw(acl))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use tempfile::tempdir;

    #[test]
    fn mode_to_acl_text_formats_correctly() {
        // rwxr-xr-x = 755
        let text = mode_to_acl_text(0o755);
        assert!(text.contains("user::rwx"));
        assert!(text.contains("group::r-x"));
        assert!(text.contains("other::r-x"));

        // rw-r--r-- = 644
        let text = mode_to_acl_text(0o644);
        assert!(text.contains("user::rw-"));
        assert!(text.contains("group::r--"));
        assert!(text.contains("other::r--"));
    }

    #[test]
    fn sync_acls_handles_missing_source() {
        let dir = tempdir().expect("tempdir");
        let source = dir.path().join("nonexistent");
        let destination = dir.path().join("dst");
        File::create(&destination).expect("create dst");

        // Should return an error for missing source
        let result = sync_acls(&source, &destination, true);
        assert!(result.is_err());
    }

    #[test]
    fn sync_acls_skips_symlinks() {
        let dir = tempdir().expect("tempdir");
        let source = dir.path().join("src");
        let destination = dir.path().join("dst");
        File::create(&source).expect("create src");
        File::create(&destination).expect("create dst");

        // Should succeed but do nothing for follow_symlinks=false
        sync_acls(&source, &destination, false).expect("sync with follow_symlinks=false");
    }

    #[test]
    fn sync_acls_between_regular_files() {
        let dir = tempdir().expect("tempdir");
        let source = dir.path().join("src");
        let destination = dir.path().join("dst");
        File::create(&source).expect("create src");
        File::create(&destination).expect("create dst");

        // Basic sync should succeed (may or may not have extended ACLs)
        sync_acls(&source, &destination, true).expect("sync acls");
    }

    #[test]
    fn reset_access_acl_handles_missing_acl() {
        let dir = tempdir().expect("tempdir");
        let file = dir.path().join("test");
        File::create(&file).expect("create file");

        // Should succeed even if no ACL exists
        reset_access_acl(&file).expect("reset acl");
    }
}
