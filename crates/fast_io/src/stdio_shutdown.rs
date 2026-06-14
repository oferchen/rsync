//! Half-close the write side of the inherited stdout descriptor.
//!
//! After the receiver-role `--server` mode finishes writing its final
//! goodbye (NDX_DONE plus the protocol trailer), it needs to signal the
//! peer that no more bytes will arrive on the link. The peer's
//! `read_final_goodbye()` reads until EOF and only then proceeds to its
//! own `exit_cleanup`. If our process simply returns from the transfer
//! function without half-closing first, the peer keeps the connection
//! alive while it streams trailing `MSG_STATS` / `MSG_ERROR_EXIT` bytes,
//! and the receiver's drain loop on stdin blocks waiting for an EOF that
//! never arrives - the symmetric deadlock that surfaced as `alt-dest`,
//! `00-hello`, `ssh-basic`, `symlink-dirlink-basis`, and `hardlinks`
//! timing out under lsh.sh in UTS-V3 cluster A.
//!
//! Calling [`shutdown_stdio_write`] after the final flush issues
//! `shutdown(STDOUT_FILENO, SHUT_WR)` on Unix. For an SSH or lsh.sh
//! transport stdout is a `SOCK_STREAM`; the half-close sends FIN to the
//! peer, which unblocks its `read_final_goodbye` and lets it close its
//! own write end. Our stdin then reads 0 deterministically and the
//! drain loop terminates without a wall-clock cap.
//!
//! When stdout is a pipe (daemon mode dispatches through a different
//! entry point that never reaches this helper, but the cli `--server`
//! receiver path is also reachable from non-socket configurations), the
//! syscall returns `ENOTSOCK` and the helper returns the error as-is.
//! Callers treat it as best-effort: regular pipes propagate EOF via the
//! same process-exit `close(2)` that already runs, so failing to
//! half-close is harmless. The Windows build is a no-op because the
//! inherited stdio handles are not sockets and the SSH transport is not
//! supported on Windows.
//!
//! upstream: io.c:943-963 `noop_io_until_death()`; cleanup.c:254
//! `noop_io_until_death()` call in `_exit_cleanup`.

use std::io;

