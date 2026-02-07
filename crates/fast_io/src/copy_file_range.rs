//! Zero-copy file transfer using `copy_file_range` syscall with automatic fallback.
//!
//! This module provides high-performance file copying using Linux's `copy_file_range`
//! syscall when available, with automatic fallback to standard read/write on other
//! platforms or when the syscall fails.
//!
//! # Platform Support
//!
//! - **Linux 4.5+**: Uses `copy_file_range` for same-filesystem copies
//! - **Linux 5.3+**: Supports cross-filesystem copies
//! - **Other platforms**: Automatic fallback to buffered read/write
//!
//! # Performance Characteristics
//!
//! - For files < 64KB: Uses read/write directly (lower syscall overhead)
//! - For files >= 64KB: Attempts `copy_file_range` for zero-copy transfer
//! - Fallback path uses 256KB buffer for efficient bulk transfer
//!
//! # Example
//!
//! ```no_run
//! use std::fs::File;
//! use fast_io::copy_file_range::copy_file_contents;
//!
//! # fn main() -> std::io::Result<()> {
//! let source = File::open("source.bin")?;
//! let destination = File::create("destination.bin")?;
//! let copied = copy_file_contents(&source, &destination, 1024 * 1024)?;
//! println!("Copied {} bytes", copied);
//! # Ok(())
//! # }
//! ```

use std::fs::File;
use std::io;
use std::os::fd::AsRawFd;

/// Minimum file size to attempt copy_file_range (below this, read/write is faster).
///
/// Small files benefit from the simpler read/write path due to lower syscall overhead.
const COPY_FILE_RANGE_THRESHOLD: u64 = 64 * 1024; // 64KB

/// Copies bytes between two files, using `copy_file_range` on Linux when available,
/// falling back to userspace read/write otherwise.
///
/// This function automatically selects the optimal transfer method:
/// - Files < 64KB: Direct read/write
/// - Files >= 64KB: Attempts zero-copy via `copy_file_range`, falls back on error
///
/// # Arguments
///
/// * `source` - Source file to read from (uses current file position)
/// * `destination` - Destination file to write to (uses current file position)
/// * `length` - Number of bytes to copy
///
/// # Returns
///
/// The number of bytes actually copied. May be less than `length` if EOF is reached.
///
/// # Errors
///
/// Returns an error if:
/// - Reading from source fails
/// - Writing to destination fails
/// - I/O errors occur during transfer
///
/// # Example
///
/// ```no_run
/// use std::fs::File;
/// use fast_io::copy_file_range::copy_file_contents;
///
/// # fn main() -> std::io::Result<()> {
/// let source = File::open("large_file.bin")?;
/// let dest = File::create("copy.bin")?;
/// let copied = copy_file_contents(&source, &dest, 10 * 1024 * 1024)?;
/// assert_eq!(copied, 10 * 1024 * 1024);
/// # Ok(())
/// # }
/// ```
pub fn copy_file_contents(source: &File, destination: &File, length: u64) -> io::Result<u64> {
    if length >= COPY_FILE_RANGE_THRESHOLD {
        // Attempt zero-copy via copy_file_range, fall back to read/write on error
        if let Ok(copied) = try_copy_file_range(source, destination, length) {
            return Ok(copied);
        }
    }
    copy_file_contents_readwrite(source, destination, length)
}

