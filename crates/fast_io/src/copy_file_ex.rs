//! Windows `CopyFileExW` file copy with automatic fallback.
//!
//! This module provides a safe wrapper around the Windows `CopyFileExW` syscall
//! for efficient file copying, following the same pattern as [`copy_file_range`]
//! (Linux) and [`sendfile`] (Linux) in this crate.
//!
//! # Platform Support
//!
//! - **Windows**: Uses `CopyFileExW` with optional `COPY_FILE_NO_BUFFERING` flag
//!   for large files (> 4 MB), bypassing the system cache for better throughput
//! - **Other platforms**: Returns `ErrorKind::Unsupported`, allowing callers to
//!   fall back to standard I/O
//!
//! # Example
//!
//! ```no_run
//! use std::path::Path;
//! use fast_io::copy_file_ex::try_copy_file_ex;
//!
//! # fn main() -> std::io::Result<()> {
//! let src = Path::new("source.bin");
//! let dst = Path::new("destination.bin");
//! match try_copy_file_ex(src, dst) {
//!     Ok(bytes) => println!("Copied {} bytes via CopyFileExW", bytes),
//!     Err(e) if e.kind() == std::io::ErrorKind::Unsupported => {
//!         // Fall back to std::fs::copy on non-Windows
//!         std::fs::copy(src, dst)?;
//!     }
//!     Err(e) => return Err(e),
//! }
//! # Ok(())
//! # }
//! ```
//!
//! [`copy_file_range`]: crate::copy_file_range
//! [`sendfile`]: crate::sendfile

use std::io;
use std::path::Path;

/// Threshold above which `COPY_FILE_NO_BUFFERING` is used on Windows.
///
/// Files larger than 4 MB benefit from unbuffered I/O by bypassing the system
/// cache, reducing memory pressure and improving throughput for large transfers.
/// Smaller files use buffered copy to avoid alignment overhead.
pub const NO_BUFFERING_THRESHOLD: u64 = 4 * 1024 * 1024;

/// Copy a file using Windows `CopyFileExW`.
///
/// On Windows, this invokes `CopyFileExW` with `COPY_FILE_NO_BUFFERING` for files
/// larger than [`NO_BUFFERING_THRESHOLD`] (4 MB). The no-buffering flag bypasses the
/// system cache, which improves throughput for large sequential copies typical of
/// rsync workloads.
///
/// On non-Windows platforms, returns `ErrorKind::Unsupported` so callers can fall
/// back to portable copy methods.
///
/// # Arguments
///
/// * `src` - Source file path (must exist and be readable)
/// * `dst` - Destination file path (created or overwritten)
///
/// # Returns
///
/// The number of bytes copied on success.
///
/// # Errors
///
/// Returns an error if:
/// - The platform is not Windows (`ErrorKind::Unsupported`)
/// - Source file does not exist or is not readable
/// - Destination cannot be created (permission denied, invalid path, etc.)
/// - The underlying `CopyFileExW` call fails
///
/// # Example
///
/// ```no_run
/// use fast_io::copy_file_ex::try_copy_file_ex;
/// use std::path::Path;
///
/// let src = Path::new("input.dat");
/// let dst = Path::new("output.dat");
/// match try_copy_file_ex(src, dst) {
///     Ok(bytes) => println!("Copied {bytes} bytes"),
///     Err(e) => eprintln!("Copy failed: {e}"),
/// }
/// ```
pub fn try_copy_file_ex(src: &Path, dst: &Path) -> io::Result<u64> {
    try_copy_file_ex_impl(src, dst)
}

/// Minimal FFI wrapper isolating the single unsafe call behind a safe API.
///
/// # Safety
///
/// The `CopyFileExW` call is safe because:
/// - `src_wide` and `dst_wide` are null-terminated UTF-16 slices produced by
///   `OsStrExt::encode_wide` chained with `once(0)`.
/// - Progress callback, user data, and cancel pointers are null (unused).
/// - The `flags` parameter is either 0 or `COPY_FILE_NO_BUFFERING` (0x8).
#[cfg(windows)]
#[allow(unsafe_code)]
mod ffi {
    use std::io;

    /// Windows `COPY_FILE_NO_BUFFERING` flag value.
    pub const COPY_FILE_NO_BUFFERING: u32 = 0x0000_0008;

