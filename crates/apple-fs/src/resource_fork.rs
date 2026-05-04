//! Safe accessors for the macOS native resource fork and Finder info.
//!
//! macOS exposes the resource fork and the 32-byte Finder info block as
//! ordinary extended attributes named `com.apple.ResourceFork` and
//! `com.apple.FinderInfo`. Upstream rsync 3.4.1 reads and writes them via
//! the standard `getxattr(2)` / `setxattr(2)` syscalls (see `xattrs.c`,
//! `rsync_xal_get` and `rsync_xal_set`); there is no dedicated resource-fork
//! pathway. This module mirrors that approach by delegating to the
//! third-party `xattr` crate, which is the same safe wrapper already used
//! by `crates/metadata`.
//!
//! On non-macOS targets every accessor is a no-op stub:
//! - readers return `Ok(None)` so callers can transparently skip Mac-specific
//!   data when the host has no concept of a resource fork,
//! - writers return `Ok(())` so a transfer that copies an AppleDouble sidecar
//!   onto a non-Mac filesystem does not abort.
//!
//! No `unsafe` is used in this module. Per workspace policy, the `apple-fs`
//! crate keeps `#![deny(unsafe_code)]`; macOS xattr syscalls run inside the
//! pre-vetted `xattr` crate.

use std::io;
use std::path::Path;

/// Extended-attribute name for the macOS resource fork.
pub const RESOURCE_FORK_XATTR: &str = "com.apple.ResourceFork";

/// Extended-attribute name for the 32-byte macOS Finder info block.
pub const FINDER_INFO_XATTR: &str = "com.apple.FinderInfo";

/// Canonical length, in bytes, of the Finder info payload.
///
/// macOS rejects writes whose length is not exactly 32 bytes.
pub const FINDER_INFO_LEN: usize = 32;

/// Reads the macOS resource fork from `path`.
///
/// Returns `Ok(None)` when the attribute is absent or when the host platform
/// has no resource-fork concept (every non-macOS target).
///
/// # Errors
///
/// On macOS, propagates any [`io::Error`] surfaced by `getxattr(2)` other
/// than `ENOATTR` (which is mapped to `Ok(None)`).
#[cfg(target_os = "macos")]
pub fn read_resource_fork(path: &Path) -> io::Result<Option<Vec<u8>>> {
    read_xattr(path, RESOURCE_FORK_XATTR)
}

/// Writes `data` as the macOS resource fork on `path`.
///
/// # Errors
///
/// On macOS, propagates any [`io::Error`] surfaced by `setxattr(2)`.
#[cfg(target_os = "macos")]
pub fn write_resource_fork(path: &Path, data: &[u8]) -> io::Result<()> {
    write_xattr(path, RESOURCE_FORK_XATTR, data)
}

/// Removes the macOS resource fork from `path`, if present.
///
/// # Errors
///
/// On macOS, propagates any [`io::Error`] surfaced by `removexattr(2)` other
/// than `ENOATTR` (which is treated as success).
#[cfg(target_os = "macos")]
pub fn remove_resource_fork(path: &Path) -> io::Result<()> {
    remove_xattr(path, RESOURCE_FORK_XATTR)
}

/// Reads the 32-byte Finder info block from `path`.
///
/// Returns `Ok(None)` when the attribute is absent. Returns
/// [`io::ErrorKind::InvalidData`] when the attribute exists but is not exactly
/// [`FINDER_INFO_LEN`] bytes long, since an arbitrarily sized Finder info is
/// always malformed.
///
/// # Errors
///
/// Surfaces I/O errors from `getxattr(2)` and an
/// [`io::ErrorKind::InvalidData`] error for malformed payloads.
#[cfg(target_os = "macos")]
pub fn read_finder_info(path: &Path) -> io::Result<Option<[u8; FINDER_INFO_LEN]>> {
    let Some(bytes) = read_xattr(path, FINDER_INFO_XATTR)? else {
        return Ok(None);
    };
    if bytes.len() != FINDER_INFO_LEN {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "{FINDER_INFO_XATTR} payload is {} bytes; expected {FINDER_INFO_LEN}",
                bytes.len()
            ),
        ));
    }
    let mut info = [0u8; FINDER_INFO_LEN];
    info.copy_from_slice(&bytes);
    Ok(Some(info))
}