/// Attempts zero-copy transfer via `copy_file_range` syscall.
///
/// This function directly invokes the Linux `copy_file_range` syscall for optimal
/// performance. It returns an error on any failure, allowing the caller to fall back
/// to standard read/write.
///
/// # Platform Support
///
/// - **Linux 4.5+**: Same filesystem only
/// - **Linux 5.3+**: Cross-filesystem support
/// - **Other platforms**: Always returns `Unsupported` error
///
/// # Arguments
///
/// * `source` - Source file descriptor
/// * `destination` - Destination file descriptor
/// * `length` - Maximum bytes to copy
///
/// # Returns
///
/// The number of bytes copied via `copy_file_range`, or an error if:
/// - The syscall is not available on this platform
/// - The kernel version is too old
/// - Cross-filesystem copy on kernel < 5.3
/// - Source and destination are the same file
/// - File descriptors are invalid
///
/// # Safety
///
/// Uses unsafe FFI to call `libc::copy_file_range`. File descriptors must be valid.
#[cfg(target_os = "linux")]
fn try_copy_file_range(source: &File, destination: &File, length: u64) -> io::Result<u64> {
    let src_fd = source.as_raw_fd();
    let dst_fd = destination.as_raw_fd();
    let mut total_copied: u64 = 0;
    let mut remaining = length;

    while remaining > 0 {
        // copy_file_range takes size_t, which is usize, but returns ssize_t (isize)
        // Limit chunk size to i64::MAX to avoid overflow issues
        let chunk = remaining.min(i64::MAX as u64) as usize;

        // SAFETY: File descriptors are valid (derived from &File references).
        // Null offset pointers instruct the syscall to use and update the current
        // file position, which is the behavior we want.
        let result = unsafe {
            libc::copy_file_range(
                src_fd,
                std::ptr::null_mut(), // Use current position for source
                dst_fd,
                std::ptr::null_mut(), // Use current position for destination
                chunk,
                0, // flags (must be 0)
            )
        };

        if result < 0 {
            let err = io::Error::last_os_error();
            if total_copied == 0 {
                // Failed on first chunk - return error to trigger fallback
                return Err(err);
            }
            // Partial copy succeeded, but now we hit an error - still return error
            return Err(err);
        }

        if result == 0 {
            // EOF reached
            break;
        }

        let copied = result as u64;
        total_copied += copied;
        remaining -= copied;
    }

    Ok(total_copied)
}

/// Stub for non-Linux platforms - always returns `Unsupported`.
#[cfg(not(target_os = "linux"))]
fn try_copy_file_range(_source: &File, _destination: &File, _length: u64) -> io::Result<u64> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "copy_file_range not available on this platform",
    ))
}

