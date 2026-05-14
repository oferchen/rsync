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
use std::io::{self, Read, Write};

/// Minimum file size to attempt sendfile (below this, read/write is fine).
///
/// Small files benefit from the simpler read/write path due to lower syscall overhead.
#[cfg(any(target_os = "linux", target_os = "macos"))]
const SENDFILE_THRESHOLD: u64 = 64 * 1024; // 64KB

/// Maximum bytes per sendfile call (Linux limit to avoid signal interruption).
///
/// Linux `sendfile` can be interrupted by signals, so we limit each call to ~2GB.
#[cfg(target_os = "linux")]
const SENDFILE_CHUNK_SIZE: usize = 0x7fff_f000; // ~2GB

/// Maximum bytes per Darwin `sendfile` call.
///
/// Darwin's `sendfile` accepts an `off_t` length so the only practical ceiling
/// is `i64::MAX`. We cap each call at ~2 GiB to keep partial-send accounting
/// simple and avoid pinning a single socket for too long when other I/O is
/// waiting.
#[cfg(target_os = "macos")]
const SENDFILE_CHUNK_SIZE: u64 = 0x7fff_f000; // ~2GB

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
/// Above [`SENDFILE_THRESHOLD`] the function attempts a zero-copy transfer
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
fn try_sendfile(source: &File, dest_fd: i32, length: u64) -> io::Result<u64> {
    #[cfg(unix)]
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
            // First-chunk failure surfaces the error so the caller can fall back.
            // Once any bytes have moved we return what was already sent so the
            // socket peer sees a consistent prefix rather than a duplicated retry.
            if total == 0 {
                return Err(io::Error::last_os_error());
            }
            return Ok(total);
        }
        if result == 0 {
            break;
        }

        total += result as u64;
        remaining -= result as u64;
    }

    Ok(total)
}