/// Writes the 32-byte Finder info block to `path`.
///
/// # Errors
///
/// On macOS, propagates any [`io::Error`] surfaced by `setxattr(2)`.
#[cfg(target_os = "macos")]
pub fn write_finder_info(path: &Path, info: &[u8; FINDER_INFO_LEN]) -> io::Result<()> {
    write_xattr(path, FINDER_INFO_XATTR, info)
}

/// Removes the Finder info block from `path`, if present.
///
/// # Errors
///
/// On macOS, propagates any [`io::Error`] surfaced by `removexattr(2)` other
/// than `ENOATTR` (which is treated as success).
#[cfg(target_os = "macos")]
pub fn remove_finder_info(path: &Path) -> io::Result<()> {
    remove_xattr(path, FINDER_INFO_XATTR)
}

#[cfg(target_os = "macos")]
fn read_xattr(path: &Path, name: &str) -> io::Result<Option<Vec<u8>>> {
    // The xattr crate already maps ENOATTR (the "no such attribute" errno on
    // macOS) to Ok(None), so the missing-attribute case requires no special
    // handling here. See the xattr crate's `get` documentation.
    xattr::get(path, name)
}

#[cfg(target_os = "macos")]
fn write_xattr(path: &Path, name: &str, value: &[u8]) -> io::Result<()> {
    xattr::set(path, name, value)
}

