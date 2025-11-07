#![cfg(unix)]
#![allow(unsafe_code)]

//! # Overview
//!
//! POSIX Access Control Lists (ACLs) extend the traditional owner/group/other
//! permission bits so rsync can preserve fine-grained access rules. This module
//! exposes safe wrappers over the `libacl` API that mirror upstream rsync's ACL
//! replication semantics when the `acl` feature is enabled.
//!
//! # Design
//!
//! The [`sync_acls`] helper coordinates the full ACL replication workflow:
//!
//! - Read the source access and default ACLs without following symbolic links
//!   unless explicitly requested.
//! - Apply the retrieved ACLs to the destination, clearing extended entries when
//!   the source omits them.
//! - Recreate the standard permission bits using `acl_from_mode` when no access
//!   ACL exists, matching upstream rsync's behaviour.
//!
//! Internally the module wraps raw `libacl` pointers with [`PosixAcl`] to ensure
//! proper ownership and deallocation, providing a minimal clone facility for
//! repeated applications.
//!
//! # Invariants
//!
//! - Symbolic links only receive ACL updates when callers explicitly request
//!   following the link target; Linux does not expose link-local ACLs.
//! - Errors from unsupported filesystems (`ENOTSUP`, `ENODATA`) are treated as
//!   absent ACLs to mirror upstream rsync's best-effort behaviour.
//! - All raw pointers obtained from `libacl` are freed through the library's
//!   allocators to avoid leaks.
//!
//! # Errors
//!
//! Operations return [`MetadataError`] describing whether the ACL read or write
//! failed together with the path involved. Unsupported filesystem responses are
//! silently ignored so higher layers can proceed without failing the transfer.
//!
//! # Examples
//!
//! ```rust,ignore
//! # #[cfg(feature = "acl")]
//! # {
//! use oc_rsync_meta::sync_acls;
//! use std::path::Path;
//!
//! # fn demo() -> Result<(), oc_rsync_meta::MetadataError> {
//! let source = Path::new("src");
//! let destination = Path::new("dst");
//! sync_acls(source, destination, true)?;
//! # Ok(())
//! # }
//! # let _ = demo();
//! # }
//! ```

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

    use std::ffi::CString;
    use std::io;
    use std::mem;
    use std::sync::OnceLock;

    #[cfg(test)]
    use libc::ssize_t;
    use libc::{c_char, c_int, c_void, mode_t};

    pub type acl_t = *mut c_void;
    pub type acl_type_t = c_int;

    pub const ACL_TYPE_ACCESS: acl_type_t = 0x8000;
    pub const ACL_TYPE_DEFAULT: acl_type_t = 0x4000;

    pub type acl_entry_t = *mut c_void;

    pub const ACL_FIRST_ENTRY: c_int = 0;

    struct LibAcl {
        _handle: *mut c_void,
        acl_get_file: unsafe extern "C" fn(*const c_char, acl_type_t) -> acl_t,
        acl_set_file: unsafe extern "C" fn(*const c_char, acl_type_t, acl_t) -> c_int,
        acl_dup: unsafe extern "C" fn(acl_t) -> acl_t,
        acl_free: unsafe extern "C" fn(*mut c_void) -> c_int,
        acl_from_mode: unsafe extern "C" fn(mode_t) -> acl_t,
        acl_delete_def_file: unsafe extern "C" fn(*const c_char) -> c_int,
        acl_get_entry: unsafe extern "C" fn(acl_t, c_int, *mut acl_entry_t) -> c_int,
        #[cfg(test)]
        acl_to_text: unsafe extern "C" fn(acl_t, *mut ssize_t) -> *mut c_char,
        #[cfg(test)]
        acl_from_text: unsafe extern "C" fn(*const c_char) -> acl_t,
    }

    unsafe impl Sync for LibAcl {}

    static LIBACL: OnceLock<LibAcl> = OnceLock::new();

    fn not_supported_error() -> io::Error {
        #[allow(clippy::unnecessary_cast)]
        let code = libc::ENOTSUP as i32;
        // Ensure `io::Error::last_os_error()` observes the same errno.
        unsafe {
            libc::set_errno(libc::Errno(code));
        }
        io::Error::from_raw_os_error(code)
    }

    fn library() -> io::Result<&'static LibAcl> {
        LIBACL.get_or_try_init(load_libacl)
    }

    fn load_libacl() -> io::Result<LibAcl> {
        let mut last_error = not_supported_error();
        for name in ["libacl.so.1", "libacl.so"] {
            match unsafe { open_library(name) } {
                Ok(lib) => return Ok(lib),
                Err(error) => last_error = error,
            }
        }
        Err(last_error)
    }

    unsafe fn open_library(name: &str) -> io::Result<LibAcl> {
        let c_name = CString::new(name).expect("library name");
        let handle = libc::dlopen(c_name.as_ptr(), libc::RTLD_NOW);
        if handle.is_null() {
            return Err(not_supported_error());
        }
        match LibAcl::from_handle(handle) {
            Ok(lib) => Ok(lib),
            Err(error) => {
                libc::dlclose(handle);
                Err(error)
            }
        }
    }

    impl LibAcl {
        unsafe fn from_handle(handle: *mut c_void) -> io::Result<Self> {
            Ok(Self {
                _handle: handle,
                acl_get_file: load_symbol(handle, "acl_get_file")?,
                acl_set_file: load_symbol(handle, "acl_set_file")?,
                acl_dup: load_symbol(handle, "acl_dup")?,
                acl_free: load_symbol(handle, "acl_free")?,
                acl_from_mode: load_symbol(handle, "acl_from_mode")?,
                acl_delete_def_file: load_symbol(handle, "acl_delete_def_file")?,
                acl_get_entry: load_symbol(handle, "acl_get_entry")?,
                #[cfg(test)]
                acl_to_text: load_symbol(handle, "acl_to_text")?,
                #[cfg(test)]
                acl_from_text: load_symbol(handle, "acl_from_text")?,
            })
        }
    }

    unsafe fn load_symbol<T>(handle: *mut c_void, name: &str) -> io::Result<T> {
        let symbol = CString::new(name).expect("symbol name");
        let ptr = libc::dlsym(handle, symbol.as_ptr());
        if ptr.is_null() {
            return Err(not_supported_error());
        }
        Ok(mem::transmute_copy(&ptr))
    }

    pub unsafe fn acl_get_file(path_p: *const c_char, ty: acl_type_t) -> io::Result<acl_t> {
        let lib = library()?;
        let acl = (lib.acl_get_file)(path_p, ty);
        if acl.is_null() {
            Err(io::Error::last_os_error())
        } else {
            Ok(acl)
        }
    }

    pub unsafe fn acl_set_file(
        path_p: *const c_char,
        ty: acl_type_t,
        acl: acl_t,
    ) -> io::Result<()> {
        let lib = library()?;
        let result = (lib.acl_set_file)(path_p, ty, acl);
        if result == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    pub unsafe fn acl_dup(acl: acl_t) -> io::Result<acl_t> {
        let lib = library()?;
        let duplicated = (lib.acl_dup)(acl);
        if duplicated.is_null() {
            Err(io::Error::last_os_error())
        } else {
            Ok(duplicated)
        }
    }

    pub unsafe fn acl_free(obj_p: *mut c_void) {
        if obj_p.is_null() {
            return;
        }
        if let Ok(lib) = library() {
            (lib.acl_free)(obj_p);
        }
    }

    pub unsafe fn acl_from_mode(mode: mode_t) -> io::Result<acl_t> {
        let lib = library()?;
        let acl = (lib.acl_from_mode)(mode);
        if acl.is_null() {
            Err(io::Error::last_os_error())
        } else {
            Ok(acl)
        }
    }

    pub unsafe fn acl_delete_def_file(path_p: *const c_char) -> io::Result<()> {
        let lib = library()?;
        let result = (lib.acl_delete_def_file)(path_p);
        if result == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    pub unsafe fn acl_get_entry(
        acl: acl_t,
        entry_id: c_int,
        entry_p: *mut acl_entry_t,
    ) -> io::Result<c_int> {
        let lib = library()?;
        let result = (lib.acl_get_entry)(acl, entry_id, entry_p);
        if result == -1 {
            Err(io::Error::last_os_error())
        } else {
            Ok(result)
        }
    }

    #[cfg(test)]
    pub unsafe fn acl_to_text(acl: acl_t, len_p: *mut ssize_t) -> io::Result<*mut c_char> {
        let lib = library()?;
        let text = (lib.acl_to_text)(acl, len_p);
        if text.is_null() {
            Err(io::Error::last_os_error())
        } else {
            Ok(text)
        }
    }

    #[cfg(test)]
    pub unsafe fn acl_from_text(buf_p: *const c_char) -> io::Result<acl_t> {
        let lib = library()?;
        let acl = (lib.acl_from_text)(buf_p);
        if acl.is_null() {
            Err(io::Error::last_os_error())
        } else {
            Ok(acl)
        }
    }
}

/// Synchronises the POSIX ACLs from `source` to `destination`.
///
/// The helper copies both the access ACL and, when present, the default ACL used by directories.
/// When the source omits ACL entries the destination's extended ACL is cleared and recreated from
/// the destination's permission bits so the access mask mirrors upstream rsync semantics.
///
/// Symbolic links do not support ACLs on Linux; when `follow_symlinks` is `false` the helper
/// returns without performing any work.
///
/// # Errors
///
/// Returns [`MetadataError`] when reading the source ACLs or applying them to the destination
/// fails. Filesystems that report ACLs as unsupported (`ENOTSUP`, `ENODATA`, `EINVAL`, `ENOENT`)
/// are treated as lacking ACLs and do not trigger an error.
///
/// # Examples
///
/// ```rust,ignore
/// # #[cfg(feature = "acl")]
/// # {
/// use oc_rsync_meta::sync_acls;
/// use std::path::Path;
///
/// # fn copy_acl() -> Result<(), oc_rsync_meta::MetadataError> {
/// let source = Path::new("src");
/// let destination = Path::new("dst");
/// sync_acls(source, destination, true)?;
/// # Ok(())
/// # }
/// # let _ = copy_acl();
/// # }
/// ```
#[allow(clippy::module_name_repetitions)]
pub fn sync_acls(
    source: &Path,
    destination: &Path,
    follow_symlinks: bool,
) -> Result<(), MetadataError> {
    if !follow_symlinks {
        return Ok(());
    }

    let access = match fetch_acl(source, sys::ACL_TYPE_ACCESS) {
        Ok(value) => value,
        Err(error) => return Err(MetadataError::new("read ACL", source, error)),
    };

    let metadata = match fs::symlink_metadata(source) {
        Ok(value) => value,
        Err(error) => return Err(MetadataError::new("stat", source, error)),
    };

    let default = if metadata.is_dir() {
        match fetch_acl(source, sys::ACL_TYPE_DEFAULT) {
            Ok(value) => value,
            Err(error) => return Err(MetadataError::new("read default ACL", source, error)),
        }
    } else {
        None
    };

    if let Err(error) = apply_access_acl(destination, access.as_ref()) {
        return Err(MetadataError::new("apply ACL", destination, error));
    }

    if let Err(error) = apply_default_acl(destination, default.as_ref()) {
        return Err(MetadataError::new("apply default ACL", destination, error));
    }

    Ok(())
}

struct PosixAcl(sys::acl_t);

impl PosixAcl {
    fn as_ptr(&self) -> sys::acl_t {
        self.0
    }

    fn from_raw(raw: sys::acl_t) -> Self {
        Self(raw)
    }

    fn clone(&self) -> io::Result<Self> {
        // Safety: `acl_dup` returns a new reference when provided with a valid ACL pointer.
        let duplicated = unsafe { sys::acl_dup(self.0)? };
        Ok(Self::from_raw(duplicated))
    }

    fn is_empty(&self) -> io::Result<bool> {
        let mut entry: sys::acl_entry_t = ptr::null_mut();
        // Safety: the ACL pointer remains valid for the duration of the call.
        let result = unsafe { sys::acl_get_entry(self.0, sys::ACL_FIRST_ENTRY, &mut entry)? };
        match result {
            0 => Ok(true),
            1 => Ok(false),
            value => Err(io::Error::other(format!(
                "unexpected acl_get_entry result {value}"
            ))),
        }
    }
}

impl Drop for PosixAcl {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // Safety: the ACL pointer originates from libacl allocation APIs.
            unsafe {
                sys::acl_free(self.0);
            }
        }
    }
}

