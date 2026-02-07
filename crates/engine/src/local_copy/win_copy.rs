//! Windows CopyFileEx optimization with dual-path runtime selection.
//!
//! This module provides Windows-optimized file copying using `CopyFileExW` when
//! available, with automatic fallback to standard copy operations. The dual-path
//! approach tries Windows API copy first (with optional no-buffering optimization),
//! then falls back to standard read/write copy on error.
//!
//! On Windows, `CopyFileExW` with `COPY_FILE_NO_BUFFERING` flag can improve
//! performance for large files by bypassing the system cache. This is particularly
//! useful for rsync-style operations involving large file transfers.
//!
//! # Platform Support
//!
//! - **Windows**: Uses `CopyFileExW` syscall with optional no-buffering for large files
//! - **Other platforms**: Always uses standard copy (CopyFileExW not available)
//!
//! # Examples
//!
//! ```
//! use engine::local_copy::win_copy::{copy_file_optimized, WinCopyResult};
//! use std::path::Path;
//!
//! # let temp = tempfile::tempdir().unwrap();
//! # let src = temp.path().join("source.txt");
//! # let dst = temp.path().join("dest.txt");
//! # std::fs::write(&src, b"data").unwrap();
//! // Try Windows-optimized copy, falling back to standard copy if needed
//! let result = copy_file_optimized(&src, &dst).expect("operation succeeds");
//! match result {
//!     WinCopyResult::WindowsCopy(bytes) => println!("Windows copy: {} bytes", bytes),
//!     WinCopyResult::StandardCopy(bytes) => println!("Standard copy: {} bytes", bytes),
//! }
//! ```

use std::io;
use std::path::Path;

/// Threshold above which COPY_FILE_NO_BUFFERING is used on Windows.
///
/// Files larger than 4MB benefit from unbuffered I/O by bypassing the system
/// cache, reducing memory pressure and improving throughput for large transfers.
pub const NO_BUFFERING_THRESHOLD: u64 = 4 * 1024 * 1024;

/// Result of a Windows-optimized copy operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WinCopyResult {
    /// File was copied using Windows CopyFileExW (bytes_copied).
    WindowsCopy(u64),
    /// File was copied using standard read/write (bytes_copied).
    StandardCopy(u64),
}

impl WinCopyResult {
    /// Get the number of bytes copied regardless of method.
    ///
    /// # Examples
    ///
    /// ```
    /// use engine::local_copy::win_copy::WinCopyResult;
    ///
    /// let win_result = WinCopyResult::WindowsCopy(1024);
    /// assert_eq!(win_result.bytes_copied(), 1024);
    ///
    /// let std_result = WinCopyResult::StandardCopy(2048);
    /// assert_eq!(std_result.bytes_copied(), 2048);
    /// ```
    #[must_use]
    pub fn bytes_copied(&self) -> u64 {
        match self {
            WinCopyResult::WindowsCopy(bytes) => *bytes,
            WinCopyResult::StandardCopy(bytes) => *bytes,
        }
    }
}

/// Copy a file using the best available platform method.
///
/// On Windows, uses `CopyFileExW` with `COPY_FILE_NO_BUFFERING` for large files
/// (> 4MB). On other platforms, falls back to `std::fs::copy`.
///
/// Returns `Ok(WinCopyResult::WindowsCopy(bytes))` if Windows API succeeded,
/// `Ok(WinCopyResult::StandardCopy(bytes))` if standard copy was used,
/// or `Err` on failure of both paths.
///
/// # Platform Behavior
///
/// - **Windows**: Attempts `CopyFileExW` first. Uses `COPY_FILE_NO_BUFFERING`
///   for files larger than 4MB. On failure, falls back to `std::fs::copy`.
/// - **Other platforms**: Always uses `std::fs::copy` (CopyFileExW unavailable).
///
/// # Errors
///
/// Returns an error if:
/// - Source file doesn't exist or isn't readable
/// - Destination cannot be created (permission denied, invalid path, etc.)
/// - Both Windows copy and standard copy fail
///
/// # Examples
///
/// ```
/// use engine::local_copy::win_copy::copy_file_optimized;
/// use tempfile::TempDir;
///
/// let temp = TempDir::new().unwrap();
/// let src = temp.path().join("source.txt");
/// let dst = temp.path().join("dest.txt");
/// std::fs::write(&src, b"data").unwrap();
///
/// let result = copy_file_optimized(&src, &dst).expect("copy should succeed");
/// assert_eq!(result.bytes_copied(), 4);
/// ```
pub fn copy_file_optimized(src: &Path, dst: &Path) -> io::Result<WinCopyResult> {
    // Get source file size to determine if we should use no-buffering
    let metadata = std::fs::metadata(src)?;
    let file_size = metadata.len();
    let use_no_buffering = file_size > NO_BUFFERING_THRESHOLD;

    // Try Windows-optimized copy first (Windows only, returns Unsupported on other platforms)
    match try_win_copy(src, dst, use_no_buffering) {
        Ok(bytes) => Ok(WinCopyResult::WindowsCopy(bytes)),
        Err(_e) => {
            // Windows copy failed or not available, clean up any partial destination
            // and fall back to standard copy
            let _ = std::fs::remove_file(dst); // Ignore errors (dst may not exist)

            // Fall back to standard copy
            let bytes = copy_file_standard(src, dst)?;
            Ok(WinCopyResult::StandardCopy(bytes))
        }
    }
}

