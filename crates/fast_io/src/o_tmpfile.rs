//! `O_TMPFILE` availability probe with `OnceLock` caching.
//!
//! Linux 3.11+ supports `O_TMPFILE` for creating unnamed temporary files directly
//! in a directory without a visible name in the filesystem. This avoids the
//! link-then-unlink dance and provides atomicity guarantees useful for safe file
//! replacement during rsync transfers.
//!
//! The probe opens a temporary file with `O_TMPFILE | O_WRONLY` on the given
//! directory and immediately closes the fd. The result is cached per-process
//! via `OnceLock` so subsequent calls are O(1).
//!
//! On non-Linux platforms, the probe always returns `false`.

use std::path::Path;
use std::sync::OnceLock;

/// Cached result of the `O_TMPFILE` availability probe.
static O_TMPFILE_AVAILABLE: OnceLock<bool> = OnceLock::new();

/// Probes whether the filesystem at `path` supports `O_TMPFILE`.
///
/// On Linux, performs a one-time probe by attempting `open(path, O_TMPFILE | O_WRONLY, 0o600)`.
/// If the syscall succeeds, the file descriptor is closed immediately and the result is
/// cached as `true`. If it fails (e.g., `ENOTSUP`, `EOPNOTSUPP`, `EISDIR`, or old kernel),
/// the result is cached as `false`.
///
/// On non-Linux platforms, always returns `false` without performing any syscall.
///
/// The result is cached per-process - only the first call performs the actual probe.
/// Subsequent calls return the cached value in O(1).
#[must_use]
pub fn o_tmpfile_available(path: &Path) -> bool {
    *O_TMPFILE_AVAILABLE.get_or_init(|| probe_o_tmpfile(path))
}

/// Performs the actual `O_TMPFILE` probe on Linux.
#[cfg(target_os = "linux")]
fn probe_o_tmpfile(path: &Path) -> bool {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let c_path = match CString::new(path.as_os_str().as_bytes()) {
        Ok(p) => p,
        Err(_) => return false,
    };

    // O_TMPFILE is defined as __O_TMPFILE | O_DIRECTORY (0x00410000 | 0x00010000).
    // Use libc::O_TMPFILE which provides the correct combined value.
    // Safety: open() with O_TMPFILE creates an unnamed inode - no filesystem
    // side effects. We close the fd immediately on success.
    let fd = unsafe {
        libc::open(
            c_path.as_ptr(),
            libc::O_TMPFILE | libc::O_WRONLY,
            0o600 as libc::mode_t,
        )
    };

    if fd >= 0 {
        // Safety: fd is a valid file descriptor we just opened.
        unsafe {
            libc::close(fd);
        }
        true
    } else {
        false
    }
}

/// Stub for non-Linux platforms - `O_TMPFILE` is a Linux-specific feature.
#[cfg(not(target_os = "linux"))]
fn probe_o_tmpfile(_path: &Path) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// Verifies that `o_tmpfile_available` runs without panicking on any platform.
    #[test]
    fn probe_does_not_panic() {
        // Use /tmp as a reasonable probe directory on all platforms.
        let result = o_tmpfile_available(Path::new("/tmp"));
        // On non-Linux, must be false. On Linux, depends on filesystem support.
        #[cfg(not(target_os = "linux"))]
        assert!(!result);
        // On any platform, the result is a valid bool (no panic).
        let _ = result;
    }

    /// Verifies that repeated calls return the same cached value.
    #[test]
    fn caching_returns_consistent_result() {
        let first = o_tmpfile_available(Path::new("/tmp"));
        let second = o_tmpfile_available(Path::new("/tmp"));
        let third = o_tmpfile_available(Path::new("/tmp"));
        assert_eq!(first, second);
        assert_eq!(second, third);
    }

    /// Verifies that the probe returns false for a nonexistent directory.
    /// Because `OnceLock` caches the first call, this test uses the internal
    /// `probe_o_tmpfile` function directly to avoid interference.
    #[test]
    fn probe_nonexistent_path_returns_false() {
        let result = probe_o_tmpfile(Path::new("/nonexistent/path/that/does/not/exist"));
        assert!(!result);
    }

    /// Verifies that the probe handles a path containing a null byte gracefully.
    #[test]
    fn probe_null_byte_in_path_returns_false() {
        let result = probe_o_tmpfile(Path::new("/tmp/\0bad"));
        assert!(!result);
    }
}
