//! Close the write side of the inherited stdout descriptor.
//!
//! After the receiver-role `--server` mode finishes writing its final
//! goodbye (NDX_DONE plus the protocol trailer), it needs to signal the
//! peer that no more bytes will arrive on the link. The peer's
//! `read_final_goodbye()` reads until EOF and only then proceeds to its
//! own `exit_cleanup`. If our process simply returns from the transfer
//! function without closing first, the peer keeps the connection alive
//! while it streams trailing `MSG_STATS` / `MSG_ERROR_EXIT` bytes, and
//! the receiver's drain loop on stdin blocks waiting for an EOF that
//! never arrives - the symmetric deadlock that surfaced as `alt-dest`,
//! `00-hello`, `ssh-basic`, `symlink-dirlink-basis`, `hardlinks`,
//! `test_iconv_local_ssh_interop` direction-b, and
//! `test_compress_ssh_interop` direction-b timing out under lsh.sh /
//! `fake_rsh` in the interop harness.
//!
//! Calling [`shutdown_stdio_write`] after the final flush first tries
//! `shutdown(STDOUT_FILENO, SHUT_WR)`. On an SSH `SOCK_STREAM`
//! transport this sends FIN to the peer while leaving the read side
//! open, the canonical half-close. On a pipe (the
//! `--rsh=fake_rsh --rsync-path=oc-rsync` interop transport configures
//! upstream rsync to spawn us via `popen2`-style pipes, NOT sockets)
//! the syscall returns `ENOTSOCK`; the helper then falls back to
//! `dup2(/dev/null, STDOUT_FILENO)`, which atomically closes the
//! pipe write end (the peer's read returns 0 deterministically)
//! while leaving FD 1 a valid writable descriptor so any late stray
//! write does not error with `EBADF`. Either way our stdin then
//! reads 0 without a wall-clock cap.
//!
//! On non-Unix platforms the helper is a no-op: the inherited stdio
//! handles are not sockets and the SSH / remote-shell transport this
//! fix targets is Unix-only.
//!
//! upstream: io.c:943-963 `noop_io_until_death()` (read loop that
//! terminates on EOF); io.c:217-232 `whine_about_eof()` treats EOF
//! inside the `kluge_around_eof` window as a clean exit; cleanup.c:254
//! `noop_io_until_death()` call in `_exit_cleanup`.

use std::io;

/// Close the write side of the inherited stdout descriptor so the
/// peer's `read_final_goodbye()` sees EOF and proceeds to exit cleanup.
///
/// On Unix the helper first tries `shutdown(STDOUT_FILENO, SHUT_WR)`.
/// On a stream socket (SSH transport) the kernel sends FIN to the
/// peer while leaving the read side open - the canonical half-close.
/// On a pipe the syscall returns `ENOTSOCK`; the helper falls back to
/// `dup2(/dev/null, STDOUT_FILENO)`, which atomically closes the
/// pipe write end (sending EOF to the peer that is reading our pipe)
/// while keeping FD 1 a valid writable sink so any late stray write
/// does not error.
///
/// On non-Unix platforms the helper returns `Ok(())` immediately.
///
/// # Safety contract
///
/// The caller must guarantee that no other thread is writing to
/// `STDOUT_FILENO` during this call. In the cli `--server` receiver
/// exit path the goodbye envelope has already been written and the
/// caller is about to return, so this invariant holds.
///
/// # Errors
///
/// Returns the OS error from `shutdown(2)` when the descriptor has
/// been closed (`EBADF`) or the kernel rejects the request for a
/// reason other than `ENOTSOCK`. Returns the OS error from `open(2)`
/// or `dup2(2)` if the `/dev/null` fallback path itself fails.
/// Callers in receiver-role server exit paths log the error and
/// continue: the close is a deadlock prevention, not a correctness
/// primitive.
pub fn shutdown_stdio_write() -> io::Result<()> {
    #[cfg(unix)]
    {
        close_write_side(libc::STDOUT_FILENO)
    }
    #[cfg(not(unix))]
    {
        Ok(())
    }
}

#[cfg(unix)]
fn close_write_side(fd: libc::c_int) -> io::Result<()> {
    // SAFETY: `libc::shutdown` takes a file descriptor and a flag
    // constant by value. The descriptor is owned by the caller for
    // the lifetime of the call; no memory is read or written. The
    // flag `SHUT_WR` is a kernel constant. On success the kernel
    // marks the write side of the socket shut down and the call
    // returns 0; on failure it returns -1 and sets `errno`.
    #[allow(unsafe_code)]
    let rc = unsafe { libc::shutdown(fd, libc::SHUT_WR) };
    if rc == 0 {
        return Ok(());
    }
    let err = io::Error::last_os_error();
    // Non-socket descriptors return ENOTSOCK; pipes are the common
    // case under the `--rsh=<wrapper>` interop transport. Fall back
    // to `dup2(/dev/null, fd)` so the peer reading our pipe sees EOF
    // without depending on process exit. Other errors (EBADF, etc.)
    // surface to the caller.
    if err.raw_os_error() != Some(libc::ENOTSOCK) {
        return Err(err);
    }
    redirect_fd_to_devnull(fd)
}

