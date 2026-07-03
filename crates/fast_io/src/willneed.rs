//! Kernel read-ahead hint for basis files (`POSIX_FADV_WILLNEED`).
//!
//! Warms the page cache for a file the caller is about to read, so a later
//! `mmap`/`read` finds its pages already resident instead of blocking on disk.
//! This is a pure advisory hint: it never changes the bytes read, only the
//! timing of the read.
//!
//! `posix_fadvise` is Linux/Android-only (BSD/macOS `libc` has no such symbol),
//! so on every other platform this is a no-op returning `Ok(())`.

use std::fs::File;
use std::io;

/// Hints to the kernel that the entire file will be read soon.
///
/// On Linux/Android this issues `posix_fadvise(fd, 0, 0, POSIX_FADV_WILLNEED)`
/// (offset 0, length 0 means "to end of file"), asking the kernel to begin
/// asynchronous read-ahead into the page cache. The call is advisory: the
/// kernel may ignore it, and it never alters observable file contents.
///
/// On all other platforms this is a no-op returning `Ok(())`.
///
/// # Errors
///
/// Returns the OS error if `posix_fadvise` reports one. Callers treat a
/// prefetch failure as a silent no-op (the hint is best-effort).
#[cfg(any(target_os = "linux", target_os = "android"))]
#[allow(unsafe_code)]
pub fn hint_basis_willneed(file: &File) -> io::Result<()> {
    use std::os::fd::AsRawFd;
    // SAFETY: `file` is a valid open file; the borrow guarantees the fd stays
    // open for the call. offset 0 / len 0 = whole file. POSIX_FADV_WILLNEED
    // is a pure advisory hint that never accesses user memory.
    let ret = unsafe { libc::posix_fadvise(file.as_raw_fd(), 0, 0, libc::POSIX_FADV_WILLNEED) };
    if ret != 0 {
        // posix_fadvise returns the error code directly, not via errno.
        Err(io::Error::from_raw_os_error(ret))
    } else {
        Ok(())
    }
}

/// No-op on platforms without `posix_fadvise` (BSD/macOS/Windows).
///
/// # Errors
///
/// Never returns an error on these platforms.
#[cfg(not(any(target_os = "linux", target_os = "android")))]
pub fn hint_basis_willneed(_file: &File) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn hint_willneed_on_populated_file_succeeds() {
        let mut temp = NamedTempFile::new().expect("create temp file");
        temp.write_all(&[0u8; 64 * 1024]).expect("write");
        temp.flush().expect("flush");

        let file = temp.reopen().expect("reopen");
        assert!(
            hint_basis_willneed(&file).is_ok(),
            "willneed hint should succeed on a populated file"
        );
    }

    #[test]
    fn hint_willneed_on_empty_file_succeeds() {
        let temp = NamedTempFile::new().expect("create temp file");
        let file = temp.reopen().expect("reopen");
        assert!(
            hint_basis_willneed(&file).is_ok(),
            "willneed hint should succeed on an empty file"
        );
    }
}