fn fetch_acl(path: &Path, ty: sys::acl_type_t) -> io::Result<Option<PosixAcl>> {
    let c_path = CString::new(path.as_os_str().as_bytes())?;
    // Safety: the pointer remains valid for the duration of the call.
    let acl = match unsafe { sys::acl_get_file(c_path.as_ptr(), ty) } {
        Ok(value) => value,
        Err(error) => match error.raw_os_error() {
            Some(libc::ENOTSUP) | Some(libc::ENOENT) | Some(libc::EINVAL) | Some(libc::ENODATA) => {
                return Ok(None);
            }
            _ => return Err(error),
        },
    };

    let acl = PosixAcl::from_raw(acl);
    if acl.is_empty()? {
        Ok(None)
    } else {
        Ok(Some(acl))
    }
}

fn apply_access_acl(path: &Path, acl: Option<&PosixAcl>) -> io::Result<()> {
    match acl {
        Some(value) => set_acl(path, sys::ACL_TYPE_ACCESS, value.clone()?),
        None => reset_access_acl(path),
    }
}

fn set_acl(path: &Path, ty: sys::acl_type_t, acl: PosixAcl) -> io::Result<()> {
    let c_path = CString::new(path.as_os_str().as_bytes())?;
    // Safety: arguments are valid pointers and libacl owns the ACL data.
    unsafe { sys::acl_set_file(c_path.as_ptr(), ty, acl.as_ptr()) }
}

