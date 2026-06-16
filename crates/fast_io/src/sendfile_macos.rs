//! Low-level macOS `sendfile(2)` wrapper.
//!
//! Exposes Darwin's BSD-style `sendfile` semantics directly: explicit
//! `offset`, explicit `len`, partial-progress return, and EAGAIN /
//! EINTR surfacing. This is intentionally narrower than the
//! [`sendfile::send_file_to_fd`](crate::sendfile::send_file_to_fd)
//! dispatch wrapper - it does not advance the source's file position,
//! does not chunk transfers internally beyond the `EINTR` retry loop,
//! and surfaces `WouldBlock` to non-blocking callers with the partial
//! byte count preserved.
//!
//! See `docs/design/net-sf-macos-audit.md` for the full Darwin
//! `sendfile(2)` audit (caller requirements, return-value semantics,
//! error matrix, header/trailer notes).
//!
//! # Source of truth
//!
//! Upstream rsync's I/O layer does not call `sendfile` directly - this
//! wrapper exists so the Rust port can opt into kernel-side zero-copy
//! on macOS where the protocol shape permits. Wire framing, ordering,
//! and end-of-transfer semantics still defer to upstream
//! (`io.c`, `sender.c`).
//!
//! # Cross-platform
//!
//! [`sendfile_macos`] compiles unconditionally. On non-macOS targets
//! it returns [`io::ErrorKind::Unsupported`] so call sites can be
//! written without `#[cfg]` gates.

use std::fmt;
use std::io;

#[cfg(unix)]
use std::os::fd::BorrowedFd;

/// Partial-progress payload attached to an [`io::ErrorKind::WouldBlock`]
/// surfaced from [`sendfile_macos`].
///
/// Darwin's `sendfile(2)` reports the number of bytes actually
/// delivered before an `EAGAIN` / `EWOULDBLOCK` even though it
/// returns `-1`. The wrapper attaches that count to the error so
/// callers driving non-blocking sockets can advance their cursor
/// without re-querying the socket.
///
/// Extract via [`io::Error::get_ref`] and
/// [`std::error::Error::downcast_ref`].
#[derive(Debug, Clone, Copy)]
pub struct PartialSend {
    /// Bytes delivered to the destination before the would-block fired.
    pub sent: usize,
}

impl fmt::Display for PartialSend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "sendfile: would block after {} byte(s)", self.sent)
    }
}

impl std::error::Error for PartialSend {}

/// Calls Darwin's `sendfile(2)` to transfer up to `len` bytes from
/// `in_fd` (regular file) starting at `offset` into `out_fd`
/// (connected stream socket).
///
/// Returns the number of file-region bytes the kernel reports as
/// delivered on success. The source file position is **not**
/// modified; callers track the cursor themselves and pass it in via
/// `offset`.
///
/// # Behaviour
///
/// - `offset < 0` returns [`io::ErrorKind::InvalidInput`].
/// - `len == 0` returns `Ok(0)` without entering the syscall.
/// - `offset + len` overflowing `i64::MAX` returns
///   [`io::ErrorKind::InvalidInput`].
/// - `EINTR` is retried internally up to a fixed cap so signal
///   pressure stays transparent to callers.
/// - `EAGAIN` / `EWOULDBLOCK` is surfaced as
///   [`io::ErrorKind::WouldBlock`] carrying a [`PartialSend`]
///   payload with the bytes-sent prefix.
/// - All other errnos surface via [`io::Error::last_os_error`].
///
/// # Platform support
///
/// Implemented on `target_os = "macos"`. On every other target the
/// function returns [`io::ErrorKind::Unsupported`] so the call site
/// can stay free of `#[cfg]` gates.
///
/// # Source of truth
///
/// See `docs/design/net-sf-macos-audit.md` for the full Darwin
/// `sendfile(2)` audit. Upstream rsync remains the authoritative
/// reference for wire framing; this wrapper only substitutes a
/// faster transport for the file-region bytes.
#[cfg(unix)]
pub fn sendfile_macos(
    in_fd: BorrowedFd<'_>,
    out_fd: BorrowedFd<'_>,
    offset: i64,
    len: usize,
) -> io::Result<usize> {
    #[cfg(target_os = "macos")]
    {
        macos_impl::sendfile_macos(in_fd, out_fd, offset, len)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (in_fd, out_fd, offset, len);
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }
}

