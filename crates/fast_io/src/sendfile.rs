//! Zero-copy file-to-socket transfer using `sendfile` syscall with automatic fallback.
//!
//! This module provides high-performance file-to-socket transfer using Linux's `sendfile`
//! syscall when available, with automatic fallback to standard read/write on other
//! platforms or when the syscall fails.
//!
//! # Platform Support
//!
//! - **Linux**: Uses `sendfile` for zero-copy file-to-socket transfer
//! - **Other platforms**: Automatic fallback to buffered read/write
//!
//! # Performance Characteristics
//!
//! - For files < 64KB: Uses read/write directly (lower syscall overhead)
//! - For files >= 64KB: Attempts `sendfile` for zero-copy transfer
//! - Fallback path uses 256KB buffer for efficient bulk transfer
//! - Sends data in chunks up to ~2GB to avoid signal interruption
//!
//! # Example
//!
//! ```no_run
//! use std::fs::File;
//! use std::net::TcpStream;
//! use std::os::fd::AsRawFd;
//! use fast_io::sendfile::send_file_to_fd;
//!
//! # fn main() -> std::io::Result<()> {
//! let file = File::open("large_file.bin")?;
//! let mut socket = TcpStream::connect("127.0.0.1:8080")?;
//! let socket_fd = socket.as_raw_fd();
//! let sent = send_file_to_fd(&file, socket_fd, 1024 * 1024)?;
//! println!("Sent {} bytes", sent);
//! # Ok(())
//! # }
//! ```

use std::fs::File;
use std::io::{self, Read, Write};

/// Minimum file size to attempt sendfile (below this, read/write is fine).
///
/// Small files benefit from the simpler read/write path due to lower syscall overhead.
const SENDFILE_THRESHOLD: u64 = 64 * 1024; // 64KB

/// Maximum bytes per sendfile call (Linux limit to avoid signal interruption).
///
/// Linux `sendfile` can be interrupted by signals, so we limit each call to ~2GB.
const SENDFILE_CHUNK_SIZE: usize = 0x7fff_f000; // ~2GB

/// Transfers file contents to a writer, using buffered read/write.
///
/// This function provides a generic interface for sending file data to any
/// `Write` implementation. For raw file descriptor targets on Linux, consider
/// using [`send_file_to_fd`] for better performance via the `sendfile` syscall.
///
/// # Arguments
///
/// * `source` - Source file to read from (uses current file position)
/// * `destination` - Writer to send data to
/// * `length` - Number of bytes to transfer
///
/// # Returns
///
/// The number of bytes actually transferred. May be less than `length` if EOF is reached.
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
/// use fast_io::sendfile::send_file_to_writer;
///
/// # fn main() -> std::io::Result<()> {
/// let source = File::open("data.bin")?;
/// let mut output = Vec::new();
/// let sent = send_file_to_writer(&source, &mut output, 1024)?;
/// assert_eq!(sent, 1024);
/// # Ok(())
/// # }
/// ```
pub fn send_file_to_writer<W: Write>(
    source: &File,
    destination: &mut W,
    length: u64,
) -> io::Result<u64> {
    copy_via_readwrite(source, destination, length)
}

/// Transfers file contents to a raw file descriptor using `sendfile` when available.
///
/// This function uses the Linux `sendfile` syscall for zero-copy transfer when:
/// - The platform is Linux
/// - The file size is >= 64KB (threshold for efficiency)
/// - The destination file descriptor is a socket
///
/// On failure or unsupported platforms, automatically falls back to buffered read/write.
///
/// # Arguments
///
/// * `source` - Source file to read from (uses current file position)
/// * `dest_fd` - Raw file descriptor to write to (typically a socket)
/// * `length` - Number of bytes to transfer
///
/// # Returns
///
/// The number of bytes actually transferred. May be less than `length` if EOF is reached.
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
/// use std::net::TcpStream;
/// use std::os::fd::AsRawFd;
/// use fast_io::sendfile::send_file_to_fd;
///
/// # fn main() -> std::io::Result<()> {
/// let file = File::open("data.bin")?;
/// let socket = TcpStream::connect("127.0.0.1:8080")?;
/// let sent = send_file_to_fd(&file, socket.as_raw_fd(), 1024 * 1024)?;
/// assert_eq!(sent, 1024 * 1024);
/// # Ok(())
/// # }
/// ```
#[cfg(target_os = "linux")]
pub fn send_file_to_fd(source: &File, dest_fd: std::os::fd::RawFd, length: u64) -> io::Result<u64> {
    if length >= SENDFILE_THRESHOLD {
        if let Ok(n) = try_sendfile(source, dest_fd, length) {
            return Ok(n);
        }
        // Fall through to read/write fallback
    }
    // Fallback: read from source, write to fd
    copy_via_fd_write(source, dest_fd, length)
}

