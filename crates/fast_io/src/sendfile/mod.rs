//! Zero-copy file-to-socket transfer using `sendfile` syscall with automatic fallback.
//!
//! This module provides high-performance file-to-socket transfer using the
//! native `sendfile` syscall on Linux and macOS, with automatic fallback to
//! standard read/write on other platforms or when the syscall fails.
//!
//! # Platform Support
//!
//! - **Linux**: Uses Linux's `sendfile(out, in, offset, count)` for
//!   zero-copy file-to-socket transfer.
//! - **macOS**: Uses Darwin's `sendfile(fd, s, offset, &len, hdtr, flags)`.
//!   The signature differs from Linux: `len` is an in/out parameter (bytes
//!   to send -> bytes actually sent) and a return of `0` means success.
//! - **Other platforms**: Automatic fallback to buffered read/write.
//!
//! # Performance Characteristics
//!
//! - For files < 64KB: Uses read/write directly (lower syscall overhead)
//! - For files >= 64KB: Attempts `sendfile` for zero-copy transfer
//! - Fallback path uses 256KB buffer for efficient bulk transfer
//! - On Linux, sends data in chunks up to ~2GB to avoid signal interruption
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

use std::fs::File;
use std::io::{self, Write};

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;

mod fallback;

#[cfg(target_os = "linux")]
use linux::try_sendfile;
#[cfg(target_os = "macos")]
use macos::try_sendfile_macos;

#[cfg(unix)]
use fallback::copy_via_fd_write;
use fallback::copy_via_readwrite;

/// Minimum file size to attempt sendfile (below this, read/write is fine).
///
/// Small files benefit from the simpler read/write path due to lower syscall overhead.
#[cfg(any(target_os = "linux", target_os = "macos"))]
const SENDFILE_THRESHOLD: u64 = 64 * 1024;

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
///
/// Since Linux 2.6.33 the destination fd may be any file, so no socket
/// check is performed; the syscall is attempted whenever the threshold is met.
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
        if let Ok(n) = try_sendfile(source, dest_fd, length) {
            return Ok(n);
        }
    }
    copy_via_fd_write(source, dest_fd, length)
}

/// macOS dispatch - prefers Darwin's native `sendfile(2)` for sockets.
///
/// Above `SENDFILE_THRESHOLD` the function attempts a zero-copy transfer
/// via `try_sendfile_macos`. The Darwin `sendfile` only accepts socket
/// destinations, so a non-socket `dest_fd` (pipe, regular file) falls back
/// to the buffered `read`/`write` loop transparently.
#[cfg(target_os = "macos")]
pub fn send_file_to_fd(source: &File, dest_fd: i32, length: u64) -> io::Result<u64> {
    if length >= SENDFILE_THRESHOLD {
        if let Ok(n) = try_sendfile_macos(source, dest_fd, length) {
            return Ok(n);
        }
    }
    copy_via_fd_write(source, dest_fd, length)
}

/// Stub for other unix platforms (BSD, illumos, ...) - uses libc::write fallback.
#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
pub fn send_file_to_fd(source: &File, dest_fd: i32, length: u64) -> io::Result<u64> {
    copy_via_fd_write(source, dest_fd, length)
}

/// Stub for non-unix platforms - raw fd is not meaningful.
#[cfg(not(unix))]
pub fn send_file_to_fd(source: &File, _dest_fd: i32, length: u64) -> io::Result<u64> {
    send_file_to_writer(source, &mut io::sink(), length)
}

/// Policy-aware variant of [`send_file_to_fd`].
///
/// When `policy` is [`ZeroCopyPolicy::Disabled`](crate::ZeroCopyPolicy::Disabled),
/// the `sendfile(2)` fast path is skipped and the function falls through to
/// a buffered `read`/`write` loop. `Auto` and `Enabled` route to the
/// existing [`send_file_to_fd`] which auto-selects `sendfile` for transfers
/// above the platform threshold.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub fn send_file_to_fd_with_policy(
    source: &File,
    dest_fd: i32,
    length: u64,
    policy: crate::ZeroCopyPolicy,
) -> io::Result<u64> {
    if matches!(policy, crate::ZeroCopyPolicy::Disabled) {
        copy_via_fd_write(source, dest_fd, length)
    } else {
        send_file_to_fd(source, dest_fd, length)
    }
}

/// Other unix policy-aware fallback - delegates to [`send_file_to_fd`].
///
/// `sendfile(2)` is not used on non-Linux, non-macOS unix targets, so the
/// policy is purely informational here and the call always uses the
/// standard `read`/`write` path.
#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
pub fn send_file_to_fd_with_policy(
    source: &File,
    dest_fd: i32,
    length: u64,
    _policy: crate::ZeroCopyPolicy,
) -> io::Result<u64> {
    send_file_to_fd(source, dest_fd, length)
}

/// Non-unix policy-aware stub for `send_file_to_fd_with_policy`.
#[cfg(not(unix))]
pub fn send_file_to_fd_with_policy(
    source: &File,
    _dest_fd: i32,
    length: u64,
    _policy: crate::ZeroCopyPolicy,
) -> io::Result<u64> {
    send_file_to_writer(source, &mut io::sink(), length)
}

#[cfg(test)]
mod tests;
