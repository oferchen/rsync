//! Process daemonization - fork, setsid, stdio redirection.
//!
//! # Unix
//!
//! Uses `libc::fork()` and `nix` for setsid/dup2/close/open to detach from
//! the controlling terminal and redirect stdio to `/dev/null`.
//!
//! # Other
//!
//! Not supported - returns an error.
//!
//! # Upstream Reference
//!
//! `clientserver.c:1463` - `become_daemon()`

use std::io;

/// Detaches the current process from the terminal by forking, creating a new
/// session, and redirecting stdin/stdout/stderr to `/dev/null`.
///
/// The parent process exits immediately after fork. The child continues as
/// a background daemon. This matches upstream rsync's `become_daemon()`.
///
/// Must be called before spawning threads (fork is not async-signal-safe
/// with threads).
///
/// upstream: clientserver.c:1463 - `become_daemon()`
#[cfg(unix)]
#[allow(unsafe_code)]
pub fn become_daemon() -> io::Result<()> {
    // Fork - parent exits, child continues.
    // upstream: clientserver.c:1466
    // SAFETY: fork() is safe to call before threads are spawned.
    let pid = unsafe { libc::fork() };
    match pid {
        -1 => return Err(io::Error::last_os_error()),
        0 => {} // child
        _ => std::process::exit(0),
    }

    // Create a new session and detach from controlling terminal.
    // upstream: clientserver.c:1478
    nix::unistd::setsid().map_err(|e| io::Error::from_raw_os_error(e as i32))?;

    // Redirect stdin/stdout/stderr to /dev/null.
    // upstream: clientserver.c:1490-1493
    redirect_stdio_to_devnull()
}

/// Redirects file descriptors 0, 1, 2 to `/dev/null`.
///
/// Uses `nix` safe wrappers for close, open, and dup2 where possible.
#[cfg(unix)]
pub fn redirect_stdio_to_devnull() -> io::Result<()> {
    use nix::fcntl::OFlag;
    use nix::sys::stat::Mode;

    let nix_to_io = |e: nix::Error| io::Error::from_raw_os_error(e as i32);

    for fd in 0..=2_i32 {
        let _ = nix::unistd::close(fd);
        let new_fd =
            nix::fcntl::open(c"/dev/null", OFlag::O_RDWR, Mode::empty()).map_err(nix_to_io)?;
        if new_fd != fd {
            nix::unistd::dup2(new_fd, fd).map_err(nix_to_io)?;
            let _ = nix::unistd::close(new_fd);
        }
    }

    Ok(())
}

/// Daemonization is not supported on non-Unix platforms.
#[cfg(not(unix))]
pub fn become_daemon() -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "daemonization is not supported on this platform",
    ))
}

/// Stdio redirection is not supported on non-Unix platforms.
#[cfg(not(unix))]
pub fn redirect_stdio_to_devnull() -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "stdio redirection is not supported on this platform",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[allow(unsafe_code)]
    #[test]
    fn redirect_stdio_to_devnull_succeeds_in_subprocess() {
        // Spawn a child process that calls redirect_stdio_to_devnull() and
        // exits with code 0 on success or 1 on failure. This avoids closing
        // the test runner's own stdio file descriptors.
        // SAFETY: fork() is safe in this single-threaded test context.
        let pid = unsafe { libc::fork() };
        match pid {
            -1 => panic!("fork failed: {}", io::Error::last_os_error()),
            0 => {
                // Child: attempt the redirect, then _exit.
                let result = redirect_stdio_to_devnull();
                // SAFETY: _exit is safe in the child process.
                unsafe {
                    libc::_exit(if result.is_ok() { 0 } else { 1 });
                }
            }
            child_pid => {
                // Parent: wait for the child and check its exit status.
                let mut status: libc::c_int = 0;
                // SAFETY: waitpid with a valid child PID is safe.
                let waited = unsafe { libc::waitpid(child_pid, &mut status, 0) };
                assert_ne!(waited, -1, "waitpid failed");
                assert!(libc::WIFEXITED(status), "child did not exit normally");
                assert_eq!(
                    libc::WEXITSTATUS(status),
                    0,
                    "redirect_stdio_to_devnull failed in subprocess"
                );
            }
        }
    }

    #[cfg(not(unix))]
    #[test]
    fn become_daemon_returns_unsupported_on_non_unix() {
        let err = become_daemon().unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::Unsupported);
    }

    #[cfg(not(unix))]
    #[test]
    fn redirect_stdio_returns_unsupported_on_non_unix() {
        let err = redirect_stdio_to_devnull().unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::Unsupported);
    }
}