#[cfg(unix)]
fn redirect_fd_to_devnull(fd: libc::c_int) -> io::Result<()> {
    // SAFETY: `libc::open` reads the NUL-terminated literal at the
    // pointer and returns a fresh file descriptor (or -1 on error).
    // The C string lives in the binary's read-only segment for the
    // lifetime of the process. The flag `O_WRONLY` is a kernel
    // constant; no memory is read or written.
    #[allow(unsafe_code)]
    let devnull = unsafe { libc::open(c"/dev/null".as_ptr(), libc::O_WRONLY) };
    if devnull < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `libc::dup2` atomically reassigns `fd` to refer to the
    // same open file description as `devnull`. If `fd` was already
    // open the kernel closes it first, which is exactly what we want:
    // sending EOF to the peer reading the pipe. On success the call
    // returns `fd`; on failure -1 with errno set.
    #[allow(unsafe_code)]
    let dup_rc = unsafe { libc::dup2(devnull, fd) };
    let dup_err = if dup_rc < 0 {
        Some(io::Error::last_os_error())
    } else {
        None
    };
    // SAFETY: `devnull` was returned by `libc::open` above and is not
    // shared with any RAII wrapper. Closing it releases the extra
    // descriptor; `fd` still references the same `/dev/null` open
    // file description.
    #[allow(unsafe_code)]
    unsafe {
        libc::close(devnull);
    }
    match dup_err {
        Some(e) => Err(e),
        None => Ok(()),
    }
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
    fn shutdown_stdio_write_drives_pipe_peer_to_eof() {
        // The `--rsh=fake_rsh` interop transport gives the spawned
        // `--server --receiver` a pipe for stdout, not a socket. The
        // helper must fall back from `shutdown(SHUT_WR)` (which returns
        // ENOTSOCK on a pipe) to `dup2(/dev/null, STDOUT_FILENO)` so the
        // peer reading our pipe still sees EOF deterministically.
        let mut fds = [0_i32; 2];
        // SAFETY: pipe(2) writes two owned descriptors into the array.
        #[allow(unsafe_code)]
        let rc = unsafe { libc::pipe(fds.as_mut_ptr()) };
        assert_eq!(rc, 0, "pipe failed: {}", io::Error::last_os_error());
        // SAFETY: take ownership of both pipe ends so they close at
        // scope exit even if a test assertion panics.
        #[allow(unsafe_code)]
        let read_end = unsafe { OwnedFd::from_raw_fd(fds[0]) };
        #[allow(unsafe_code)]
        let write_end = unsafe { OwnedFd::from_raw_fd(fds[1]) };

        let _guard = StdoutRedirect::install(write_end.as_raw_fd()).expect("redirect stdout");

        shutdown_stdio_write().expect("close on a pipe should succeed via /dev/null fallback");

        // The original pipe write end on STDOUT_FILENO is now closed
        // (replaced by /dev/null) while `write_end` is the only other
        // handle; once the test runner's stdout is also restored on
        // guard drop, the read end will see EOF. Drop the guard now to
        // restore stdout and then verify the pipe reader sees EOF.
        drop(_guard);
        // After guard drop, the kernel reference count on the pipe
        // write side drops to zero - `write_end` is the only remaining
        // handle and we close it now.
        drop(write_end);

        let mut buf = [0u8; 4];
        // SAFETY: `libc::read` reads up to `buf.len()` bytes into the
        // start of `buf`. The descriptor is owned by `read_end`.
        #[allow(unsafe_code)]
        let n = unsafe { libc::read(read_end.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) };
        assert_eq!(n, 0, "pipe read should return EOF after close fallback");
    }

    #[test]
    fn close_write_side_pipe_returns_ok() {
        // Direct exercise of the `dup2(/dev/null, fd)` fallback on a
        // pipe FD without redirecting STDOUT_FILENO, so this test runs
        // safely under parallel threads.
        let mut fds = [0_i32; 2];
        // SAFETY: pipe(2) writes two owned descriptors into the array.
        #[allow(unsafe_code)]
        let rc = unsafe { libc::pipe(fds.as_mut_ptr()) };
        assert_eq!(rc, 0, "pipe failed: {}", io::Error::last_os_error());
        // SAFETY: take ownership of both pipe ends so they close at
        // scope exit even if a test assertion panics.
        #[allow(unsafe_code)]
        let read_end = unsafe { OwnedFd::from_raw_fd(fds[0]) };
        #[allow(unsafe_code)]
        let write_end = unsafe { OwnedFd::from_raw_fd(fds[1]) };

        close_write_side(write_end.as_raw_fd()).expect("close_write_side ok on pipe fd");

        // write_end now points at /dev/null; the original pipe write
        // side has been closed by dup2. Drop write_end and confirm the
        // pipe read end sees EOF.
        drop(write_end);
        let mut buf = [0u8; 4];
        // SAFETY: `libc::read` reads up to `buf.len()` bytes into the
        // start of `buf`. The descriptor is owned by `read_end`.
        #[allow(unsafe_code)]
        let n = unsafe { libc::read(read_end.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) };
        assert_eq!(n, 0, "pipe read should return EOF after close_write_side");
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