/// Copy a file using Windows CopyFileExW (Windows only).
///
/// Returns `Unsupported` error on non-Windows platforms.
///
/// # Platform Support
///
/// - **Windows**: Calls `CopyFileExW` syscall with optional `COPY_FILE_NO_BUFFERING` flag
/// - **Other platforms**: Always returns `ErrorKind::Unsupported`
///
/// # Arguments
///
/// * `src` - Source file path
/// * `dst` - Destination file path
/// * `use_no_buffering` - Whether to use `COPY_FILE_NO_BUFFERING` flag (for large files)
///
/// # Errors
///
/// Returns an error if:
/// - Platform doesn't support CopyFileExW (non-Windows)
/// - Source doesn't exist or isn't readable
/// - Destination cannot be created
/// - I/O error during copy
///
/// # Examples
///
/// ```
/// use engine::local_copy::win_copy::try_win_copy;
/// use tempfile::TempDir;
///
/// let temp = TempDir::new().unwrap();
/// let src = temp.path().join("source.txt");
/// let dst = temp.path().join("dest.txt");
/// std::fs::write(&src, b"data").unwrap();
///
/// // On Windows: might succeed; on Linux: returns Unsupported
/// let result = try_win_copy(&src, &dst, false);
/// # #[cfg(not(target_os = "windows"))]
/// # assert!(result.is_err());
/// ```
pub fn try_win_copy(src: &Path, dst: &Path, use_no_buffering: bool) -> io::Result<u64> {
    try_win_copy_impl(src, dst, use_no_buffering)
}

/// Standard file copy (always available on all platforms).
///
/// Uses `std::fs::copy` which provides platform-optimal copy behavior:
/// - Linux: `copy_file_range()`, `sendfile()`, or fallback
/// - Windows: `CopyFileExW` with appropriate flags
/// - Other: Read/write loop
///
/// # Errors
///
/// Returns an error if:
/// - Source file doesn't exist or isn't readable
/// - Destination cannot be created
/// - I/O error during copy
///
/// # Examples
///
/// ```
/// use engine::local_copy::win_copy::copy_file_standard;
/// use tempfile::TempDir;
///
/// let temp = TempDir::new().unwrap();
/// let src = temp.path().join("source.txt");
/// let dst = temp.path().join("dest.txt");
/// std::fs::write(&src, b"test data").unwrap();
///
/// let bytes = copy_file_standard(&src, &dst).expect("copy should succeed");
/// assert_eq!(bytes, 9);
/// ```
pub fn copy_file_standard(src: &Path, dst: &Path) -> io::Result<u64> {
    std::fs::copy(src, dst)
}

// ---------------------------------------------------------------------------
// Platform-specific implementations
// ---------------------------------------------------------------------------

#[cfg(target_os = "windows")]
fn try_win_copy_impl(src: &Path, dst: &Path, use_no_buffering: bool) -> io::Result<u64> {
    use std::os::windows::ffi::OsStrExt;

    // Convert paths to wide strings for Win32 API
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

    // Set flags: COPY_FILE_NO_BUFFERING = 0x00000008 for large files
    let flags = if use_no_buffering {
        0x00000008_u32 // COPY_FILE_NO_BUFFERING
    } else {
        0_u32
    };

    // SAFETY: We're passing valid wide-string pointers to CopyFileExW.
    // - src_wide and dst_wide are null-terminated wide strings
    // - We pass null pointers for the progress callback and cancel flag (not using them)
    // - The syscall is safe to call with valid paths
    // - Any errors are returned via GetLastError and converted to io::Error
    let result = unsafe {
        windows_sys::Win32::Storage::FileSystem::CopyFileExW(
            src_wide.as_ptr(),
            dst_wide.as_ptr(),
            None,                 // no progress callback
            std::ptr::null(),     // no callback data
            std::ptr::null_mut(), // no cancel flag
            flags,
        )
    };

    if result != 0 {
        // Success: get file size for return value
        let metadata = std::fs::metadata(dst)?;
        Ok(metadata.len())
    } else {
        // Failed: return last OS error
        Err(io::Error::last_os_error())
    }
}