/// Half-close the write side of the inherited stdout descriptor so the
/// peer's `read_final_goodbye()` sees EOF and proceeds to exit cleanup.
///
/// On Unix this issues `shutdown(STDOUT_FILENO, SHUT_WR)`. On a stream
/// socket (the SSH or `lsh.sh` server transport) it sends FIN to the
/// peer. On a pipe or a closed descriptor the syscall returns
/// `ENOTSOCK` / `EBADF`; the error is propagated so callers can log it
/// when useful, but the caller treats it as best-effort: process exit
/// will close the descriptor regardless.
///
/// On non-Unix platforms the helper returns `Ok(())` immediately
/// because the inherited stdio handles are not sockets and the SSH
/// transport that needs the half-close is Unix-only.
///
/// # Errors
///
/// Returns the OS error from `shutdown(2)` when the descriptor is not a
/// socket (`ENOTSOCK`), has been closed (`EBADF`), or the kernel rejects
/// the request for another reason. Callers in receiver-role server exit
/// paths log the error and continue: the half-close is a deadlock
/// prevention, not a correctness primitive.
pub fn shutdown_stdio_write() -> io::Result<()> {
    #[cfg(unix)]
    {
        // SAFETY: `libc::shutdown` takes a file descriptor and a flag
        // constant by value. The descriptor is the inherited stdout FD
        // (constant `STDOUT_FILENO`), which the calling process owns
        // for its lifetime; no memory is read or written. The flag
        // `SHUT_WR` is a kernel-defined integer. On success the kernel
        // marks the write side of the socket shut down and the call
        // returns 0; on failure it returns -1 and sets `errno`.
        #[allow(unsafe_code)]
        let rc = unsafe { libc::shutdown(libc::STDOUT_FILENO, libc::SHUT_WR) };
        if rc < 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd};
    use std::os::unix::net::UnixStream;

    /// Replace `STDOUT_FILENO` with `replacement_fd` for the duration of
    /// the returned guard, restoring the original stdout on drop. Used
    /// to verify `shutdown_stdio_write` against owned socket pairs
    /// without touching the test runner's real stdout.
    struct StdoutRedirect {
        saved: OwnedFd,
    }

    impl StdoutRedirect {
        fn install(replacement_fd: i32) -> io::Result<Self> {
            // SAFETY: `libc::dup` returns a new FD that duplicates
            // `STDOUT_FILENO`; we immediately wrap it in `OwnedFd` so it
            // is closed when the guard drops. The descriptor is owned
            // by the process and not aliased.
            #[allow(unsafe_code)]
            let saved_raw = unsafe { libc::dup(libc::STDOUT_FILENO) };
            if saved_raw < 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: `saved_raw` is a fresh descriptor that only this
            // guard holds; ownership transfer to `OwnedFd` is sound.
            #[allow(unsafe_code)]
            let saved = unsafe { OwnedFd::from_raw_fd(saved_raw) };
            // SAFETY: `libc::dup2` atomically closes `STDOUT_FILENO` and
            // duplicates `replacement_fd` onto it. Both descriptors are
            // owned by this test for the duration of the call.
            #[allow(unsafe_code)]
            let rc = unsafe { libc::dup2(replacement_fd, libc::STDOUT_FILENO) };
            if rc < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(Self { saved })
        }
    }

    impl Drop for StdoutRedirect {
        fn drop(&mut self) {
            // SAFETY: restore the original stdout descriptor we saved
            // at construction. `dup2` is documented to handle the
            // duplicate-onto-self case, and any failure here is
            // observable only as test stdout being closed - acceptable
            // because Drop runs on test thread teardown.
            #[allow(unsafe_code)]
            let _ = unsafe { libc::dup2(self.saved.as_raw_fd(), libc::STDOUT_FILENO) };
        }
    }

    #[test]
    fn shutdown_stdio_write_sends_fin_on_socket() {
        // Wire stdout to one end of a Unix socket pair. After the
        // half-close the peer must observe EOF on its read end - this
        // is exactly the upstream sender's view of our half-close on
        // the lsh.sh transport.
        let (mut peer, ours) = UnixStream::pair().expect("socketpair");
        let ours_fd = ours.into_raw_fd();
        let _guard = StdoutRedirect::install(ours_fd).expect("redirect stdout");
        // SAFETY: ours_fd was just duped onto STDOUT_FILENO and the
        // original handle is no longer used; take ownership back via
        // OwnedFd so the descriptor is closed at scope exit and does
        // not leak.
        #[allow(unsafe_code)]
        let _ours = unsafe { OwnedFd::from_raw_fd(ours_fd) };

        // Send a sentinel byte through stdout so the peer can confirm
        // the pre-shutdown writes were delivered, then half-close.
        // SAFETY: writing through STDOUT_FILENO with a kernel-allocated
        // socket descriptor; the buffer is borrowed for the call.
        #[allow(unsafe_code)]
        let written =
            unsafe { libc::write(libc::STDOUT_FILENO, b"X".as_ptr().cast::<libc::c_void>(), 1) };
        assert_eq!(written, 1, "write returned {written}");

        shutdown_stdio_write().expect("half-close should succeed on a socket");

        let mut buf = [0u8; 4];
        let n = peer.read(&mut buf).expect("peer reads sentinel");
        assert_eq!(&buf[..n], b"X");
        let eof = peer.read(&mut buf).expect("peer reads EOF after FIN");
        assert_eq!(eof, 0, "peer should see EOF after our half-close");
    }

    #[test]
    fn shutdown_stdio_write_surfaces_enotsock_on_pipe() {
        // Pipes are not sockets; the helper must surface the kernel's
        // ENOTSOCK so callers that care can log it.
        let mut fds = [0_i32; 2];
        // SAFETY: pipe(2) writes two owned descriptors into the array.
        #[allow(unsafe_code)]
        let rc = unsafe { libc::pipe(fds.as_mut_ptr()) };
        assert_eq!(rc, 0, "pipe failed: {}", io::Error::last_os_error());
        // SAFETY: take ownership of both pipe ends so they close at
        // scope exit even if a test assertion panics.
        #[allow(unsafe_code)]
        let _read_end = unsafe { OwnedFd::from_raw_fd(fds[0]) };
        #[allow(unsafe_code)]
        let write_end = unsafe { OwnedFd::from_raw_fd(fds[1]) };

        let _guard = StdoutRedirect::install(write_end.as_raw_fd()).expect("redirect stdout");

        let err = shutdown_stdio_write().expect_err("shutdown on a pipe should fail with ENOTSOCK");
        assert_eq!(err.raw_os_error(), Some(libc::ENOTSOCK));
    }

    #[test]
    fn shutdown_stdio_write_is_observable_to_buffered_writer() {
        // Validate the call sequence that the cli `--server` exit path
        // uses: flush a `BufWriter` wrapping stdout, then half-close.
        // The peer must observe the flushed bytes followed by EOF.
        let (mut peer, ours) = UnixStream::pair().expect("socketpair");
        let ours_fd = ours.into_raw_fd();
        let _guard = StdoutRedirect::install(ours_fd).expect("redirect stdout");
        // SAFETY: reclaim ownership of the duped descriptor so it is
        // released when the test exits; see the previous test for the
        // identical pattern.
        #[allow(unsafe_code)]
        let _ours = unsafe { OwnedFd::from_raw_fd(ours_fd) };

        let mut writer = io::BufWriter::new(io::stdout());
        writer.write_all(b"goodbye").expect("buffered write");
        writer.flush().expect("flush before half-close");

        shutdown_stdio_write().expect("half-close after flush");

        let mut buf = Vec::new();
        peer.read_to_end(&mut buf).expect("peer reads to EOF");
        assert_eq!(buf, b"goodbye");
    }
}
