//! High-performance file copying with tiered fallback.
//!
//! This module provides optimized file-to-file copying that selects the best
//! available mechanism at runtime, falling through on failure:
//!
//! 1. **io_uring** - Linux 5.6+, `io_uring` feature enabled. Batches read and
//!    write syscalls through a single ring for minimal kernel transitions.
//! 2. **`copy_file_range`** - Linux 4.5+ (same-fs), 5.3+ (cross-fs). Zero-copy
//!    in-kernel transfer.
//! 3. **Buffered read/write** - All platforms. Standard portable fallback.
//!
//! # Performance Characteristics
//!
//! - For files < 64KB: Uses read/write directly (lower syscall overhead)
//! - For files >= 64KB: Attempts io_uring, then `copy_file_range`, then read/write
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
#[cfg(target_os = "linux")]
use std::os::fd::AsRawFd;

/// Minimum file size to attempt io_uring copy (below this, simpler paths win).
///
/// io_uring has ring setup overhead, so it only pays off for larger transfers.
const IO_URING_COPY_THRESHOLD: u64 = 256 * 1024; // 256KB

/// Minimum file size to attempt copy_file_range (below this, read/write is faster).
///
/// Small files benefit from the simpler read/write path due to lower syscall overhead.
const COPY_FILE_RANGE_THRESHOLD: u64 = 64 * 1024; // 64KB

/// Copies bytes between two files using the best available I/O mechanism.
///
/// This function automatically selects the optimal transfer method:
/// - Files >= 256KB: Tries io_uring batched read/write (Linux 5.6+)
/// - Files >= 64KB: Tries `copy_file_range` zero-copy (Linux 4.5+)
/// - All sizes: Falls back to standard buffered read/write
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
    // Tier 1: io_uring batched read/write (Linux 5.6+, large files)
    if length >= IO_URING_COPY_THRESHOLD {
        if let Ok(copied) = try_io_uring_copy(source, destination, length) {
            return Ok(copied);
        }
    }
    // Tier 2: copy_file_range zero-copy (Linux 4.5+)
    if length >= COPY_FILE_RANGE_THRESHOLD {
        if let Ok(copied) = try_copy_file_range(source, destination, length) {
            return Ok(copied);
        }
    }
    // Tier 3: standard buffered read/write
    copy_file_contents_readwrite(source, destination, length)
}

/// Like [`copy_file_contents`] but uses a caller-provided buffer for the
/// read/write fallback path, avoiding per-file heap allocation.
///
/// The buffer pool in the engine crate manages reusable buffers across files.
/// Passing that buffer here eliminates the 256KB `Vec` allocation that the
/// standard fallback path creates for every file, which is significant for
/// workloads with many small files (e.g., 100K x 100B).
pub fn copy_file_contents_buffered(
    source: &File,
    destination: &File,
    length: u64,
    buffer: &mut [u8],
) -> io::Result<u64> {
    // Tier 1: io_uring batched read/write (Linux 5.6+, large files)
    if length >= IO_URING_COPY_THRESHOLD {
        if let Ok(copied) = try_io_uring_copy(source, destination, length) {
            return Ok(copied);
        }
    }
    // Tier 2: copy_file_range zero-copy (Linux 4.5+)
    if length >= COPY_FILE_RANGE_THRESHOLD {
        if let Ok(copied) = try_copy_file_range(source, destination, length) {
            return Ok(copied);
        }
    }
    // Tier 3: buffered read/write with caller-provided buffer
    copy_file_contents_readwrite_with_buffer(source, destination, length, buffer)
}