/// Non-Unix stub: file descriptors are not meaningful so the call
/// always returns [`io::ErrorKind::Unsupported`].
#[cfg(not(unix))]
pub fn sendfile_macos(_in_fd: i32, _out_fd: i32, _offset: i64, _len: usize) -> io::Result<usize> {
    Err(io::Error::from(io::ErrorKind::Unsupported))
}

#[cfg(target_os = "macos")]
mod macos_impl {
    use super::PartialSend;
    use std::io;
    use std::os::fd::{AsRawFd, BorrowedFd};

    /// Maximum number of `EINTR` retries before surfacing the error.
    ///
    /// Large enough that real-world signal pressure stays transparent;
    /// finite so a signal storm cannot wedge a calling thread.
    const EINTR_RETRY_CAP: u32 = 1024;

    /// Reject lengths that would overflow `off_t` when added to
    /// `offset`. Darwin's `off_t` is `i64`.
    fn check_offset_len(offset: i64, len: usize) -> io::Result<libc::off_t> {
        if offset < 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "sendfile: negative offset",
            ));
        }
        let len_i64 = i64::try_from(len).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "sendfile: length overflows i64",
            )
        })?;
        offset.checked_add(len_i64).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "sendfile: offset + len overflows i64",
            )
        })?;
        Ok(len_i64 as libc::off_t)
    }

    #[allow(unsafe_code)]
    pub(super) fn sendfile_macos(
        in_fd: BorrowedFd<'_>,
        out_fd: BorrowedFd<'_>,
        offset: i64,
        len: usize,
    ) -> io::Result<usize> {
        if len == 0 {
            return Ok(0);
        }
        let total_len = check_offset_len(offset, len)?;

        let src = in_fd.as_raw_fd();
        let dst = out_fd.as_raw_fd();

        let mut total_sent: i64 = 0;
        let mut eintr_retries: u32 = 0;

        loop {
            let remaining = total_len - total_sent;
            if remaining <= 0 {
                break;
            }
            let mut chunk_len: libc::off_t = remaining;
            let cur_offset: libc::off_t = offset + total_sent;

            // SAFETY: `src` and `dst` are raw fds borrowed from
            // `BorrowedFd` values; they remain valid for the
            // duration of the call. `&mut chunk_len` is a valid
            // stack pointer. `hdtr = NULL` and `flags = 0` request a
            // plain file-region transfer with no scatter-gather.
            let ret = unsafe {
                libc::sendfile(
                    src,
                    dst,
                    cur_offset,
                    &mut chunk_len as *mut libc::off_t,
                    std::ptr::null_mut(),
                    0,
                )
            };

            // Darwin populates `chunk_len` with bytes actually
            // delivered on both success and EAGAIN/EINTR. Clamp
            // negative values (should never occur) to zero so we
            // never undercount or panic on the cast.
            let sent_this_call = if chunk_len > 0 { chunk_len } else { 0 };
            total_sent += sent_this_call;

            if ret == 0 {
                if sent_this_call == 0 {
                    // Source reached EOF before fulfilling the
                    // requested length. Return the short count
                    // instead of looping forever.
                    break;
                }
                continue;
            }

            // ret == -1: the kernel set errno. Decide whether to
            // retry, surface as `WouldBlock` with a partial count,
            // or surface as-is. On Darwin `EAGAIN == EWOULDBLOCK`, so
            // a single `if` covers both rather than triggering
            // `unreachable_patterns` on a match arm.
            let err = io::Error::last_os_error();
            let raw = err.raw_os_error();
            if raw == Some(libc::EINTR) {
                eintr_retries += 1;
                if eintr_retries > EINTR_RETRY_CAP {
                    return Err(err);
                }
                continue;
            }
            if raw == Some(libc::EAGAIN) || raw == Some(libc::EWOULDBLOCK) {
                let sent = usize::try_from(total_sent).unwrap_or(usize::MAX);
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    PartialSend { sent },
                ));
            }
            return Err(err);
        }

        Ok(usize::try_from(total_sent).unwrap_or(usize::MAX))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Seek, SeekFrom, Write};
    #[cfg(unix)]
    use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd};
    use tempfile::NamedTempFile;

    /// Build a temp file containing `content`, leaving its position at 0.
    fn fixture(content: &[u8]) -> NamedTempFile {
        let mut f = NamedTempFile::new().expect("tempfile");
        f.write_all(content).expect("write");
        f.flush().expect("flush");
        f.seek(SeekFrom::Start(0)).expect("seek");
        f
    }

    /// Create an `AF_UNIX` `SOCK_STREAM` socket pair as
    /// `(receiver, sender)`. Used to exercise the zero-copy path
    /// against the canonical Darwin sendfile destination.
    #[cfg(unix)]
    fn socketpair_stream() -> (OwnedFd, OwnedFd) {
        let mut fds = [0i32; 2];
        // SAFETY: `fds` is the two-int output slot the syscall fills.
        let rc = unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, fds.as_mut_ptr()) };
        assert_eq!(rc, 0, "socketpair: {}", io::Error::last_os_error());
        // SAFETY: both fds were just returned by the kernel and are
        // not aliased elsewhere; `OwnedFd` takes exclusive ownership.
        unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) }
    }

    /// Drain everything readable from `fd` until EOF or `cap`
    /// bytes accumulate, whichever comes first.
    #[cfg(target_os = "macos")]
    fn drain_to_vec(fd: &OwnedFd, cap: usize) -> Vec<u8> {
        let mut out = Vec::with_capacity(cap);
        let mut buf = [0u8; 8192];
        while out.len() < cap {
            // SAFETY: `fd` is borrowed for the duration of the read;
            // `buf` provides `buf.len()` writable bytes.
            let n = unsafe {
                libc::read(
                    fd.as_raw_fd(),
                    buf.as_mut_ptr().cast::<libc::c_void>(),
                    buf.len(),
                )
            };
            if n <= 0 {
                break;
            }
            out.extend_from_slice(&buf[..n as usize]);
        }
        out
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn len_zero_is_noop() {
        let f = fixture(b"hello");
        let (_recv, send) = socketpair_stream();
        let n = sendfile_macos(f.as_file().as_fd(), send.as_fd(), 0, 0).expect("ok");
        assert_eq!(n, 0);
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn negative_offset_rejected() {
        let f = fixture(b"hello");
        let (_recv, send) = socketpair_stream();
        let err = sendfile_macos(f.as_file().as_fd(), send.as_fd(), -1, 1).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn length_overflow_rejected() {
        // Drive the `offset + len > i64::MAX` overflow path. On 64-bit
        // `usize` targets we can construct `len` directly; on 32-bit
        // hosts `usize::MAX < i64::MAX`, but macOS is 64-bit only so
        // this test is gated to `target_os = "macos"`.
        let f = fixture(b"hello");
        let (_recv, send) = socketpair_stream();
        let big_len = (i64::MAX / 2) as usize + 32;
        let err =
            sendfile_macos(f.as_file().as_fd(), send.as_fd(), i64::MAX - 1, big_len).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    #[cfg(all(unix, not(target_os = "macos")))]
    fn non_macos_returns_unsupported() {
        let f = fixture(b"hello");
        let (_recv, send) = socketpair_stream();
        let err = sendfile_macos(f.as_file().as_fd(), send.as_fd(), 0, 5).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::Unsupported);
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn roundtrip_small() {
        let content = b"hello, sendfile world";
        let f = fixture(content);
        let (recv, send) = socketpair_stream();

        let n = sendfile_macos(f.as_file().as_fd(), send.as_fd(), 0, content.len())
            .expect("sendfile ok");
        assert_eq!(n, content.len());
        drop(send);

        let got = drain_to_vec(&recv, content.len());
        assert_eq!(got.as_slice(), content);
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn respects_offset() {
        // Middle 10 bytes of a 36-byte fixture.
        let content = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ";
        let f = fixture(content);
        let (recv, send) = socketpair_stream();

        let n = sendfile_macos(f.as_file().as_fd(), send.as_fd(), 10, 10).expect("ok");
        assert_eq!(n, 10);
        drop(send);

        let got = drain_to_vec(&recv, 10);
        assert_eq!(got.as_slice(), b"ABCDEFGHIJ");
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn does_not_advance_source_position() {
        // The low-level wrapper must leave the source fd's position
        // untouched - that is the contract that separates it from
        // the higher-level `sendfile::send_file_to_fd`.
        let content = b"abcdefghij";
        let mut f = fixture(content);
        f.seek(SeekFrom::Start(3)).expect("seek");
        let pos_before = f.stream_position().expect("pos");

        let (_recv, send) = socketpair_stream();
        let n = sendfile_macos(f.as_file().as_fd(), send.as_fd(), 0, 5).expect("ok");
        assert_eq!(n, 5);

        let pos_after = f.stream_position().expect("pos");
        assert_eq!(pos_before, pos_after);
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn eof_short_send() {
        // Asking for more than the file holds yields a short count,
        // not an error.
        let content = b"short";
        let f = fixture(content);
        let (recv, send) = socketpair_stream();

        let n = sendfile_macos(f.as_file().as_fd(), send.as_fd(), 0, 1024).expect("ok");
        assert_eq!(n, content.len());
        drop(send);

        let got = drain_to_vec(&recv, content.len());
        assert_eq!(got.as_slice(), content);
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn would_block_carries_partial_count() {
        // Set the send socket non-blocking and shrink its buffer so
        // a large transfer is guaranteed to overflow. Darwin should
        // return EAGAIN with a partial byte count, which the wrapper
        // exposes as `PartialSend`.
        let size = 4 * 1024 * 1024;
        let content: Vec<u8> = (0..size).map(|i| (i % 251) as u8).collect();
        let f = fixture(&content);
        let (_recv, send) = socketpair_stream();

        // Shrink the send buffer.
        let buf: libc::c_int = 4096;
        // SAFETY: `send` is owned for the duration of the call; the
        // setsockopt arguments describe an int-valued option.
        let rc = unsafe {
            libc::setsockopt(
                send.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_SNDBUF,
                (&buf as *const libc::c_int).cast::<libc::c_void>(),
                std::mem::size_of::<libc::c_int>() as libc::socklen_t,
            )
        };
        assert_eq!(
            rc,
            0,
            "setsockopt SO_SNDBUF: {}",
            io::Error::last_os_error()
        );

        // Flip the send socket non-blocking.
        // SAFETY: `send` is a valid open fd; F_GETFL/F_SETFL on a
        // socket fd are well-defined and do not transfer ownership.
        let flags = unsafe { libc::fcntl(send.as_raw_fd(), libc::F_GETFL) };
        assert!(flags >= 0, "fcntl F_GETFL");
        let rc = unsafe { libc::fcntl(send.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) };
        assert!(rc >= 0, "fcntl F_SETFL");

        // Either the kernel happens to absorb the whole transfer
        // (unlikely given a 4 KiB send buffer vs 4 MiB payload) and
        // returns `Ok(size)`, or it returns `WouldBlock` with a
        // partial count. Both outcomes are valid for a correct
        // wrapper; we only fail if we get a different error kind or
        // a `WouldBlock` whose payload is missing.
        match sendfile_macos(f.as_file().as_fd(), send.as_fd(), 0, size) {
            Ok(n) => {
                assert!(n <= size);
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                let partial = e
                    .get_ref()
                    .and_then(|inner| inner.downcast_ref::<PartialSend>())
                    .expect("WouldBlock must carry PartialSend payload");
                assert!(partial.sent <= size);
            }
            Err(e) => panic!("unexpected error: {e}"),
        }
    }
}
