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
///
/// On macOS the read is routed through the macOS variant so resource
/// forks larger than the kernel's 64 MiB single-call ceiling are fetched in
/// full via positioned `getxattr(2)` calls, matching upstream rsync's
/// `sys_lgetxattr` (`lib/sysxattrs.c:60-80`). On other Unix platforms the
/// value fits in a single call, so the `xattr` crate is used directly.
#[cfg(not(target_os = "macos"))]
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

/// macOS variant of [`read_attribute`] that loops over `getxattr(2)` with a
/// rising `position` argument.
///
/// The macOS kernel returns at most 64 MiB from a single `getxattr(2)` call
/// for the resource fork attribute (`com.apple.ResourceFork`). Upstream rsync
/// works around this by re-issuing the call with an increasing byte offset
/// until the whole attribute has been read (`lib/sysxattrs.c:60-80`,
/// `GETXATTR_FETCH_LIMIT = 64*1024*1024`). The `xattr` crate issues a single
/// positionless call, so it silently truncates resource forks past 64 MiB;
/// this helper restores full fidelity.
///
/// For ordinary xattrs (well under 64 MiB) the first call reads the whole
/// value and the loop exits after one iteration, so behaviour is identical to
/// the positionless path.
#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
pub fn read_attribute(
    path: &Path,
    name: &[u8],
    follow_symlinks: bool,
) -> io::Result<Option<Vec<u8>>> {
    use std::ffi::CString;

    let options: libc::c_int = if follow_symlinks {
        0
    } else {
        libc::XATTR_NOFOLLOW
    };

    let c_path = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL byte"))?;
    let c_name = CString::new(name)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "xattr name contains NUL byte"))?;

    // Probe the full attribute size (a size==0 call returns the total length,
    // uncapped by the 64 MiB fetch limit). ENOATTR means the attribute is
    // absent, mirroring `xattr::get` returning `Ok(None)`.
    let total = unsafe {
        libc::getxattr(
            c_path.as_ptr(),
            c_name.as_ptr(),
            std::ptr::null_mut(),
            0,
            0,
            options,
        )
    };
    if total < 0 {
        let err = io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::ENOATTR) {
            return Ok(None);
        }
        return Err(err);
    }

    let total = total as usize;
    let mut buf = vec![0u8; total];
    let mut offset: usize = 0;
    // Read in chunks with a rising `position`. The kernel caps each resource
    // fork read at 64 MiB, so multiple iterations are needed for large forks;
    // ordinary attributes complete in the first iteration.
    while offset < total {
        let got = unsafe {
            libc::getxattr(
                c_path.as_ptr(),
                c_name.as_ptr(),
                buf[offset..].as_mut_ptr().cast::<libc::c_void>(),
                total - offset,
                offset as u32,
                options,
            )
        };
        if got < 0 {
            let err = io::Error::last_os_error();
            if err.raw_os_error() == Some(libc::ENOATTR) {
                return Ok(None);
            }
            return Err(err);
        }
        if got == 0 {
            // Attribute shrank since the size probe; stop at what we have.
            break;
        }
        offset += got as usize;
    }
    buf.truncate(offset);
    Ok(Some(buf))
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

#[cfg(all(test, target_os = "macos"))]
mod macos_tests {
    use super::*;
    use tempfile::tempdir;

    /// The macOS single-call `getxattr(2)` ceiling that the positioned read
    /// loop works around (upstream `GETXATTR_FETCH_LIMIT`).
    const FETCH_LIMIT: usize = 64 * 1024 * 1024;

    fn patterned(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i % 251) as u8).collect()
    }

    #[test]
    fn read_attribute_reads_small_value_and_missing() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("small");
        std::fs::write(&path, b"data").unwrap();

        let name = b"com.example.small";
        let value = patterned(4096);
        xattr::set(&path, OsStr::from_bytes(name), &value).unwrap();

        let got = read_attribute(&path, name, false).unwrap();
        assert_eq!(got.as_deref(), Some(value.as_slice()));

        // A missing attribute reports None, matching `xattr::get`.
        let missing = read_attribute(&path, b"com.example.absent", false).unwrap();
        assert_eq!(missing, None);
    }

    #[test]
    fn read_attribute_reads_resource_fork_past_fetch_limit() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("bigfork");
        std::fs::write(&path, b"payload").unwrap();

        // Just over the 64 MiB single-call ceiling so the read must loop with
        // a rising `position` to recover the full value. Writing is a single
        // positionless `setxattr`, matching upstream `sys_lsetxattr`.
        let value = patterned(FETCH_LIMIT + 4096);
        let name = OsStr::from_bytes(b"com.apple.ResourceFork");
        if xattr::set(&path, name, &value).is_err() {
            // Filesystem rejected a large resource fork; nothing to verify.
            eprintln!("skipping: filesystem does not accept a >64 MiB resource fork");
            return;
        }

        let got = read_attribute(&path, b"com.apple.ResourceFork", false)
            .unwrap()
            .expect("resource fork present");
        assert_eq!(
            got.len(),
            value.len(),
            "positioned read must recover the full resource fork, not truncate at 64 MiB"
        );
        assert_eq!(got, value, "resource fork bytes must round-trip exactly");
    }
}