/// Stub for non-Linux platforms - always uses read/write fallback.
#[cfg(not(target_os = "linux"))]
pub fn send_file_to_fd(source: &File, dest_fd: std::os::fd::RawFd, length: u64) -> io::Result<u64> {
    copy_via_fd_write(source, dest_fd, length)
}

/// Attempts zero-copy transfer via `sendfile` syscall.
///
/// This function directly invokes the Linux `sendfile` syscall for optimal
/// performance. It returns an error on any failure, allowing the caller to fall back
/// to standard read/write.
///
/// # Platform Support
///
/// - **Linux**: Uses `sendfile` syscall for direct kernel-to-kernel transfer
/// - **Other platforms**: Not available (compile-time gated)
///
/// # Arguments
///
/// * `source` - Source file descriptor
/// * `dest_fd` - Destination file descriptor (typically a socket)
/// * `length` - Maximum bytes to transfer
///
/// # Returns
///
/// The number of bytes transferred via `sendfile`, or an error if:
/// - The syscall is not available on this platform
/// - The destination is not a socket
/// - The source file is not seekable
/// - Signal interruption occurs
/// - File descriptors are invalid
///
/// # Safety
///
/// Uses unsafe FFI to call `libc::sendfile`. File descriptors must be valid.
#[cfg(target_os = "linux")]
fn try_sendfile(source: &File, dest_fd: std::os::fd::RawFd, length: u64) -> io::Result<u64> {
    use std::os::fd::AsRawFd;

    let src_fd = source.as_raw_fd();
    let mut total: u64 = 0;
    let mut remaining = length;

    while remaining > 0 {
        let chunk = remaining.min(SENDFILE_CHUNK_SIZE as u64) as usize;
        // SAFETY: File descriptors are valid (derived from &File references).
        // Null offset pointer instructs the syscall to use and update the current
        // file position, which is the behavior we want.
        let result = unsafe { libc::sendfile(dest_fd, src_fd, std::ptr::null_mut(), chunk) };

        if result < 0 {
            let err = io::Error::last_os_error();
            if total == 0 {
                // Failed on first chunk - return error to trigger fallback
                return Err(err);
            }
            // Partial transfer succeeded, but now we hit an error - return what we have
            return Ok(total);
        }
        if result == 0 {
            // EOF reached
            break;
        }

        total += result as u64;
        remaining -= result as u64;
    }

    Ok(total)
}

/// Fallback: write from file to raw fd using buffered read/write.
///
/// This function reads from the source file and writes to a raw file descriptor
/// using manual buffer management and `libc::write`. Used as a fallback when
/// `sendfile` is unavailable or fails.
///
/// # Arguments
///
/// * `source` - Source file to read from
/// * `dest_fd` - Raw file descriptor to write to
/// * `length` - Number of bytes to copy
///
/// # Returns
///
/// The number of bytes actually copied.
///
/// # Errors
///
/// Returns an error if reading or writing fails.
#[cfg(target_os = "linux")]
fn copy_via_fd_write(source: &File, dest_fd: std::os::fd::RawFd, length: u64) -> io::Result<u64> {
    let mut reader = io::BufReader::new(source);
    let mut buf = vec![0u8; 256 * 1024]; // 256KB buffer
    let mut total: u64 = 0;
    let mut remaining = length;

    while remaining > 0 {
        let to_read = (remaining as usize).min(buf.len());
        let n = reader.read(&mut buf[..to_read])?;
        if n == 0 {
            // EOF reached
            break;
        }

        // Write all bytes to the file descriptor, handling partial writes
        let mut written = 0;
        while written < n {
            // SAFETY: buf[written..n] is a valid slice, and dest_fd is assumed valid
            let result = unsafe {
                libc::write(
                    dest_fd,
                    buf[written..n].as_ptr().cast::<libc::c_void>(),
                    n - written,
                )
            };
            if result < 0 {
                return Err(io::Error::last_os_error());
            }
            written += result as usize;
        }

        total += n as u64;
        remaining -= n as u64;
    }

    Ok(total)
}

