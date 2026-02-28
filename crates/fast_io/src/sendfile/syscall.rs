//! Linux `sendfile` syscall wrapper for zero-copy file-to-socket transfer.

use std::fs::File;
use std::io;
#[cfg(unix)]
use std::os::fd::AsRawFd;

use super::SENDFILE_CHUNK_SIZE;

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
pub(super) fn try_sendfile(source: &File, dest_fd: i32, length: u64) -> io::Result<u64> {
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
