//! Force the standard input/output streams to blocking mode at server entry.
//!
//! Pipes inherited from a parent process are normally blocking on Unix, but a
//! misbehaving parent (or a future runtime change) could leave `O_NONBLOCK`
//! set on stdin/stdout. The multiplex-frame writer in `transfer` assumes
//! blocking I/O semantics matching upstream rsync's `io.c::writefd_unbuffered`,
//! which retries on `EAGAIN` via `select()`/`poll()`. When our writer sees a
//! `WouldBlock` error it currently propagates immediately, surfacing as
//! `Resource temporarily unavailable (os error 11)` and aborting the transfer.
//!
//! Calling [`force_blocking_stdio`] at server entry clears `O_NONBLOCK` on FDs
//! 0 and 1 so the writer's blocking contract holds regardless of how stdio
//! was inherited.
//!
//! On Windows the helper is a no-op: there is no `O_NONBLOCK` equivalent on
//! the inherited stdio handles.
//!
//! upstream: `io.c::writefd_unbuffered` relies on blocking stdin/stdout.

use std::io;

/// Clear `O_NONBLOCK` on the inherited stdin and stdout file descriptors so
/// they match upstream rsync's blocking-I/O contract.
///
/// On Unix this calls `fcntl(F_GETFL)` and `fcntl(F_SETFL)` to clear the
/// `O_NONBLOCK` bit on FDs 0 and 1. The call is idempotent: if the flag is
/// already cleared, the second `fcntl` is a no-op from the kernel's
/// perspective.
///
/// On non-Unix platforms this returns `Ok(())` immediately.
///
/// # Errors
///
/// Returns the OS error from `fcntl(2)` on the first descriptor that fails.
/// Typical failures (`EBADF`) only occur if the calling process closed its
/// stdin or stdout before invoking this helper.
pub fn force_blocking_stdio() -> io::Result<()> {
    #[cfg(unix)]
    {
        clear_nonblock(libc::STDIN_FILENO)?;
        clear_nonblock(libc::STDOUT_FILENO)?;
    }
    Ok(())
}

#[cfg(unix)]
fn clear_nonblock(fd: libc::c_int) -> io::Result<()> {
    // SAFETY: `libc::fcntl` with `F_GETFL` takes a file descriptor and
    // returns the current flags (or `-1` on error). The descriptor is the
    // inherited stdio FD, which is owned by the process for its lifetime.
    // No memory is read or written; only the kernel-side flags are queried.
    #[allow(unsafe_code)]
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    if flags & libc::O_NONBLOCK == 0 {
        return Ok(());
    }
    let cleared = flags & !libc::O_NONBLOCK;
    // SAFETY: `libc::fcntl` with `F_SETFL` writes the supplied integer flag
    // mask back to the descriptor. The flags value originated from a
    // preceding `F_GETFL` on the same descriptor with `O_NONBLOCK` masked
    // off, so the kernel will accept it without altering any other state.
    #[allow(unsafe_code)]
    let result = unsafe { libc::fcntl(fd, libc::F_SETFL, cleared) };
    if result < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

    #[test]
    fn force_blocking_stdio_is_idempotent() {
        // First call clears any inherited O_NONBLOCK (typically already
        // clear in cargo test); the second call must still succeed on the
        // now-blocking descriptors and report Ok.
        force_blocking_stdio().expect("first call succeeds on inherited stdio");
        force_blocking_stdio().expect("second call is idempotent");
    }

    #[test]
    fn clear_nonblock_removes_flag_on_owned_fd() {
        // Use a pipe so we can flip the flag on a real FD and verify the
        // helper clears it without affecting any other bits.
        let mut fds = [0_i32; 2];
        // SAFETY: `libc::pipe` writes two file descriptors into the
        // 2-element array. The call is a standard syscall with no aliasing
        // requirements; the returned FDs are owned by this test and closed
        // through `OwnedFd` Drop.
        #[allow(unsafe_code)]
        let rc = unsafe { libc::pipe(fds.as_mut_ptr()) };
        assert_eq!(rc, 0, "pipe(2) failed: {}", io::Error::last_os_error());
        // SAFETY: `fds[0]` and `fds[1]` are fresh descriptors returned by
        // `pipe(2)` and not yet owned by any other RAII wrapper, so it is
        // valid to take ownership here.
        #[allow(unsafe_code)]
        let read_fd = unsafe { OwnedFd::from_raw_fd(fds[0]) };
        #[allow(unsafe_code)]
        let _write_fd = unsafe { OwnedFd::from_raw_fd(fds[1]) };

        let raw = read_fd.as_raw_fd();
        // SAFETY: setting flags via fcntl on an owned descriptor; no memory aliasing.
        #[allow(unsafe_code)]
        let prev = unsafe { libc::fcntl(raw, libc::F_GETFL) };
        assert!(prev >= 0);
        // SAFETY: writing a valid flags integer back to the same owned descriptor.
        #[allow(unsafe_code)]
        let rc = unsafe { libc::fcntl(raw, libc::F_SETFL, prev | libc::O_NONBLOCK) };
        assert_eq!(rc, 0);

        clear_nonblock(raw).expect("clear_nonblock succeeds on owned pipe fd");

        // SAFETY: reading flags on the same owned descriptor.
        #[allow(unsafe_code)]
        let post = unsafe { libc::fcntl(raw, libc::F_GETFL) };
        assert!(post >= 0);
        assert_eq!(post & libc::O_NONBLOCK, 0, "O_NONBLOCK should be cleared");
    }
}