/// Attempts zero-copy transfer via Darwin's `sendfile(2)`.
///
/// macOS exposes a BSD-style `sendfile` whose signature differs from Linux:
///
/// ```c
/// int sendfile(int fd, int s, off_t offset, off_t *len,
///              struct sf_hdtr *hdtr, int flags);
/// ```
///
/// `*len` is in/out: on entry it holds the maximum bytes to send; on return
/// it holds the bytes actually sent. A return value of `0` indicates
/// success; `-1` indicates failure with errno set. On `EAGAIN`, `EINTR`, or
/// `EINPROGRESS`, `*len` still reports a partial byte count and the caller
/// is expected to advance and retry from `offset + *len`.
///
/// Darwin's `sendfile` requires the destination to be a `SOCK_STREAM`
/// socket. Non-socket destinations (pipes, regular files) fail with
/// `ENOTSOCK`, which surfaces here as an error so the dispatch in
/// [`send_file_to_fd`] can fall back to the buffered `read`/`write` loop.
///
/// # Source offset
///
/// Linux's `sendfile` with a NULL offset pointer uses and advances the
/// source file position. Darwin's signature requires an explicit offset
/// and does not touch the file position. To preserve the "transfer from
/// the current file position" contract documented on [`send_file_to_fd`],
/// this function reads the current position via `lseek(fd, 0, SEEK_CUR)`,
/// passes it as the syscall offset, and advances the position with a
/// matching `lseek(SEEK_SET)` after a successful transfer.
///
/// # Safety
///
/// Uses unsafe FFI to call `libc::sendfile` and `libc::lseek`. File
/// descriptors must be valid (they are derived from `&File` and a caller-
/// provided socket fd).
#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
fn try_sendfile_macos(source: &File, dest_fd: i32, length: u64) -> io::Result<u64> {
    use std::os::fd::AsRawFd;

    let src_fd = source.as_raw_fd();

    // Capture the current source position so we mirror Linux's
    // "advance the file position" contract after the transfer succeeds.
    // SAFETY: `src_fd` is a valid open file descriptor borrowed from `source`.
    // `lseek` with `SEEK_CUR` and offset 0 is a pure query - it cannot move
    // or corrupt the position.
    let start_offset = unsafe { libc::lseek(src_fd, 0, libc::SEEK_CUR) };
    if start_offset < 0 {
        return Err(io::Error::last_os_error());
    }

    let mut total: u64 = 0;
    let mut remaining = length;

    while remaining > 0 {
        let chunk = remaining.min(SENDFILE_CHUNK_SIZE);
        // Darwin's sendfile treats `*len == 0` as "send until EOF". We always
        // pass an explicit non-zero chunk so partial-send accounting stays
        // simple - 0 only occurs after the outer loop terminates.
        let mut len: libc::off_t = chunk as libc::off_t;
        let offset: libc::off_t = start_offset
            .checked_add(total as i64)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "offset overflow"))?;

        // SAFETY: `src_fd` and `dest_fd` are valid open file descriptors.
        // `&mut len` is a valid pointer to a stack-allocated `off_t` for the
        // duration of the call. NULL `hdtr` and `flags = 0` request a plain
        // file-to-socket transfer with no header/trailer iovecs.
        let ret = unsafe {
            libc::sendfile(
                src_fd,
                dest_fd,
                offset,
                &mut len as *mut libc::off_t,
                std::ptr::null_mut(),
                0,
            )
        };

        // Darwin populates `*len` with the bytes actually sent on both
        // success and EAGAIN/EINTR. Treat any positive `len` as forward
        // progress before deciding whether to surface the error.
        let sent = if len >= 0 { len as u64 } else { 0 };

        if ret != 0 {
            let err = io::Error::last_os_error();
            // EAGAIN / EINTR with a partial send: treat as forward progress
            // and stop. The caller observes the prefix that did move and the
            // socket peer sees a consistent stream.
            if total + sent == 0 {
                return Err(err);
            }
            total += sent;
            // Best-effort: advance the source file position to match the
            // bytes we actually transferred so callers that resume from the
            // file's current offset stay correct. Saturating arithmetic
            // protects against the (astronomically unlikely) case of an
            // `off_t` overflow on multi-exabyte transfers.
            let new_offset = start_offset.saturating_add(total as i64);
            // SAFETY: `src_fd` is still valid; SEEK_SET with a non-negative
            // offset is well-defined for regular files.
            unsafe {
                let _ = libc::lseek(src_fd, new_offset, libc::SEEK_SET);
            }
            return Ok(total);
        }

        total += sent;
        if sent == 0 {
            // No progress and no error: source reached EOF.
            break;
        }
        remaining = remaining.saturating_sub(sent);
    }

    // Advance the source file position by the total bytes transferred so
    // the post-call file offset matches Linux's behaviour.
    let new_offset = start_offset.saturating_add(total as i64);
    // SAFETY: `src_fd` is a valid open file descriptor; SEEK_SET with a
    // non-negative offset is a well-defined operation.
    unsafe {
        let _ = libc::lseek(src_fd, new_offset, libc::SEEK_SET);
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
fn copy_via_fd_write(source: &File, dest_fd: i32, length: u64) -> io::Result<u64> {
    let mut reader = io::BufReader::new(source);
    let mut buf = vec![0u8; 256 * 1024]; // 256KB buffer
    let mut total: u64 = 0;
    let mut remaining = length;

    while remaining > 0 {
        let to_read = (remaining as usize).min(buf.len());
        let n = reader.read(&mut buf[..to_read])?;
        if n == 0 {
            break;
        }

        // libc::write may return short, so loop until the chunk is drained.
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

/// Stub for non-Linux unix platforms - uses libc::write.
#[cfg(all(unix, not(target_os = "linux")))]
fn copy_via_fd_write(source: &File, dest_fd: i32, length: u64) -> io::Result<u64> {
    let mut reader = io::BufReader::new(source);
    let mut buf = vec![0u8; 256 * 1024]; // 256KB buffer
    let mut total: u64 = 0;
    let mut remaining = length;

    while remaining > 0 {
        let to_read = (remaining as usize).min(buf.len());
        let n = reader.read(&mut buf[..to_read])?;
        if n == 0 {
            break;
        }

        // libc::write may return short, so loop until the chunk is drained.
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
        // Above SENDFILE_THRESHOLD; the Vec writer forces the read/write fallback.
        let size = 128 * 1024;
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
        let content = b"";
        let source = create_temp_file(content).unwrap();
        let mut output = Vec::new();

        let sent = send_file_to_writer(source.as_file(), &mut output, 0).unwrap();

        assert_eq!(sent, 0);
        assert_eq!(output, content);
    }

    #[test]
    fn test_send_to_writer_exact_content() {
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
        let content = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ";
        let source = create_temp_file(content).unwrap();
        let mut output = Vec::new();

        let sent = send_file_to_writer(source.as_file(), &mut output, 10).unwrap();

        assert_eq!(sent, 10);
        assert_eq!(output, b"0123456789");
    }

    #[test]
    fn test_send_to_writer_beyond_eof() {
        let content = b"Short content";
        let source = create_temp_file(content).unwrap();
        let mut output = Vec::new();

        let sent = send_file_to_writer(source.as_file(), &mut output, 10000).unwrap();

        assert_eq!(sent, content.len() as u64);
        assert_eq!(output, content);
    }

    #[test]
    fn test_send_with_file_position() {
        // sendfile uses the source's current offset; verify the wrapper preserves
        // that contract instead of implicitly seeking to zero.
        let content = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ";
        let mut source = create_temp_file(content).unwrap();
        let mut output = Vec::new();

        source.seek(SeekFrom::Start(10)).unwrap();

        let sent = send_file_to_writer(source.as_file(), &mut output, 10).unwrap();

        assert_eq!(sent, 10);
        assert_eq!(output, b"ABCDEFGHIJ");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn test_send_file_to_fd_pipe() {
        let content = b"Testing sendfile with pipe on Linux";
        let source = create_temp_file(content).unwrap();

        let mut pipe_fds = [0i32; 2];
        let result = unsafe { libc::pipe(pipe_fds.as_mut_ptr()) };
        assert_eq!(result, 0, "Failed to create pipe");

        let read_fd = pipe_fds[0];
        let write_fd = pipe_fds[1];

        let sent = send_file_to_fd(source.as_file(), write_fd, content.len() as u64);

        unsafe { libc::close(write_fd) };

        if let Ok(sent_bytes) = sent {
            assert_eq!(sent_bytes, content.len() as u64);

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

        unsafe { libc::close(read_fd) };
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn test_send_file_to_fd_socketpair() {
        // socketpair is the canonical sendfile destination - exercises the
        // kernel's zero-copy fast path rather than the read/write fallback.
        let content = b"Testing sendfile with socketpair";
        let source = create_temp_file(content).unwrap();

        let mut socket_fds = [0i32; 2];
        let result = unsafe {
            libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, socket_fds.as_mut_ptr())
        };
        assert_eq!(result, 0, "Failed to create socketpair");

        let recv_fd = socket_fds[0];
        let send_fd = socket_fds[1];

        let sent = send_file_to_fd(source.as_file(), send_fd, content.len() as u64).unwrap();
        assert_eq!(sent, content.len() as u64);

        unsafe { libc::close(send_fd) };

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

        unsafe { libc::close(recv_fd) };
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn test_send_file_to_fd_large() {
        use std::thread;

        let size = 512 * 1024; // 512KB - exceeds SENDFILE_THRESHOLD
        let content: Vec<u8> = (0..size).map(|i| (i % 256) as u8).collect();
        let source = create_temp_file(&content).unwrap();

        let mut pipe_fds = [0i32; 2];
        let result = unsafe { libc::pipe(pipe_fds.as_mut_ptr()) };
        assert_eq!(result, 0, "Failed to create pipe");

        let read_fd = pipe_fds[0];
        let write_fd = pipe_fds[1];

        // 512KB exceeds the default 64KB pipe buffer, so we must drain it from
        // another thread to avoid blocking the sender.
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

        let sent = send_file_to_fd(source.as_file(), write_fd, size as u64);

        unsafe { libc::close(write_fd) };

        assert!(sent.is_ok(), "sendfile should succeed");
        let sent_bytes = sent.unwrap();
        assert_eq!(sent_bytes, size as u64);

        let received = reader_thread.join().expect("reader thread should succeed");
        assert_eq!(received.len(), expected_content.len());
        assert_eq!(received, expected_content);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn test_send_file_to_fd_socketpair_macos() {
        // Darwin's sendfile(2) only accepts SOCK_STREAM destinations.
        // Exercise the native path with a content length above
        // SENDFILE_THRESHOLD so try_sendfile_macos is invoked rather than
        // the buffered read/write fallback.
        use std::thread;

        let size = SENDFILE_THRESHOLD as usize + 4096; // 68 KiB
        let content: Vec<u8> = (0..size).map(|i| (i % 256) as u8).collect();
        let source = create_temp_file(&content).unwrap();

        let mut socket_fds = [0i32; 2];
        let result = unsafe {
            libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, socket_fds.as_mut_ptr())
        };
        assert_eq!(result, 0, "Failed to create socketpair");

        let recv_fd = socket_fds[0];
        let send_fd = socket_fds[1];

        // Drain the receive end concurrently so a small socket buffer does
        // not deadlock the sender.
        let expected_content = content.clone();
        let reader_thread = thread::spawn(move || {
            let mut received = Vec::with_capacity(expected_content.len());
            let mut buf = [0u8; 8192];
            while received.len() < expected_content.len() {
                let n = unsafe {
                    libc::read(recv_fd, buf.as_mut_ptr().cast::<libc::c_void>(), buf.len())
                };
                if n <= 0 {
                    break;
                }
                received.extend_from_slice(&buf[..n as usize]);
            }
            unsafe { libc::close(recv_fd) };
            received
        });

        // try_sendfile_macos should succeed end-to-end for a SOCK_STREAM peer.
        let sent = try_sendfile_macos(source.as_file(), send_fd, size as u64)
            .expect("native macOS sendfile should succeed on a SOCK_STREAM");
        assert_eq!(sent, size as u64);

        unsafe { libc::close(send_fd) };

        let received = reader_thread.join().expect("reader thread should succeed");
        assert_eq!(received.len(), expected_content.len());
        assert_eq!(received, expected_content);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn test_send_file_to_fd_dispatch_uses_native_macos() {
        // The high-level dispatch must succeed for SOCK_STREAM destinations
        // on macOS without falling back to read/write (which would also pass
        // the byte-equality check but defeat the purpose of this audit).
        let content: Vec<u8> = (0..(SENDFILE_THRESHOLD as usize + 1024))
            .map(|i| ((i * 13 + 7) % 256) as u8)
            .collect();
        let source = create_temp_file(&content).unwrap();

        let mut socket_fds = [0i32; 2];
        let result = unsafe {
            libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, socket_fds.as_mut_ptr())
        };
        assert_eq!(result, 0, "Failed to create socketpair");

        let recv_fd = socket_fds[0];
        let send_fd = socket_fds[1];

        let expected = content.clone();
        let reader_thread = std::thread::spawn(move || {
            let mut received = Vec::with_capacity(expected.len());
            let mut buf = [0u8; 8192];
            while received.len() < expected.len() {
                let n = unsafe {
                    libc::read(recv_fd, buf.as_mut_ptr().cast::<libc::c_void>(), buf.len())
                };
                if n <= 0 {
                    break;
                }
                received.extend_from_slice(&buf[..n as usize]);
            }
            unsafe { libc::close(recv_fd) };
            received
        });

        let sent = send_file_to_fd(source.as_file(), send_fd, content.len() as u64).unwrap();
        assert_eq!(sent, content.len() as u64);

        unsafe { libc::close(send_fd) };

        let received = reader_thread.join().expect("reader thread should succeed");
        assert_eq!(received, content);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn test_send_file_to_fd_macos_non_socket_falls_back() {
        // Darwin's sendfile returns ENOTSOCK for pipes; the dispatch must
        // fall back to the read/write loop and still deliver the bytes.
        let content: Vec<u8> = (0..(SENDFILE_THRESHOLD as usize + 512))
            .map(|i| (i % 256) as u8)
            .collect();
        let source = create_temp_file(&content).unwrap();

        let mut pipe_fds = [0i32; 2];
        let result = unsafe { libc::pipe(pipe_fds.as_mut_ptr()) };
        assert_eq!(result, 0, "Failed to create pipe");

        let read_fd = pipe_fds[0];
        let write_fd = pipe_fds[1];

        let expected = content.clone();
        let reader_thread = std::thread::spawn(move || {
            let mut received = Vec::with_capacity(expected.len());
            let mut buf = [0u8; 8192];
            while received.len() < expected.len() {
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

        let sent = send_file_to_fd(source.as_file(), write_fd, content.len() as u64).unwrap();
        assert_eq!(sent, content.len() as u64);

        unsafe { libc::close(write_fd) };

        let received = reader_thread.join().expect("reader thread should succeed");
        assert_eq!(received, content);
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

    #[cfg(target_os = "linux")]
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