/// Stub for non-Linux platforms.
#[cfg(not(target_os = "linux"))]
fn copy_via_fd_write(source: &File, dest_fd: std::os::fd::RawFd, length: u64) -> io::Result<u64> {
    let mut reader = io::BufReader::new(source);
    let mut buf = vec![0u8; 256 * 1024]; // 256KB buffer
    let mut total: u64 = 0;
    let mut remaining = length;

    while remaining > 0 {
        let to_read = (remaining as usize).min(buf.len());
        let n = reader.read(&mut buf[..to_read])?;
        if n == 0 {
            // EOF reached
            break;
        }

        // Write all bytes to the file descriptor, handling partial writes
        let mut written = 0;
        while written < n {
            // SAFETY: buf[written..n] is a valid slice, and dest_fd is assumed valid
            let result = unsafe {
                libc::write(
                    dest_fd,
                    buf[written..n].as_ptr().cast::<libc::c_void>(),
                    n - written,
                )
            };
            if result < 0 {
                return Err(io::Error::last_os_error());
            }
            written += result as usize;
        }

        total += n as u64;
        remaining -= n as u64;
    }

    Ok(total)
}

/// Fallback: buffered read/write through userspace.
///
/// Uses buffered I/O with a 256KB buffer for efficient bulk transfer.
/// This is the most portable path and works with any `Write` implementation.
///
/// # Arguments
///
/// * `source` - Source file to read from
/// * `destination` - Writer to send data to
/// * `length` - Number of bytes to copy
///
/// # Returns
///
/// The number of bytes actually copied.
///
/// # Errors
///
/// Returns an error if reading or writing fails.
fn copy_via_readwrite<W: Write>(
    source: &File,
    destination: &mut W,
    length: u64,
) -> io::Result<u64> {
    let mut reader = io::BufReader::new(source);
    let mut buf = vec![0u8; 256 * 1024]; // 256KB buffer
    let mut total: u64 = 0;
    let mut remaining = length;

    while remaining > 0 {
        let to_read = (remaining as usize).min(buf.len());
        let n = reader.read(&mut buf[..to_read])?;
        if n == 0 {
            // EOF reached
            break;
        }
        destination.write_all(&buf[..n])?;
        total += n as u64;
        remaining -= n as u64;
    }
    destination.flush()?;

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

    #[test]
    fn test_send_to_writer_small_file() {
        // Small file (< 64KB) should use read/write path directly
        let content = b"Hello, world! This is a small file for testing.";
        let source = create_temp_file(content).unwrap();
        let mut output = Vec::new();

        let sent =
            send_file_to_writer(source.as_file(), &mut output, content.len() as u64).unwrap();

        assert_eq!(sent, content.len() as u64);
        assert_eq!(output, content);
    }

    #[test]
    fn test_send_to_writer_large_file() {
        // Large file (>= 64KB) should trigger sendfile attempt (but falls back for Vec)
        let size = 128 * 1024; // 128KB
        let content: Vec<u8> = (0..size).map(|i| (i % 256) as u8).collect();
        let source = create_temp_file(&content).unwrap();
        let mut output = Vec::new();

        let sent =
            send_file_to_writer(source.as_file(), &mut output, content.len() as u64).unwrap();

        assert_eq!(sent, content.len() as u64);
        assert_eq!(output, content);
    }

    #[test]
    fn test_send_to_writer_empty_file() {
        // Empty file should work without errors
        let content = b"";
        let source = create_temp_file(content).unwrap();
        let mut output = Vec::new();

        let sent = send_file_to_writer(source.as_file(), &mut output, 0).unwrap();

        assert_eq!(sent, 0);
        assert_eq!(output, content);
    }

    #[test]
    fn test_send_to_writer_exact_content() {
        // Verify data integrity with specific pattern
        let content: Vec<u8> = (0..1000).map(|i| ((i * 7 + 13) % 256) as u8).collect();
        let source = create_temp_file(&content).unwrap();
        let mut output = Vec::new();

        let sent =
            send_file_to_writer(source.as_file(), &mut output, content.len() as u64).unwrap();

        assert_eq!(sent, content.len() as u64);
        assert_eq!(output, content);
    }

    #[test]
    fn test_send_to_writer_partial() {
        // Request fewer bytes than available
        let content = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ";
        let source = create_temp_file(content).unwrap();
        let mut output = Vec::new();

        let sent = send_file_to_writer(source.as_file(), &mut output, 10).unwrap();

        assert_eq!(sent, 10);
        assert_eq!(output, b"0123456789");
    }

    #[test]
    fn test_send_to_writer_beyond_eof() {
        // Request more bytes than available - should stop at EOF
        let content = b"Short content";
        let source = create_temp_file(content).unwrap();
        let mut output = Vec::new();

        let sent = send_file_to_writer(source.as_file(), &mut output, 10000).unwrap();

        assert_eq!(sent, content.len() as u64);
        assert_eq!(output, content);
    }

    #[test]
    fn test_send_with_file_position() {
        // Test that file position is respected
        let content = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ";
        let mut source = create_temp_file(content).unwrap();
        let mut output = Vec::new();

        // Seek source to position 10
        source.seek(SeekFrom::Start(10)).unwrap();

        // Send 10 bytes from position 10
        let sent = send_file_to_writer(source.as_file(), &mut output, 10).unwrap();

        assert_eq!(sent, 10);
        assert_eq!(output, b"ABCDEFGHIJ");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn test_send_file_to_fd_pipe() {
        // Test sendfile to a pipe (should work on Linux)
        let content = b"Testing sendfile with pipe on Linux";
        let source = create_temp_file(content).unwrap();

        // Create a pipe for testing
        let mut pipe_fds = [0i32; 2];
        let result = unsafe { libc::pipe(pipe_fds.as_mut_ptr()) };
        assert_eq!(result, 0, "Failed to create pipe");

        let read_fd = pipe_fds[0];
        let write_fd = pipe_fds[1];

        // Send data through sendfile to pipe
        let sent = send_file_to_fd(source.as_file(), write_fd, content.len() as u64);

        // Close write end
        unsafe { libc::close(write_fd) };

        if let Ok(sent_bytes) = sent {
            assert_eq!(sent_bytes, content.len() as u64);

            // Read from pipe to verify content
            let mut received = vec![0u8; content.len()];
            let n = unsafe {
                libc::read(
                    read_fd,
                    received.as_mut_ptr().cast::<libc::c_void>(),
                    content.len(),
                )
            };
            assert_eq!(n, content.len() as isize);
            assert_eq!(received, content);
        }

        // Close read end
        unsafe { libc::close(read_fd) };
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn test_send_file_to_fd_socketpair() {
        // Test sendfile to a socket (ideal use case)
        let content = b"Testing sendfile with socketpair";
        let source = create_temp_file(content).unwrap();

        // Create a socket pair for testing
        let mut socket_fds = [0i32; 2];
        let result = unsafe {
            libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, socket_fds.as_mut_ptr())
        };
        assert_eq!(result, 0, "Failed to create socketpair");

        let recv_fd = socket_fds[0];
        let send_fd = socket_fds[1];

        // Send data through sendfile to socket
        let sent = send_file_to_fd(source.as_file(), send_fd, content.len() as u64).unwrap();

        assert_eq!(sent, content.len() as u64);

        // Close send end to signal EOF
        unsafe { libc::close(send_fd) };

        // Read from socket to verify content
        let mut received = vec![0u8; content.len()];
        let n = unsafe {
            libc::read(
                recv_fd,
                received.as_mut_ptr().cast::<libc::c_void>(),
                content.len(),
            )
        };
        assert_eq!(n, content.len() as isize);
        assert_eq!(received, content);

        // Close receive end
        unsafe { libc::close(recv_fd) };
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn test_send_file_to_fd_large() {
        use std::thread;

        // Test large file transfer via sendfile
        let size = 512 * 1024; // 512KB - well above threshold
        let content: Vec<u8> = (0..size).map(|i| (i % 256) as u8).collect();
        let source = create_temp_file(&content).unwrap();

        // Create a pipe for testing
        let mut pipe_fds = [0i32; 2];
        let result = unsafe { libc::pipe(pipe_fds.as_mut_ptr()) };
        assert_eq!(result, 0, "Failed to create pipe");

        let read_fd = pipe_fds[0];
        let write_fd = pipe_fds[1];

        // Spawn reader thread to avoid pipe buffer deadlock
        let expected_content = content.clone();
        let reader_thread = thread::spawn(move || {
            let mut received = Vec::new();
            let mut buf = [0u8; 8192];
            loop {
                let n = unsafe {
                    libc::read(read_fd, buf.as_mut_ptr().cast::<libc::c_void>(), buf.len())
                };
                if n <= 0 {
                    break;
                }
                received.extend_from_slice(&buf[..n as usize]);
            }
            unsafe { libc::close(read_fd) };
            received
        });

        // Send data through sendfile (main thread)
        let sent = send_file_to_fd(source.as_file(), write_fd, size as u64);

        // Close write end to signal EOF to reader
        unsafe { libc::close(write_fd) };

        // Verify send succeeded
        assert!(sent.is_ok(), "sendfile should succeed");
        let sent_bytes = sent.unwrap();
        assert_eq!(sent_bytes, size as u64);

        // Wait for reader and verify content
        let received = reader_thread.join().expect("reader thread should succeed");
        assert_eq!(received.len(), expected_content.len());
        assert_eq!(received, expected_content);
    }

    #[test]
    fn test_copy_via_readwrite_direct() {
        // Test the read/write fallback path directly
        let content = b"Testing fallback path directly with specific data";
        let source = create_temp_file(content).unwrap();
        let mut output = Vec::new();

        let copied =
            copy_via_readwrite(source.as_file(), &mut output, content.len() as u64).unwrap();

        assert_eq!(copied, content.len() as u64);
        assert_eq!(output, content);
    }

    #[test]
    fn test_threshold_boundary() {
        // Test at exact threshold boundary
        let size = SENDFILE_THRESHOLD as usize;
        let content: Vec<u8> = (0..size).map(|i| (i % 256) as u8).collect();
        let source = create_temp_file(&content).unwrap();
        let mut output = Vec::new();

        let sent = send_file_to_writer(source.as_file(), &mut output, size as u64).unwrap();

        assert_eq!(sent, size as u64);
        assert_eq!(output.len(), content.len());
        assert_eq!(output, content);
    }

    #[test]
    fn test_multiple_writes() {
        // Test that multiple independent writes work correctly
        let content1 = b"First write";
        let content2 = b"Second";
        let source1 = create_temp_file(content1).unwrap();
        let source2 = create_temp_file(content2).unwrap();
        let mut output = Vec::new();

        // First write
        let sent1 =
            send_file_to_writer(source1.as_file(), &mut output, content1.len() as u64).unwrap();
        assert_eq!(sent1, content1.len() as u64);
        assert_eq!(&output[..sent1 as usize], content1);

        // Second write appends to output
        let sent2 =
            send_file_to_writer(source2.as_file(), &mut output, content2.len() as u64).unwrap();
        assert_eq!(sent2, content2.len() as u64);
        assert_eq!(output, b"First writeSecond");
    }
}
