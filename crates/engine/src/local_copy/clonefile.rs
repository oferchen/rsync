//! Copy-on-write file cloning with dual-path runtime selection.
//!
//! This module provides file cloning using macOS `clonefile()` when available,
//! with automatic fallback to standard copy operations. The dual-path approach
//! tries copy-on-write first (instant, zero data copied), then falls back to
//! standard read/write copy on error.
//!
//! On macOS APFS, `clonefile()` creates instant copy-on-write clones that share
//! storage blocks until modified. This is particularly useful for `--link-dest`
//! scenarios and same-filesystem copies.
//!
//! # Platform Support
//!
//! - **macOS**: Uses `clonefile()` syscall for CoW cloning on supported filesystems (APFS)
//! - **Other platforms**: Always uses standard copy (clonefile not available)
//!
//! # Examples
//!
//! ```
//! use engine::local_copy::clonefile::{clone_or_copy, CloneResult};
//! use std::path::Path;
//!
//! # let temp = tempfile::tempdir().unwrap();
//! # let src = temp.path().join("source.txt");
//! # let dst = temp.path().join("dest.txt");
//! # std::fs::write(&src, b"data").unwrap();
//! // Try to clone, falling back to copy if needed
//! let result = clone_or_copy(&src, &dst).expect("operation succeeds");
//! match result {
//!     CloneResult::Cloned => println!("Instant CoW clone"),
//!     CloneResult::Copied(bytes) => println!("Copied {} bytes", bytes),
//! }
//! ```

use std::io;
use std::path::Path;

/// Result of a clone-or-copy operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CloneResult {
    /// File was cloned using copy-on-write (zero data copied).
    Cloned,
    /// File was copied using standard read/write (bytes_copied).
    Copied(u64),
}

/// Try to clone a file using copy-on-write if available.
///
/// Falls back to standard copy on non-macOS or when clonefile fails
/// (e.g., cross-filesystem, unsupported filesystem).
///
/// Returns `Ok(CloneResult::Cloned)` if CoW clone succeeded,
/// `Ok(CloneResult::Copied(bytes))` if standard copy was used,
/// or `Err` on failure of both paths.
///
/// # Platform Behavior
///
/// - **macOS**: Attempts `clonefile()` first. On failure (e.g., cross-device,
///   unsupported fs), falls back to `std::fs::copy`.
/// - **Other platforms**: Always uses `std::fs::copy` (clonefile unavailable).
///
/// # Errors
///
/// Returns an error if:
/// - Source file doesn't exist or isn't readable
/// - Destination cannot be created (permission denied, invalid path, etc.)
/// - Both clonefile and standard copy fail
pub fn clone_or_copy(src: &Path, dst: &Path) -> io::Result<CloneResult> {
    // Try clonefile first (macOS only, returns Unsupported on other platforms)
    match try_clonefile(src, dst) {
        Ok(()) => Ok(CloneResult::Cloned),
        Err(_e) => {
            // Clonefile failed or not available, clean up any partial destination
            // and fall back to standard copy
            let _ = std::fs::remove_file(dst); // Ignore errors (dst may not exist)

            // Fall back to standard copy
            let bytes = copy_file_standard(src, dst)?;
            Ok(CloneResult::Copied(bytes))
        }
    }
}

/// Try clonefile only (macOS).
///
/// Returns error on non-macOS or if clonefile fails (cross-filesystem,
/// unsupported filesystem, permissions, etc.).
///
/// # Platform Support
///
/// - **macOS**: Calls `clonefile()` syscall
/// - **Other platforms**: Always returns `ErrorKind::Unsupported`
///
/// # Errors
///
/// Returns an error if:
/// - Platform doesn't support clonefile (non-macOS)
/// - Source doesn't exist or isn't readable
/// - Destination already exists (clonefile doesn't overwrite)
/// - Cross-filesystem copy attempted
/// - Filesystem doesn't support CoW (e.g., HFS+)
pub fn try_clonefile(src: &Path, dst: &Path) -> io::Result<()> {
    try_clonefile_impl(src, dst)
}