#[cfg(target_os = "macos")]
fn remove_xattr(path: &Path, name: &str) -> io::Result<()> {
    // ENOATTR surfaces as io::ErrorKind::Other from xattr::remove on macOS.
    // Treat any "no such attribute" error as success so callers can use the
    // remove accessors as idempotent cleanup hooks.
    match xattr::remove(path, name) {
        Ok(()) => Ok(()),
        Err(error) if is_no_attr(&error) => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(target_os = "macos")]
fn is_no_attr(error: &io::Error) -> bool {
    // macOS exposes ENOATTR (errno 93) when an xattr name is absent. The
    // raw_os_error is the most reliable signal: the io::ErrorKind mapping
    // varies across stdlib versions but the underlying errno does not.
    // ENOATTR = 93 on Darwin (sys/xattr.h).
    const ENOATTR_DARWIN: i32 = 93;
    error.raw_os_error() == Some(ENOATTR_DARWIN)
}

// -- non-macOS stubs ---------------------------------------------------------

/// Stub: returns `Ok(None)`. The host has no resource-fork concept.
#[cfg(not(target_os = "macos"))]
pub fn read_resource_fork(_path: &Path) -> io::Result<Option<Vec<u8>>> {
    Ok(None)
}

/// Stub: returns `Ok(())`. The host has no resource-fork concept.
#[cfg(not(target_os = "macos"))]
pub fn write_resource_fork(_path: &Path, _data: &[u8]) -> io::Result<()> {
    Ok(())
}

/// Stub: returns `Ok(())`. The host has no resource-fork concept.
#[cfg(not(target_os = "macos"))]
pub fn remove_resource_fork(_path: &Path) -> io::Result<()> {
    Ok(())
}

/// Stub: returns `Ok(None)`. The host has no Finder-info concept.
#[cfg(not(target_os = "macos"))]
pub fn read_finder_info(_path: &Path) -> io::Result<Option<[u8; FINDER_INFO_LEN]>> {
    Ok(None)
}

/// Stub: returns `Ok(())`. The host has no Finder-info concept.
#[cfg(not(target_os = "macos"))]
pub fn write_finder_info(_path: &Path, _info: &[u8; FINDER_INFO_LEN]) -> io::Result<()> {
    Ok(())
}

/// Stub: returns `Ok(())`. The host has no Finder-info concept.
#[cfg(not(target_os = "macos"))]
pub fn remove_finder_info(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[cfg(not(target_os = "macos"))]
    fn dummy_path() -> PathBuf {
        PathBuf::from("/nonexistent/oc-rsync/apple-fs/stub")
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn non_macos_resource_fork_reader_returns_none() {
        let path = dummy_path();
        assert!(read_resource_fork(&path).expect("stub ok").is_none());
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn non_macos_resource_fork_writer_is_noop() {
        let path = dummy_path();
        write_resource_fork(&path, b"ignored").expect("stub ok");
        remove_resource_fork(&path).expect("stub ok");
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn non_macos_finder_info_accessors_are_noops() {
        let path = dummy_path();
        assert!(read_finder_info(&path).expect("stub ok").is_none());
        write_finder_info(&path, &[0u8; FINDER_INFO_LEN]).expect("stub ok");
        remove_finder_info(&path).expect("stub ok");
    }

    #[cfg(target_os = "macos")]
    fn make_temp_file() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("subject");
        std::fs::write(&path, b"data fork contents").expect("write");
        (dir, path)
    }

    #[cfg(target_os = "macos")]
    fn xattr_supported(path: &Path) -> bool {
        // Some macOS filesystems (FAT volumes mounted under /Volumes) silently
        // refuse xattr writes. Probe with a no-op write+remove and skip the
        // test when the filesystem rejects the operation.
        match xattr::set(path, "com.apple.oc-rsync.probe", b"") {
            Ok(()) => {
                let _ = xattr::remove(path, "com.apple.oc-rsync.probe");
                true
            }
            Err(error) => !matches!(
                error.kind(),
                io::ErrorKind::Unsupported | io::ErrorKind::PermissionDenied
            ),
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_resource_fork_round_trip() {
        let (_dir, path) = make_temp_file();
        if !xattr_supported(&path) {
            eprintln!("skipping: filesystem does not support xattrs");
            return;
        }
        assert!(read_resource_fork(&path).expect("read absent").is_none());
        let payload = b"resource-fork-payload".to_vec();
        write_resource_fork(&path, &payload).expect("write");
        let observed = read_resource_fork(&path).expect("read");
        assert_eq!(observed.as_deref(), Some(payload.as_slice()));
        remove_resource_fork(&path).expect("remove");
        assert!(
            read_resource_fork(&path)
                .expect("read after remove")
                .is_none()
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_finder_info_round_trip() {
        let (_dir, path) = make_temp_file();
        if !xattr_supported(&path) {
            eprintln!("skipping: filesystem does not support xattrs");
            return;
        }
        assert!(read_finder_info(&path).expect("read absent").is_none());
        let mut info = [0u8; FINDER_INFO_LEN];
        info[0..4].copy_from_slice(b"TEXT");
        info[4..8].copy_from_slice(b"ttxt");
        write_finder_info(&path, &info).expect("write");
        let observed = read_finder_info(&path).expect("read").expect("present");
        assert_eq!(observed, info);
        remove_finder_info(&path).expect("remove");
        assert!(
            read_finder_info(&path)
                .expect("read after remove")
                .is_none()
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_finder_info_rejects_wrong_length_payload() {
        let (_dir, path) = make_temp_file();
        if !xattr_supported(&path) {
            eprintln!("skipping: filesystem does not support xattrs");
            return;
        }
        // Inject a malformed FinderInfo (16 bytes instead of 32) directly via
        // the underlying xattr API to verify the read accessor flags it.
        xattr::set(&path, FINDER_INFO_XATTR, &[1u8; 16]).expect("write malformed");
        let err = read_finder_info(&path).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        let _ = xattr::remove(&path, FINDER_INFO_XATTR);
    }
}
