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
//! # #[cfg(unix)]
//! use std::os::fd::AsRawFd;
//! use fast_io::sendfile::send_file_to_fd;
//!
//! # fn main() -> std::io::Result<()> {
//! let file = File::open("large_file.bin")?;
//! let mut socket = TcpStream::connect("127.0.0.1:8080")?;
//! # #[cfg(unix)]
//! let socket_fd = socket.as_raw_fd();
//! # #[cfg(unix)]
//! let sent = send_file_to_fd(&file, socket_fd, 1024 * 1024)?;
//! # #[cfg(unix)]
//! println!("Sent {} bytes", sent);
//! # Ok(())
//! # }
//! ```

mod fallback;
#[cfg(target_os = "linux")]
mod syscall;

use std::fs::File;
use std::io::{self, Write};

/// Minimum file size to attempt sendfile (below this, read/write is fine).
///
/// Small files benefit from the simpler read/write path due to lower syscall overhead.
#[cfg(target_os = "linux")]
const SENDFILE_THRESHOLD: u64 = 64 * 1024; // 64KB

/// Maximum bytes per sendfile call (Linux limit to avoid signal interruption).
///
/// Linux `sendfile` can be interrupted by signals, so we limit each call to ~2GB.
#[cfg(target_os = "linux")]
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
    fallback::copy_via_readwrite(source, destination, length)
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
/// # #[cfg(unix)]
/// use std::os::fd::AsRawFd;
/// use fast_io::sendfile::send_file_to_fd;
///
/// # fn main() -> std::io::Result<()> {
/// let file = File::open("data.bin")?;
/// let socket = TcpStream::connect("127.0.0.1:8080")?;
/// # #[cfg(unix)]
/// let sent = send_file_to_fd(&file, socket.as_raw_fd(), 1024 * 1024)?;
/// # #[cfg(unix)]
/// assert_eq!(sent, 1024 * 1024);
/// # Ok(())
/// # }
/// ```
#[cfg(target_os = "linux")]
pub fn send_file_to_fd(source: &File, dest_fd: i32, length: u64) -> io::Result<u64> {
    if length >= SENDFILE_THRESHOLD {
        if let Ok(n) = syscall::try_sendfile(source, dest_fd, length) {
            return Ok(n);
        }
        // Fall through to read/write fallback
    }
    // Fallback: read from source, write to fd
    fallback::copy_via_fd_write(source, dest_fd, length)
}

/// Stub for non-Linux unix platforms -- uses libc::write fallback.
#[cfg(all(unix, not(target_os = "linux")))]
pub fn send_file_to_fd(source: &File, dest_fd: i32, length: u64) -> io::Result<u64> {
    fallback::copy_via_fd_write(source, dest_fd, length)
}

/// Stub for non-unix platforms -- raw fd is not meaningful.
#[cfg(not(unix))]
pub fn send_file_to_fd(source: &File, _dest_fd: i32, length: u64) -> io::Result<u64> {
    send_file_to_writer(source, &mut io::sink(), length)
}

#[cfg(test)]
mod tests;