/// Standard file copy (always available on all platforms).
///
/// Uses `std::fs::copy` which provides platform-optimal copy behavior:
/// - Linux: `copy_file_range()`, `sendfile()`, or fallback
/// - macOS: `copyfile()` with appropriate flags
/// - Other: Read/write loop
///
/// # Errors
///
/// Returns an error if:
/// - Source file doesn't exist or isn't readable
/// - Destination cannot be created
/// - I/O error during copy
pub fn copy_file_standard(src: &Path, dst: &Path) -> io::Result<u64> {
    std::fs::copy(src, dst)
}

// ---------------------------------------------------------------------------
// Platform-specific implementations
// ---------------------------------------------------------------------------

#[cfg(target_os = "macos")]
fn try_clonefile_impl(src: &Path, dst: &Path) -> io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    // Convert paths to C strings
    let src_c = CString::new(src.as_os_str().as_bytes()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "source path contains null byte",
        )
    })?;
    let dst_c = CString::new(dst.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "dest path contains null byte"))?;

    // Call clonefile(src, dst, 0)
    // Flag 0 means no special options (CLONE_NOFOLLOW = 1 could be used for symlinks)
    // SAFETY: We're passing valid C strings to clonefile. The syscall is safe to call
    // with valid paths. Any errors are returned via errno and converted to io::Error.
    let ret = unsafe { libc::clonefile(src_c.as_ptr(), dst_c.as_ptr(), 0) };

    if ret == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(not(target_os = "macos"))]