    /// Call `CopyFileExW` with the given null-terminated wide-string paths.
    pub fn copy_file_ex_w(src_wide: &[u16], dst_wide: &[u16], flags: u32) -> io::Result<()> {
        // SAFETY: src_wide and dst_wide are null-terminated UTF-16 slices
        // produced by OsStrExt::encode_wide + chain(once(0)).
        // Progress callback, data, and cancel pointers are null (unused).
        let result = unsafe {
            windows_sys::Win32::Storage::FileSystem::CopyFileExW(
                src_wide.as_ptr(),
                dst_wide.as_ptr(),
                None,
                std::ptr::null(),
                std::ptr::null_mut(),
                flags,
            )
        };
        if result != 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
}

#[cfg(windows)]
fn try_copy_file_ex_impl(src: &Path, dst: &Path) -> io::Result<u64> {
    use std::os::windows::ffi::OsStrExt;

    let src_wide: Vec<u16> = src
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let dst_wide: Vec<u16> = dst
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    // Bypass the system cache for files above the threshold: large sequential
    // copies otherwise pollute the cache and take a hit from the buffer copy.
    let file_size = std::fs::metadata(src)?.len();
    let flags = if file_size > NO_BUFFERING_THRESHOLD {
        ffi::COPY_FILE_NO_BUFFERING
    } else {
        0
    };

    ffi::copy_file_ex_w(&src_wide, &dst_wide, flags)?;

    // CopyFileExW does not return the byte count - read it from the destination
    let dst_meta = std::fs::metadata(dst)?;
    Ok(dst_meta.len())
}

#[cfg(not(windows))]
fn try_copy_file_ex_impl(_src: &Path, _dst: &Path) -> io::Result<u64> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "CopyFileExW not available on this platform",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_try_copy_file_ex_nonexistent_src() {
        let temp = TempDir::new().expect("create temp dir");
        let src = temp.path().join("nonexistent.txt");
        let dst = temp.path().join("dest.txt");

        let result = try_copy_file_ex(&src, &dst);
        assert!(result.is_err(), "should error on missing source");
    }

    #[cfg(not(windows))]
    #[test]
    fn test_try_copy_file_ex_returns_unsupported() {
        let temp = TempDir::new().expect("create temp dir");
        let src = temp.path().join("source.txt");
        let dst = temp.path().join("dest.txt");
        std::fs::write(&src, b"data").expect("write source");

        let result = try_copy_file_ex(&src, &dst);
        assert!(result.is_err(), "should fail on non-Windows");

        let err = result.unwrap_err();
        assert_eq!(
            err.kind(),
            io::ErrorKind::Unsupported,
            "should return Unsupported error kind"
        );
    }

    #[cfg(windows)]
    #[test]
    fn test_try_copy_file_ex_copies_content() {
        let temp = TempDir::new().expect("create temp dir");
        let src = temp.path().join("source.txt");
        let dst = temp.path().join("dest.txt");
        let content = b"The quick brown fox jumps over the lazy dog";
        std::fs::write(&src, content).expect("write source");

        let bytes = try_copy_file_ex(&src, &dst).expect("copy should succeed");

        assert_eq!(bytes, content.len() as u64, "byte count should match");
        let dst_content = std::fs::read(&dst).expect("read destination");
        assert_eq!(dst_content, content, "content should match source");
    }

    #[cfg(windows)]
    #[test]
    fn test_try_copy_file_ex_empty_file() {
        let temp = TempDir::new().expect("create temp dir");
        let src = temp.path().join("empty_src.txt");
        let dst = temp.path().join("empty_dst.txt");
        std::fs::write(&src, b"").expect("write empty source");

        let bytes = try_copy_file_ex(&src, &dst).expect("copy should succeed");

        assert_eq!(bytes, 0, "empty file should copy 0 bytes");
        assert!(dst.exists(), "destination should exist");
    }

    #[test]
    #[allow(clippy::assertions_on_constants)]
    fn test_no_buffering_threshold_value() {
        assert_eq!(
            NO_BUFFERING_THRESHOLD,
            4 * 1024 * 1024,
            "threshold should be 4 MB"
        );
        assert!(NO_BUFFERING_THRESHOLD > 0, "threshold must be positive");
    }
}