/// Attempts file copy using io_uring batched read/write operations.
///
/// Creates a temporary io_uring ring and alternates between read and write
/// submissions to transfer data from `source` to `destination`. This avoids
/// the per-syscall overhead of standard read/write by batching operations
/// through the ring.
///
/// Returns the number of bytes copied, or an error if io_uring is unavailable
/// or any ring operation fails - allowing the caller to fall back to
/// `copy_file_range` or standard read/write.
///
/// # Platform support
///
/// - **Linux 5.6+** with `io_uring` feature: Full implementation
/// - **Other platforms**: Always returns `Unsupported` error
#[cfg(all(target_os = "linux", feature = "io_uring"))]
fn try_io_uring_copy(source: &File, destination: &File, length: u64) -> io::Result<u64> {
    use crate::io_uring::{IoUringConfig, is_io_uring_available};

    if !is_io_uring_available() {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "io_uring not available",
        ));
    }

    let config = IoUringConfig::default();
    let mut ring = config.build_ring()?;

    let src_fd = source.as_raw_fd();
    let dst_fd = destination.as_raw_fd();
    let buf_size = config.buffer_size;
    let mut buf = vec![0u8; buf_size];
    let mut total_copied: u64 = 0;
    let mut src_offset: u64 = 0;
    let mut dst_offset: u64 = 0;

    while total_copied < length {
        let want = ((length - total_copied) as usize).min(buf_size);

        let read_entry =
            io_uring::opcode::Read::new(io_uring::types::Fd(src_fd), buf.as_mut_ptr(), want as u32)
                .offset(src_offset)
                .build()
                .user_data(0);

        // SAFETY: fd is valid (borrowed from &File), buffer outlives the operation,
        // and we wait for completion before accessing the buffer.
        unsafe {
            ring.submission()
                .push(&read_entry)
                .map_err(|_| io::Error::other("io_uring SQ full on read"))?;
        }

        ring.submit_and_wait(1)?;

        let cqe = ring
            .completion()
            .next()
            .ok_or_else(|| io::Error::other("io_uring: missing read CQE"))?;

        let read_result = cqe.result();
        if read_result < 0 {
            return Err(io::Error::from_raw_os_error(-read_result));
        }
        if read_result == 0 {
            break; // EOF
        }

        let bytes_read = read_result as usize;
        src_offset += bytes_read as u64;

        let write_entry = io_uring::opcode::Write::new(
            io_uring::types::Fd(dst_fd),
            buf.as_ptr(),
            bytes_read as u32,
        )
        .offset(dst_offset)
        .build()
        .user_data(1);

        // SAFETY: fd is valid (borrowed from &File), buffer outlives the operation,
        // and we wait for completion before reusing the buffer.
        unsafe {
            ring.submission()
                .push(&write_entry)
                .map_err(|_| io::Error::other("io_uring SQ full on write"))?;
        }

        ring.submit_and_wait(1)?;

        let cqe = ring
            .completion()
            .next()
            .ok_or_else(|| io::Error::other("io_uring: missing write CQE"))?;

        let write_result = cqe.result();
        if write_result < 0 {
            return Err(io::Error::from_raw_os_error(-write_result));
        }

        let bytes_written = write_result as usize;
        if bytes_written == 0 {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "io_uring write returned 0",
            ));
        }

        // Handle short writes by adjusting source offset back
        if bytes_written < bytes_read {
            src_offset -= (bytes_read - bytes_written) as u64;
        }

        dst_offset += bytes_written as u64;
        total_copied += bytes_written as u64;
    }

    Ok(total_copied)
}

/// Stub for platforms without io_uring - always returns `Unsupported`.
#[cfg(not(all(target_os = "linux", feature = "io_uring")))]
fn try_io_uring_copy(_source: &File, _destination: &File, _length: u64) -> io::Result<u64> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "io_uring not available on this platform",
    ))
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
            // Always surface the error so the caller can fall back; partial copies
            // are still treated as failures because a partial dst is invalid.
            return Err(io::Error::last_os_error());
        }

        if result == 0 {
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
            break;
        }
        writer.write_all(&buf[..n])?;
        total += n as u64;
        remaining -= n as u64;
    }
    writer.flush()?;

    Ok(total)
}