fn try_clonefile_impl(_src: &Path, _dst: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "clonefile not available on this platform",
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

        clone_or_copy(&src, &dst).expect("operation should succeed");

        let dst_content = std::fs::read(&dst).expect("read destination");
        assert_eq!(
            dst_content, content,
            "copied file should have identical content"
        );
    }

    #[test]
    fn test_clone_or_copy_nonexistent_src() {
        let temp = TempDir::new().expect("create temp dir");
        let src = temp.path().join("nonexistent.txt");
        let dst = temp.path().join("dest.txt");

        let result = clone_or_copy(&src, &dst);
        assert!(result.is_err(), "should error on missing source");
    }

    #[test]
    fn test_clone_or_copy_creates_dst() {
        let temp = TempDir::new().expect("create temp dir");
        let src = temp.path().join("creates_src.txt");
        let dst = temp.path().join("creates_dst.txt");

        // Create only the source file
        std::fs::write(&src, b"data").expect("write source file");

        assert!(
            !dst.exists(),
            "destination should not exist before operation"
        );

        clone_or_copy(&src, &dst).expect("operation should succeed");

        assert!(dst.exists(), "destination should exist after operation");
    }

    #[test]
    fn test_clone_or_copy_overwrites_dst() {
        let temp = TempDir::new().expect("create temp dir");
        let (src, dst) = setup_test_files(temp.path(), "overwrite", b"new data");

        // Pre-populate destination with different content
        std::fs::write(&dst, b"old data").expect("write old data");

        clone_or_copy(&src, &dst).expect("operation should succeed");

        let content = std::fs::read(&dst).expect("read destination");
        assert_eq!(content, b"new data", "should overwrite with new content");
    }

    #[test]
    fn test_clone_or_copy_result_type() {
        let temp = TempDir::new().expect("create temp dir");
        let (src, dst) = setup_test_files(temp.path(), "result_type", b"test");

        let result = clone_or_copy(&src, &dst).expect("operation should succeed");

        #[cfg(target_os = "macos")]
        {
            // On macOS, we might get Cloned (if APFS) or Copied (if HFS+ or error)
            // We can't assert which one without knowing the filesystem
            match result {
                CloneResult::Cloned => {
                    // Clonefile succeeded on APFS
                }
                CloneResult::Copied(bytes) => {
                    // Fallback to copy (HFS+ or cross-device)
                    assert_eq!(bytes, 4, "should copy 4 bytes");
                }
            }
        }

        #[cfg(not(target_os = "macos"))]
        {
            // On non-macOS, should always be Copied
            match result {
                CloneResult::Cloned => panic!("should not clone on non-macOS"),
                CloneResult::Copied(bytes) => {
                    assert_eq!(bytes, 4, "should copy 4 bytes");
                }
            }
        }
    }

    #[test]
    fn test_try_clonefile_on_linux() {
        #[cfg(not(target_os = "macos"))]
        {
            let temp = TempDir::new().expect("create temp dir");
            let (src, dst) = setup_test_files(temp.path(), "clonefile_linux", b"data");

            let result = try_clonefile(&src, &dst);
            assert!(result.is_err(), "clonefile should fail on non-macOS");

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

        let result = clone_or_copy(&src, &dst).expect("operation should succeed");

        assert!(dst.exists(), "destination should exist");
        let content = std::fs::read(&dst).expect("read destination");
        assert_eq!(content, b"", "empty file should remain empty");

        // Verify result reports correct bytes
        match result {
            CloneResult::Cloned => {
                // Empty file cloned
            }
            CloneResult::Copied(bytes) => {
                assert_eq!(bytes, 0, "should copy 0 bytes for empty file");
            }
        }
    }

    #[test]
    fn test_copy_large_file() {
        let temp = TempDir::new().expect("create temp dir");

        // Create 1MB file
        let large_content = vec![0xAB_u8; 1024 * 1024];
        let src = temp.path().join("large_src.txt");
        let dst = temp.path().join("large_dst.txt");
        std::fs::write(&src, &large_content).expect("write large file");

        let result = clone_or_copy(&src, &dst).expect("operation should succeed");

        assert!(dst.exists(), "destination should exist");
        let dst_content = std::fs::read(&dst).expect("read destination");
        assert_eq!(dst_content.len(), 1024 * 1024, "should copy all bytes");
        assert_eq!(dst_content, large_content, "content should match exactly");

        // Verify result
        match result {
            CloneResult::Cloned => {
                // Instant CoW clone (macOS APFS)
            }
            CloneResult::Copied(bytes) => {
                assert_eq!(bytes, 1024 * 1024, "should report correct byte count");
            }
        }
    }

    #[test]
    fn test_parity_clone_or_copy_vs_standard() {
        let temp = TempDir::new().expect("create temp dir");

        // Test content with various patterns
        let mut test_content = Vec::new();
        test_content.extend_from_slice(b"ASCII text\n");
        test_content.extend_from_slice(&[0x00, 0xFF, 0xAA, 0x55]); // Binary data
        test_content.extend_from_slice("Unicode: \u{1F980}\u{1F99E}".as_bytes()); // UTF-8

        let src = temp.path().join("parity_src.txt");
        std::fs::write(&src, &test_content).expect("write source");

        // Test clone_or_copy path
        let dst1 = temp.path().join("parity_dst1.txt");
        clone_or_copy(&src, &dst1).expect("clone_or_copy should succeed");

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
    fn test_copy_file_standard_nonexistent_src() {
        let temp = TempDir::new().expect("create temp dir");
        let src = temp.path().join("nonexistent.txt");
        let dst = temp.path().join("dest.txt");

        let result = copy_file_standard(&src, &dst);
        assert!(result.is_err(), "should error on missing source");
    }

    #[test]
    fn test_clone_result_equality() {
        assert_eq!(CloneResult::Cloned, CloneResult::Cloned);
        assert_eq!(CloneResult::Copied(100), CloneResult::Copied(100));
        assert_ne!(CloneResult::Cloned, CloneResult::Copied(0));
        assert_ne!(CloneResult::Copied(100), CloneResult::Copied(200));
    }
}