fn reset_access_acl(path: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    let mode = metadata.mode() & 0o777;
    // Safety: acl_from_mode allocates a new ACL from the provided bitmask.
    let acl = unsafe { sys::acl_from_mode(mode as libc::mode_t)? };

    let acl = PosixAcl::from_raw(acl);
    set_acl(path, sys::ACL_TYPE_ACCESS, acl)
}

fn apply_default_acl(path: &Path, acl: Option<&PosixAcl>) -> io::Result<()> {
    match acl {
        Some(value) => set_acl(path, sys::ACL_TYPE_DEFAULT, value.clone()?),
        None => clear_default_acl(path),
    }
}

fn clear_default_acl(path: &Path) -> io::Result<()> {
    let c_path = CString::new(path.as_os_str().as_bytes())?;
    // Safety: the call removes the default ACL when present.
    match unsafe { sys::acl_delete_def_file(c_path.as_ptr()) } {
        Ok(()) => Ok(()),
        Err(error) => match error.raw_os_error() {
            Some(libc::ENOENT) | Some(libc::ENOTSUP) => Ok(()),
            _ => Err(error),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::sys;
    use super::*;
    use std::os::unix::ffi::OsStrExt;
    use tempfile::tempdir;

    fn acl_to_text(path: &Path, ty: sys::acl_type_t) -> Option<String> {
        let c_path = CString::new(path.as_os_str().as_bytes()).expect("cstring");
        let acl = unsafe { sys::acl_get_file(c_path.as_ptr(), ty) }.ok()?;
        let mut len = 0;
        let text_ptr = unsafe { sys::acl_to_text(acl, &mut len) }.ok()?;
        let slice = unsafe { std::slice::from_raw_parts(text_ptr.cast::<u8>(), len as usize) };
        let text = String::from_utf8_lossy(slice).trim().to_string();
        unsafe {
            sys::acl_free(text_ptr.cast());
            sys::acl_free(acl);
        }
        if text.is_empty() { None } else { Some(text) }
    }

    fn set_acl_from_text(path: &Path, text: &str, ty: sys::acl_type_t) {
        let c_path = CString::new(path.as_os_str().as_bytes()).expect("cstring");
        let c_text = CString::new(text).expect("text");
        let acl = unsafe { sys::acl_from_text(c_text.as_ptr()) }.expect("acl_from_text");
        unsafe { sys::acl_set_file(c_path.as_ptr(), ty, acl) }.expect("acl_set_file");
        unsafe {
            sys::acl_free(acl);
        }
    }

    #[test]
    fn syncs_access_and_default_acls() {
        let dir = tempdir().expect("tempdir");
        let source = dir.path().join("src");
        let destination = dir.path().join("dst");
        fs::create_dir(&source).expect("create source dir");
        fs::create_dir(&destination).expect("create destination dir");

        set_acl_from_text(
            &source,
            "user::rwx\ngroup::r-x\nother::r-x\n",
            sys::ACL_TYPE_ACCESS,
        );
        set_acl_from_text(
            &source,
            "user::rwx\ngroup::r-x\nother::r-x\n",
            sys::ACL_TYPE_DEFAULT,
        );

        sync_acls(&source, &destination, true).expect("sync acls");

        let access = acl_to_text(&destination, sys::ACL_TYPE_ACCESS).expect("access acl");
        assert!(access.contains("user::rwx"));

        let default = acl_to_text(&destination, sys::ACL_TYPE_DEFAULT).expect("default acl");
        assert!(default.contains("default:user::rwx") || default.contains("user::rwx"));
    }

    #[test]
    fn clears_default_acl_when_source_missing() {
        let dir = tempdir().expect("tempdir");
        let source = dir.path().join("src");
        let destination = dir.path().join("dst");
        fs::create_dir(&source).expect("create source dir");
        fs::create_dir(&destination).expect("create destination dir");

        set_acl_from_text(
            &destination,
            "user::rwx\ngroup::r-x\nother::r-x\n",
            sys::ACL_TYPE_DEFAULT,
        );
        assert!(acl_to_text(&destination, sys::ACL_TYPE_DEFAULT).is_some());

        sync_acls(&source, &destination, true).expect("sync acls");

        assert!(acl_to_text(&destination, sys::ACL_TYPE_DEFAULT).is_none());
    }
}