#[cfg(not(target_os = "windows"))]
fn try_win_copy_impl(_src: &Path, _dst: &Path, _use_no_buffering: bool) -> io::Result<u64> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "CopyFileExW not available on this platform",
    ))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn setup_test_files(
        dir: &Path,
        name: &str,
        content: &[u8],
    ) -> (std::path::PathBuf, std::path::PathBuf) {
        let src = dir.join(format!("{name}_src.txt"));
        let dst = dir.join(format!("{name}_dst.txt"));
        std::fs::write(&src, content).expect("write source file");
        (src, dst)
    }

    #[test]
    fn test_copy_file_standard() {
        let temp = TempDir::new().expect("create temp dir");
        let (src, dst) = setup_test_files(temp.path(), "standard", b"test data");

        let bytes = copy_file_standard(&src, &dst).expect("copy should succeed");
        assert_eq!(bytes, 9, "should copy 9 bytes");
        assert!(dst.exists(), "destination should exist");

        let content = std::fs::read(&dst).expect("read destination");
        assert_eq!(content, b"test data", "content should match");
    }

    #[test]
    fn test_copy_preserves_content() {
        let temp = TempDir::new().expect("create temp dir");
        let content = b"The quick brown fox jumps over the lazy dog";
        let (src, dst) = setup_test_files(temp.path(), "preserve", content);

        copy_file_optimized(&src, &dst).expect("operation should succeed");

        let dst_content = std::fs::read(&dst).expect("read destination");
        assert_eq!(
            dst_content, content,
            "copied file should have identical content"
        );
    }

    #[test]
    fn test_copy_file_optimized_nonexistent_src() {
        let temp = TempDir::new().expect("create temp dir");
        let src = temp.path().join("nonexistent.txt");
        let dst = temp.path().join("dest.txt");

        let result = copy_file_optimized(&src, &dst);
        assert!(result.is_err(), "should error on missing source");
    }

    #[test]
    fn test_copy_file_optimized_creates_dst() {
        let temp = TempDir::new().expect("create temp dir");
        let src = temp.path().join("creates_src.txt");
        let dst = temp.path().join("creates_dst.txt");

        // Create only the source file
        std::fs::write(&src, b"data").expect("write source file");

        assert!(
            !dst.exists(),
            "destination should not exist before operation"
        );

        copy_file_optimized(&src, &dst).expect("operation should succeed");

        assert!(dst.exists(), "destination should exist after operation");
    }

    #[test]
    fn test_copy_file_optimized_result_type() {
        let temp = TempDir::new().expect("create temp dir");
        let (src, dst) = setup_test_files(temp.path(), "result_type", b"test");

        let result = copy_file_optimized(&src, &dst).expect("operation should succeed");

        #[cfg(target_os = "windows")]
        {
            // On Windows, should get WindowsCopy result
            match result {
                WinCopyResult::WindowsCopy(bytes) => {
                    assert_eq!(bytes, 4, "should copy 4 bytes");
                }
                WinCopyResult::StandardCopy(_) => {
                    // Fallback might happen if Windows API fails
                }
            }
        }

        #[cfg(not(target_os = "windows"))]
        {
            // On non-Windows, should always be StandardCopy
            match result {
                WinCopyResult::WindowsCopy(_) => {
                    panic!("should not use Windows copy on non-Windows")
                }
                WinCopyResult::StandardCopy(bytes) => {
                    assert_eq!(bytes, 4, "should copy 4 bytes");
                }
            }
        }
    }

    #[test]
    fn test_try_win_copy_on_linux() {
        #[cfg(not(target_os = "windows"))]
        {
            let temp = TempDir::new().expect("create temp dir");
            let (src, dst) = setup_test_files(temp.path(), "wincopy_linux", b"data");

            let result = try_win_copy(&src, &dst, false);
            assert!(result.is_err(), "Windows copy should fail on non-Windows");

            let err = result.unwrap_err();
            assert_eq!(
                err.kind(),
                io::ErrorKind::Unsupported,
                "should return Unsupported error"
            );
        }
    }

    #[test]
    fn test_copy_empty_file() {
        let temp = TempDir::new().expect("create temp dir");
        let (src, dst) = setup_test_files(temp.path(), "empty", b"");

        let result = copy_file_optimized(&src, &dst).expect("operation should succeed");

        assert!(dst.exists(), "destination should exist");
        let content = std::fs::read(&dst).expect("read destination");
        assert_eq!(content, b"", "empty file should remain empty");

        // Verify result reports correct bytes
        assert_eq!(
            result.bytes_copied(),
            0,
            "should copy 0 bytes for empty file"
        );
    }

    #[test]
    fn test_copy_large_file() {
        let temp = TempDir::new().expect("create temp dir");

        // Create 1MB file
        let large_content = vec![0xAB_u8; 1024 * 1024];
        let src = temp.path().join("large_src.txt");
        let dst = temp.path().join("large_dst.txt");
        std::fs::write(&src, &large_content).expect("write large file");

        let result = copy_file_optimized(&src, &dst).expect("operation should succeed");

        assert!(dst.exists(), "destination should exist");
        let dst_content = std::fs::read(&dst).expect("read destination");
        assert_eq!(dst_content.len(), 1024 * 1024, "should copy all bytes");
        assert_eq!(dst_content, large_content, "content should match exactly");

        // Verify result
        assert_eq!(
            result.bytes_copied(),
            1024 * 1024,
            "should report correct byte count"
        );
    }

    #[test]
    fn test_win_copy_result_bytes_copied() {
        // Test WindowsCopy variant
        let win_result = WinCopyResult::WindowsCopy(1024);
        assert_eq!(
            win_result.bytes_copied(),
            1024,
            "WindowsCopy should return correct bytes"
        );

        // Test StandardCopy variant
        let std_result = WinCopyResult::StandardCopy(2048);
        assert_eq!(
            std_result.bytes_copied(),
            2048,
            "StandardCopy should return correct bytes"
        );

        // Test zero bytes
        let zero_result = WinCopyResult::StandardCopy(0);
        assert_eq!(
            zero_result.bytes_copied(),
            0,
            "should handle zero bytes correctly"
        );
    }

    #[test]
    fn test_parity_optimized_vs_standard() {
        let temp = TempDir::new().expect("create temp dir");

        // Test content with various patterns
        let mut test_content = Vec::new();
        test_content.extend_from_slice(b"ASCII text\n");
        test_content.extend_from_slice(&[0x00, 0xFF, 0xAA, 0x55]); // Binary data
        test_content.extend_from_slice("Unicode: \u{1F980}\u{1F99E}".as_bytes()); // UTF-8

        let src = temp.path().join("parity_src.txt");
        std::fs::write(&src, &test_content).expect("write source");

        // Test optimized path
        let dst1 = temp.path().join("parity_dst1.txt");
        copy_file_optimized(&src, &dst1).expect("copy_file_optimized should succeed");

        // Test standard copy path
        let dst2 = temp.path().join("parity_dst2.txt");
        copy_file_standard(&src, &dst2).expect("copy_file_standard should succeed");

        // Both should produce identical results
        let content1 = std::fs::read(&dst1).expect("read dst1");
        let content2 = std::fs::read(&dst2).expect("read dst2");

        assert_eq!(
            content1, content2,
            "both copy methods should produce identical output"
        );
        assert_eq!(content1, test_content, "both should match source");
    }

    #[test]
    #[allow(clippy::assertions_on_constants)]
    fn test_no_buffering_threshold() {
        // Verify the threshold constant has expected value
        assert_eq!(
            NO_BUFFERING_THRESHOLD,
            4 * 1024 * 1024,
            "threshold should be 4MB"
        );
        assert!(NO_BUFFERING_THRESHOLD > 0, "threshold must be positive");
    }

    #[test]
    fn test_win_copy_result_equality() {
        assert_eq!(
            WinCopyResult::WindowsCopy(100),
            WinCopyResult::WindowsCopy(100)
        );
        assert_eq!(
            WinCopyResult::StandardCopy(100),
            WinCopyResult::StandardCopy(100)
        );
        assert_ne!(
            WinCopyResult::WindowsCopy(100),
            WinCopyResult::StandardCopy(100)
        );
        assert_ne!(
            WinCopyResult::WindowsCopy(100),
            WinCopyResult::WindowsCopy(200)
        );
    }

    #[test]
    fn test_copy_file_standard_nonexistent_src() {
        let temp = TempDir::new().expect("create temp dir");
        let src = temp.path().join("nonexistent.txt");
        let dst = temp.path().join("dest.txt");

        let result = copy_file_standard(&src, &dst);
        assert!(result.is_err(), "should error on missing source");
    }
}
