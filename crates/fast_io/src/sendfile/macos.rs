//! Darwin `sendfile(2)` zero-copy implementation.
//!
//! macOS exposes a BSD-style `sendfile` whose signature differs from Linux,
//! requiring an explicit offset and an in/out length parameter. This module
//! adapts that signature back to the "advance file position" contract that
//! the public [`send_file_to_fd`](super::send_file_to_fd) wrapper documents.

use std::fs::File;
use std::io;

/// Maximum bytes per Darwin `sendfile` call.
///
/// Darwin's `sendfile` accepts an `off_t` length so the only practical ceiling
/// is `i64::MAX`. We cap each call at ~2 GiB to keep partial-send accounting
/// simple and avoid pinning a single socket for too long when other I/O is
/// waiting.
const SENDFILE_CHUNK_SIZE: u64 = 0x7fff_f000;

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
/// [`send_file_to_fd`](super::send_file_to_fd) can fall back to the buffered
/// `read`/`write` loop.
///
/// # Source offset
///
/// Linux's `sendfile` with a NULL offset pointer uses and advances the
/// source file position. Darwin's signature requires an explicit offset
/// and does not touch the file position. To preserve the "transfer from
/// the current file position" contract documented on
/// [`send_file_to_fd`](super::send_file_to_fd), this function reads the
/// current position via `lseek(fd, 0, SEEK_CUR)`, passes it as the syscall
/// offset, and advances the position with a matching `lseek(SEEK_SET)`
/// after a successful transfer.
///
/// # Safety
///
/// Uses unsafe FFI to call `libc::sendfile` and `libc::lseek`. File
/// descriptors must be valid (they are derived from `&File` and a caller-
/// provided socket fd).
#[allow(unsafe_code)]
pub(super) fn try_sendfile_macos(source: &File, dest_fd: i32, length: u64) -> io::Result<u64> {
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