/// Standard read/write fallback path.
///
/// Uses buffered I/O with a 256KB buffer for efficient bulk transfer.
/// This path is used on non-Linux platforms, for small files, or when
/// `copy_file_range` fails.
///
/// # Arguments
///
/// * `source` - Source file to read from
/// * `destination` - Destination file to write to
/// * `length` - Number of bytes to copy
///
/// # Returns
///
/// The number of bytes actually copied.
///
/// # Errors
///
/// Returns an error if reading or writing fails.
fn copy_file_contents_readwrite(source: &File, destination: &File, length: u64) -> io::Result<u64> {
    use std::io::{Read, Write};

    let mut reader = io::BufReader::new(source);
    let mut writer = io::BufWriter::new(destination);
    let mut total = 0u64;
    let mut buf = vec![0u8; 256 * 1024]; // 256KB buffer
    let mut remaining = length;

    while remaining > 0 {
        let to_read = (remaining as usize).min(buf.len());
        let n = reader.read(&mut buf[..to_read])?;
        if n == 0 {
            // EOF reached
            break;
        }
        writer.write_all(&buf[..n])?;
        total += n as u64;
        remaining -= n as u64;
    }
    writer.flush()?;

    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Seek, SeekFrom, Write};
    use tempfile::NamedTempFile;

    /// Helper to create a temp file with specified content
    fn create_temp_file(content: &[u8]) -> io::Result<NamedTempFile> {
        let mut file = NamedTempFile::new()?;
        file.write_all(content)?;
        file.flush()?;
        file.seek(SeekFrom::Start(0))?;
        Ok(file)
    }

    /// Helper to read entire file content
    fn read_file_contents(file: &File) -> io::Result<Vec<u8>> {
        use std::io::Read;
        let mut content = Vec::new();
        let mut reader = io::BufReader::new(file);
        reader.read_to_end(&mut content)?;
        Ok(content)
    }

    #[test]
    fn test_copy_small_file_below_threshold() {
        // Small file (< 64KB) should use read/write path directly
        let content = b"Hello, world! This is a small file.";
        let source = create_temp_file(content).unwrap();
        let mut dest = NamedTempFile::new().unwrap();

        let copied =
            copy_file_contents(source.as_file(), dest.as_file(), content.len() as u64).unwrap();

        assert_eq!(copied, content.len() as u64);
        dest.seek(SeekFrom::Start(0)).unwrap();
        let dest_content = read_file_contents(dest.as_file()).unwrap();
        assert_eq!(dest_content, content);
    }

    #[test]
    fn test_copy_large_file_above_threshold() {
        // Large file (>= 64KB) should attempt copy_file_range
        let size = 128 * 1024; // 128KB
        let content: Vec<u8> = (0..size).map(|i| (i % 256) as u8).collect();
        let source = create_temp_file(&content).unwrap();
        let mut dest = NamedTempFile::new().unwrap();

        let copied =
            copy_file_contents(source.as_file(), dest.as_file(), content.len() as u64).unwrap();

        assert_eq!(copied, content.len() as u64);
        dest.seek(SeekFrom::Start(0)).unwrap();
        let dest_content = read_file_contents(dest.as_file()).unwrap();
        assert_eq!(dest_content, content);
    }

    #[test]
    fn test_copy_empty_file() {
        let content = b"";
        let source = create_temp_file(content).unwrap();
        let mut dest = NamedTempFile::new().unwrap();

        let copied = copy_file_contents(source.as_file(), dest.as_file(), 0).unwrap();

        assert_eq!(copied, 0);
        dest.seek(SeekFrom::Start(0)).unwrap();
        let dest_content = read_file_contents(dest.as_file()).unwrap();
        assert_eq!(dest_content, content);
    }

    #[test]
    fn test_copy_partial_eof() {
        // Request more bytes than available - should stop at EOF
        let content = b"Short content";
        let source = create_temp_file(content).unwrap();
        let mut dest = NamedTempFile::new().unwrap();

        let copied = copy_file_contents(source.as_file(), dest.as_file(), 10000).unwrap();

        assert_eq!(copied, content.len() as u64);
        dest.seek(SeekFrom::Start(0)).unwrap();
        let dest_content = read_file_contents(dest.as_file()).unwrap();
        assert_eq!(dest_content, content);
    }

    #[test]
    fn test_copy_exact_threshold() {
        // Exactly at threshold should attempt copy_file_range
        let size = COPY_FILE_RANGE_THRESHOLD as usize;
        let content: Vec<u8> = (0..size).map(|i| (i % 256) as u8).collect();
        let source = create_temp_file(&content).unwrap();
        let mut dest = NamedTempFile::new().unwrap();

        let copied =
            copy_file_contents(source.as_file(), dest.as_file(), content.len() as u64).unwrap();

        assert_eq!(copied, content.len() as u64);
        dest.seek(SeekFrom::Start(0)).unwrap();
        let dest_content = read_file_contents(dest.as_file()).unwrap();
        assert_eq!(dest_content, content);
    }

    #[test]
    fn test_readwrite_fallback_direct() {
        // Test the read/write fallback path directly
        let content = b"Testing fallback path directly";
        let source = create_temp_file(content).unwrap();
        let mut dest = NamedTempFile::new().unwrap();

        let copied =
            copy_file_contents_readwrite(source.as_file(), dest.as_file(), content.len() as u64)
                .unwrap();

        assert_eq!(copied, content.len() as u64);
        dest.seek(SeekFrom::Start(0)).unwrap();
        let dest_content = read_file_contents(dest.as_file()).unwrap();
        assert_eq!(dest_content, content);
    }

    #[test]
    fn test_parity_both_paths() {
        // Both paths should produce identical output for same input
        let size = 256 * 1024; // 256KB - forces copy_file_range attempt
        let content: Vec<u8> = (0..size)
            .map(|i| ((i * 7 + 13) % 256) as u8) // Pseudo-random pattern
            .collect();

        // Path 1: copy_file_contents (attempts copy_file_range)
        let source1 = create_temp_file(&content).unwrap();
        let mut dest1 = NamedTempFile::new().unwrap();
        let copied1 = copy_file_contents(source1.as_file(), dest1.as_file(), size as u64).unwrap();
        dest1.seek(SeekFrom::Start(0)).unwrap();
        let result1 = read_file_contents(dest1.as_file()).unwrap();

        // Path 2: copy_file_contents_readwrite (direct fallback)
        let source2 = create_temp_file(&content).unwrap();
        let mut dest2 = NamedTempFile::new().unwrap();
        let copied2 =
            copy_file_contents_readwrite(source2.as_file(), dest2.as_file(), size as u64).unwrap();
        dest2.seek(SeekFrom::Start(0)).unwrap();
        let result2 = read_file_contents(dest2.as_file()).unwrap();

        // Both should copy all bytes
        assert_eq!(copied1, size as u64);
        assert_eq!(copied2, size as u64);

        // Both should produce identical output
        assert_eq!(result1, result2);
        assert_eq!(result1, content);
    }

    #[test]
    fn test_large_file_multi_chunk() {
        // Test file larger than typical kernel copy_file_range limit
        let size = 2 * 1024 * 1024; // 2MB - may require multiple syscalls
        let content: Vec<u8> = (0..size).map(|i| (i % 256) as u8).collect();
        let source = create_temp_file(&content).unwrap();
        let mut dest = NamedTempFile::new().unwrap();

        let copied = copy_file_contents(source.as_file(), dest.as_file(), size as u64).unwrap();

        assert_eq!(copied, size as u64);
        dest.seek(SeekFrom::Start(0)).unwrap();
        let dest_content = read_file_contents(dest.as_file()).unwrap();
        assert_eq!(dest_content.len(), content.len());
        assert_eq!(dest_content, content);
    }

    #[test]
    fn test_copy_with_file_position() {
        // Test that file position is respected
        let content = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ";
        let mut source = create_temp_file(content).unwrap();
        let mut dest = NamedTempFile::new().unwrap();

        // Seek source to position 10
        source.seek(SeekFrom::Start(10)).unwrap();

        // Copy 10 bytes from position 10
        let copied = copy_file_contents(source.as_file(), dest.as_file(), 10).unwrap();

        assert_eq!(copied, 10);
        dest.seek(SeekFrom::Start(0)).unwrap();
        let dest_content = read_file_contents(dest.as_file()).unwrap();
        assert_eq!(dest_content, b"ABCDEFGHIJ");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn test_try_copy_file_range_linux() {
        // On Linux, try_copy_file_range should succeed for same-filesystem copy
        let content = b"Testing copy_file_range syscall directly";
        let source = create_temp_file(content).unwrap();
        let mut dest = NamedTempFile::new().unwrap();

        // This may fail on old kernels or cross-filesystem, but that's expected
        match try_copy_file_range(source.as_file(), dest.as_file(), content.len() as u64) {
            Ok(copied) => {
                assert_eq!(copied, content.len() as u64);
                dest.seek(SeekFrom::Start(0)).unwrap();
                let dest_content = read_file_contents(dest.as_file()).unwrap();
                assert_eq!(dest_content, content);
            }
            Err(_) => {
                // Fallback is expected on old kernels or cross-filesystem
                eprintln!("copy_file_range not available, fallback will be used");
            }
        }
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn test_try_copy_file_range_non_linux() {
        // On non-Linux platforms, should always return Unsupported
        let content = b"Test";
        let source = create_temp_file(content).unwrap();
        let dest = NamedTempFile::new().unwrap();

        let result = try_copy_file_range(source.as_file(), dest.as_file(), content.len() as u64);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::Unsupported);
    }
}
