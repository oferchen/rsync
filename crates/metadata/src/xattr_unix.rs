//! Unix extended-attribute backend.
//!
//! Wraps the [`xattr`] crate so the cross-platform [`crate::xattr`] layer
//! can call into it without platform-specific imports. Behaviour mirrors
//! upstream rsync's POSIX xattr handling exactly: list/get/set/remove are
//! delegated to the kernel via the `xattr` crate, with optional
//! follow-symlinks variants for the `_deref` flavours.
//!
//! # Upstream Reference
//!
//! `xattrs.c:rsync_xal_get()` (`xattr.c:listxattr/getxattr`) and
//! `xattrs.c:rsync_xal_set()` (`xattr.c:setxattr/removexattr`).

use std::ffi::{OsStr, OsString};
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

/// Lists all xattr names attached to `path`, optionally following symlinks.
pub fn list_attributes(path: &Path, follow_symlinks: bool) -> io::Result<Vec<OsString>> {
    let attrs = if follow_symlinks {
        xattr::list_deref(path)?
    } else {
        xattr::list(path)?
    };
    Ok(attrs.collect())
}

/// Reads a single xattr value as raw bytes, or `Ok(None)` if missing.
pub fn read_attribute(
    path: &Path,
    name: &[u8],
    follow_symlinks: bool,
) -> io::Result<Option<Vec<u8>>> {
    let os_name = OsStr::from_bytes(name);
    if follow_symlinks {
        xattr::get_deref(path, os_name)
    } else {
        xattr::get(path, os_name)
    }
}

/// Writes a single xattr value, replacing any existing value.
pub fn write_attribute(
    path: &Path,
    name: &[u8],
    value: &[u8],
    follow_symlinks: bool,
) -> io::Result<()> {
    let os_name = OsStr::from_bytes(name);
    if follow_symlinks {
        xattr::set_deref(path, os_name, value)
    } else {
        xattr::set(path, os_name, value)
    }
}

/// Removes an xattr from `path`. Removing a missing xattr propagates the
/// underlying kernel error; the higher layer compensates by checking the
/// listing first.
pub fn remove_attribute(path: &Path, name: &[u8], follow_symlinks: bool) -> io::Result<()> {
    let os_name = OsStr::from_bytes(name);
    if follow_symlinks {
        xattr::remove_deref(path, os_name)
    } else {
        xattr::remove(path, os_name)
    }
}

/// Converts the [`OsString`] returned by [`list_attributes`] into the raw
/// bytes used by the cross-platform layer. On Unix the conversion is a
/// zero-copy view; on Windows the equivalent helper UTF-8-encodes the
/// underlying UTF-16 buffer.
pub fn os_name_to_bytes(name: &OsStr) -> Vec<u8> {
    name.as_bytes().to_vec()
}
