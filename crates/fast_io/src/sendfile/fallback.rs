//! Portable read/write fallback paths.
//!
//! These functions implement the buffered read/write loops used when
//! `sendfile(2)` is unavailable, unsupported by the destination fd, or
//! disabled by policy. The userspace `read`/`write` loop works on every
//! supported target, so this module compiles unconditionally and provides
//! both the generic [`copy_via_readwrite`] writer fallback and the unix
//! [`copy_via_fd_write`] raw-fd fallback.

use std::fs::File;
use std::io::{self, Read, Write};

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
pub(super) fn copy_via_readwrite<W: Write>(
    source: &File,
    destination: &mut W,
    length: u64,
) -> io::Result<u64> {
    let mut reader = io::BufReader::new(source);
    let mut buf = vec![0u8; 256 * 1024];
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
#[cfg(unix)]
pub(super) fn copy_via_fd_write(source: &File, dest_fd: i32, length: u64) -> io::Result<u64> {
    let mut reader = io::BufReader::new(source);
    let mut buf = vec![0u8; 256 * 1024];
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