/// Read/write path using a caller-provided buffer.
///
/// Avoids the per-file 256KB heap allocation in [`copy_file_contents_readwrite`]
/// by reusing a buffer from the engine's buffer pool.
fn copy_file_contents_readwrite_with_buffer(
    source: &File,
    destination: &File,
    length: u64,
    buffer: &mut [u8],
) -> io::Result<u64> {
    use std::io::{Read, Write};

    let mut source = source;
    let mut destination = destination;
    let mut total = 0u64;
    let mut remaining = length;

    while remaining > 0 {
        let to_read = (remaining as usize).min(buffer.len());
        let n = source.read(&mut buffer[..to_read])?;
        if n == 0 {
            break;
        }
        destination.write_all(&buffer[..n])?;
        total += n as u64;
        remaining -= n as u64;
    }

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
        let size = 128 * 1024; // 128KB - exceeds COPY_FILE_RANGE_THRESHOLD
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
        // Tiered dispatch and the read/write fallback must produce byte-identical
        // output for the same input - regression guard against silent data drift.
        let size = 256 * 1024; // 256KB - forces copy_file_range attempt
        let content: Vec<u8> = (0..size).map(|i| ((i * 7 + 13) % 256) as u8).collect();

        let source1 = create_temp_file(&content).unwrap();
        let mut dest1 = NamedTempFile::new().unwrap();
        let copied1 = copy_file_contents(source1.as_file(), dest1.as_file(), size as u64).unwrap();
        dest1.seek(SeekFrom::Start(0)).unwrap();
        let result1 = read_file_contents(dest1.as_file()).unwrap();

        let source2 = create_temp_file(&content).unwrap();
        let mut dest2 = NamedTempFile::new().unwrap();
        let copied2 =
            copy_file_contents_readwrite(source2.as_file(), dest2.as_file(), size as u64).unwrap();
        dest2.seek(SeekFrom::Start(0)).unwrap();
        let result2 = read_file_contents(dest2.as_file()).unwrap();

        assert_eq!(copied1, size as u64);
        assert_eq!(copied2, size as u64);
        assert_eq!(result1, result2);
        assert_eq!(result1, content);
    }

    #[test]
    fn test_large_file_multi_chunk() {
        // 2MB exercises the multi-iteration loop in try_copy_file_range, where the
        // kernel typically returns short copies and the caller must continue.
        let size = 2 * 1024 * 1024;
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
        // Verifies that copy_file_contents honours the source's current file
        // position rather than implicitly seeking to zero - critical because
        // copy_file_range advances the source offset in place.
        let content = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ";
        let mut source = create_temp_file(content).unwrap();
        let mut dest = NamedTempFile::new().unwrap();

        source.seek(SeekFrom::Start(10)).unwrap();

        let copied = copy_file_contents(source.as_file(), dest.as_file(), 10).unwrap();

        assert_eq!(copied, 10);
        dest.seek(SeekFrom::Start(0)).unwrap();
        let dest_content = read_file_contents(dest.as_file()).unwrap();
        assert_eq!(dest_content, b"ABCDEFGHIJ");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn test_try_copy_file_range_linux() {
        // Same-filesystem temp files succeed on kernel 4.5+. Older kernels and
        // cross-filesystem cases (kernel < 5.3) return an error, which is the
        // signal the production code uses to fall back to read/write.
        let content = b"Testing copy_file_range syscall directly";
        let source = create_temp_file(content).unwrap();
        let mut dest = NamedTempFile::new().unwrap();

        match try_copy_file_range(source.as_file(), dest.as_file(), content.len() as u64) {
            Ok(copied) => {
                assert_eq!(copied, content.len() as u64);
                dest.seek(SeekFrom::Start(0)).unwrap();
                let dest_content = read_file_contents(dest.as_file()).unwrap();
                assert_eq!(dest_content, content);
            }
            Err(_) => {
                eprintln!("copy_file_range not available, fallback will be used");
            }
        }
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn test_try_copy_file_range_non_linux() {
        let content = b"Test";
        let source = create_temp_file(content).unwrap();
        let dest = NamedTempFile::new().unwrap();

        let result = try_copy_file_range(source.as_file(), dest.as_file(), content.len() as u64);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::Unsupported);
    }

    #[cfg(all(target_os = "linux", feature = "io_uring"))]
    #[test]
    fn test_try_io_uring_copy_linux() {
        // Succeeds on kernels with io_uring (5.6+); older kernels return Err so
        // production callers can fall back. Either outcome is acceptable here.
        let size = 512 * 1024; // 512KB - above IO_URING_COPY_THRESHOLD
        let content: Vec<u8> = (0..size).map(|i| (i % 256) as u8).collect();
        let source = create_temp_file(&content).unwrap();
        let mut dest = NamedTempFile::new().unwrap();

        match try_io_uring_copy(source.as_file(), dest.as_file(), size as u64) {
            Ok(copied) => {
                assert_eq!(copied, size as u64);
                dest.seek(SeekFrom::Start(0)).unwrap();
                let dest_content = read_file_contents(dest.as_file()).unwrap();
                assert_eq!(dest_content, content);
            }
            Err(_) => {
                eprintln!("io_uring not available, fallback will be used");
            }
        }
    }

    #[cfg(not(all(target_os = "linux", feature = "io_uring")))]
    #[test]
    fn test_try_io_uring_copy_stub() {
        let content = b"Test io_uring stub";
        let source = create_temp_file(content).unwrap();
        let dest = NamedTempFile::new().unwrap();

        let result = try_io_uring_copy(source.as_file(), dest.as_file(), content.len() as u64);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::Unsupported);
    }

    #[test]
    fn test_buffered_copy_above_io_uring_threshold() {
        // 512KB exceeds IO_URING_COPY_THRESHOLD so the buffered path exercises
        // tier 1 (io_uring), then 2 (copy_file_range), then 3 (read/write).
        let size = 512 * 1024;
        let content: Vec<u8> = (0..size).map(|i| (i % 256) as u8).collect();
        let source = create_temp_file(&content).unwrap();
        let mut dest = NamedTempFile::new().unwrap();
        let mut buffer = vec![0u8; 256 * 1024];

        let copied =
            copy_file_contents_buffered(source.as_file(), dest.as_file(), size as u64, &mut buffer)
                .unwrap();

        assert_eq!(copied, size as u64);
        dest.seek(SeekFrom::Start(0)).unwrap();
        let dest_content = read_file_contents(dest.as_file()).unwrap();
        assert_eq!(dest_content, content);
    }
}
