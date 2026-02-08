//! Prefetch hints for sequential file reads.
//!
//! Uses `posix_fadvise(FADV_SEQUENTIAL)` on Linux/Unix to hint to the kernel
//! that the file will be read sequentially, enabling aggressive read-ahead.
//! On unsupported platforms, the hints are silently ignored (no-op).

#![allow(dead_code, unsafe_code)]

use std::fs::File;
use std::io;

/// Minimum file size to bother with fadvise hints.
/// Small files are already in cache or read in a single I/O operation.
const FADVISE_THRESHOLD: u64 = 256 * 1024; // 256KB

/// Advises the OS that the file will be read sequentially.
///
/// This enables the kernel's read-ahead mechanism to prefetch pages,
/// which can significantly improve throughput for large sequential reads.
///
/// Files smaller than 256KB are skipped (overhead not worthwhile).
/// On non-Linux platforms, this is a no-op.
pub fn advise_sequential_read(file: &File, file_size: u64) -> io::Result<()> {
    if file_size < FADVISE_THRESHOLD {
        return Ok(()); // Too small to benefit
    }
    advise_sequential_impl(file, file_size)
}

/// Advises the OS that the file data is no longer needed.
///
/// After finishing a transfer, this hint allows the kernel to evict
/// the file's pages from the page cache, freeing memory for other uses.
pub fn advise_dontneed(file: &File, file_size: u64) -> io::Result<()> {
    advise_dontneed_impl(file, file_size)
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn advise_sequential_impl(file: &File, file_size: u64) -> io::Result<()> {
    use std::os::fd::AsRawFd;
    // SAFETY: valid fd, offset 0, length = file size, FADV_SEQUENTIAL is safe advice
    let ret = unsafe {
        libc::posix_fadvise(
            file.as_raw_fd(),
            0,
            file_size as i64,
            libc::POSIX_FADV_SEQUENTIAL,
        )
    };
    if ret != 0 {
        // posix_fadvise returns error code directly (not via errno)
        Err(io::Error::from_raw_os_error(ret))
    } else {
        Ok(())
    }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn advise_dontneed_impl(file: &File, file_size: u64) -> io::Result<()> {
    use std::os::fd::AsRawFd;
    let ret = unsafe {
        libc::posix_fadvise(
            file.as_raw_fd(),
            0,
            file_size as i64,
            libc::POSIX_FADV_DONTNEED,
        )
    };
    if ret != 0 {
        Err(io::Error::from_raw_os_error(ret))
    } else {
        Ok(())
    }
}

#[cfg(not(any(target_os = "linux", target_os = "android")))]
fn advise_sequential_impl(_file: &File, _file_size: u64) -> io::Result<()> {
    Ok(()) // No-op on non-Linux
}

#[cfg(not(any(target_os = "linux", target_os = "android")))]
fn advise_dontneed_impl(_file: &File, _file_size: u64) -> io::Result<()> {
    Ok(()) // No-op on non-Linux
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn advise_sequential_on_small_file_is_noop() {
        let mut temp = NamedTempFile::new().expect("create temp file");
        // Write less than FADVISE_THRESHOLD (256KB)
        temp.write_all(&[0u8; 1024]).expect("write");
        temp.flush().expect("flush");

        let file = temp.reopen().expect("reopen");
        let result = advise_sequential_read(&file, 1024);
        assert!(result.is_ok(), "small file advise should succeed (noop)");
    }

    #[test]
    fn advise_sequential_on_large_file_succeeds() {
        let mut temp = NamedTempFile::new().expect("create temp file");
        // Write 1MB (larger than FADVISE_THRESHOLD)
        let data = vec![0u8; 1024 * 1024];
        temp.write_all(&data).expect("write");
        temp.flush().expect("flush");

        let file = temp.reopen().expect("reopen");
        let result = advise_sequential_read(&file, 1024 * 1024);
        // On Linux/Android this should call posix_fadvise, on others it's a noop
        assert!(result.is_ok(), "large file advise should succeed");
    }

    #[test]
    fn advise_dontneed_succeeds() {
        let mut temp = NamedTempFile::new().expect("create temp file");
        temp.write_all(&[0u8; 1024]).expect("write");
        temp.flush().expect("flush");

        let file = temp.reopen().expect("reopen");
        let result = advise_dontneed(&file, 1024);
        assert!(result.is_ok(), "advise_dontneed should succeed");
    }

    #[test]
    fn advise_on_empty_file_succeeds() {
        let temp = NamedTempFile::new().expect("create temp file");
        let file = temp.reopen().expect("reopen");

        let result_seq = advise_sequential_read(&file, 0);
        assert!(
            result_seq.is_ok(),
            "sequential advise on empty file should succeed (noop)"
        );

        let result_dontneed = advise_dontneed(&file, 0);
        assert!(
            result_dontneed.is_ok(),
            "dontneed advise on empty file should succeed"
        );
    }

    #[test]
    #[allow(clippy::assertions_on_constants)]
    fn threshold_constant_is_reasonable() {
        assert!(FADVISE_THRESHOLD > 0, "threshold must be positive");
        assert_eq!(FADVISE_THRESHOLD, 256 * 1024, "threshold should be 256KB");
    }
}
