/// Detaches the current process from the terminal by forking, creating a new
/// session, and redirecting stdin/stdout/stderr to `/dev/null`.
///
/// The parent process exits immediately after fork. The child continues as
/// a background daemon. This matches upstream rsync's `become_daemon()`.
///
/// Must be called before spawning threads (fork is not async-signal-safe
/// with threads).
///
/// # Upstream Reference
///
/// `clientserver.c:1463` -- `become_daemon()`
///
/// # Safety
///
/// Uses `libc::fork()`, `libc::setsid()`, and file descriptor manipulation
/// via the `libc` crate. Must be called before spawning threads.
#[allow(unsafe_code)]
fn become_daemon() -> Result<(), DaemonError> {
    // Fork -- parent exits, child continues.
    // upstream: clientserver.c:1466
    let pid = unsafe { libc::fork() };
    match pid {
        -1 => return Err(daemonize_error("fork", io::Error::last_os_error())),
        0 => {} // child
        _ => std::process::exit(0),
    }

    // Create a new session and detach from controlling terminal.
    // upstream: clientserver.c:1478
    if unsafe { libc::setsid() } == -1 {
        return Err(daemonize_error("setsid", io::Error::last_os_error()));
    }

    // Redirect stdin/stdout/stderr to /dev/null.
    // upstream: clientserver.c:1490-1493
    redirect_stdio_to_devnull()
}

/// Redirects file descriptors 0, 1, 2 to `/dev/null`.
#[allow(unsafe_code)]
fn redirect_stdio_to_devnull() -> Result<(), DaemonError> {
    let dev_null = c"/dev/null";

    for fd in 0..=2 {
        unsafe { libc::close(fd) };
        let new_fd = unsafe { libc::open(dev_null.as_ptr(), libc::O_RDWR) };
        if new_fd == -1 {
            return Err(daemonize_error(
                "open /dev/null",
                io::Error::last_os_error(),
            ));
        }
        if new_fd != fd {
            unsafe { libc::dup2(new_fd, fd) };
            unsafe { libc::close(new_fd) };
        }
    }

    Ok(())
}

/// Creates a [`DaemonError`] for daemonization failures.
fn daemonize_error(action: &str, error: io::Error) -> DaemonError {
    DaemonError::new(
        FEATURE_UNAVAILABLE_EXIT_CODE,
        rsync_error!(
            FEATURE_UNAVAILABLE_EXIT_CODE,
            format!("failed to {action} during daemonization: {error}")
        )
        .with_role(Role::Daemon),
    )
}

#[cfg(test)]
#[allow(unsafe_code)]
mod daemonize_tests {
    use super::*;

    #[test]
    fn daemonize_error_uses_feature_unavailable_exit_code() {
        let error = daemonize_error("fork", io::Error::from_raw_os_error(libc::EAGAIN));
        assert_eq!(error.exit_code(), FEATURE_UNAVAILABLE_EXIT_CODE);
    }

    #[test]
    fn daemonize_error_includes_action_in_message() {
        let error = daemonize_error("setsid", io::Error::from_raw_os_error(libc::EPERM));
        let message = format!("{}", error.message());
        assert!(
            message.contains("setsid"),
            "error message should include the action name, got: {message}"
        );
        assert!(
            message.contains("daemonization"),
            "error message should mention daemonization, got: {message}"
        );
    }

    #[test]
    fn daemonize_error_includes_underlying_os_error() {
        let os_error = io::Error::from_raw_os_error(libc::ENOMEM);
        let expected_fragment = os_error.to_string();
        let error = daemonize_error("fork", io::Error::from_raw_os_error(libc::ENOMEM));
        let message = format!("{}", error.message());
        assert!(
            message.contains(&expected_fragment),
            "error message should contain the OS error description, got: {message}"
        );
    }

    #[test]
    fn redirect_stdio_to_devnull_succeeds_in_subprocess() {
        // Spawn a child process that calls redirect_stdio_to_devnull() and
        // exits with code 0 on success or 1 on failure. This avoids closing
        // the test runner's own stdio file descriptors.
        let pid = unsafe { libc::fork() };
        match pid {
            -1 => panic!("fork failed: {}", io::Error::last_os_error()),
            0 => {
                // Child: attempt the redirect, then _exit.
                let result = redirect_stdio_to_devnull();
                unsafe {
                    libc::_exit(if result.is_ok() { 0 } else { 1 });
                }
            }
            child_pid => {
                // Parent: wait for the child and check its exit status.
                let mut status: libc::c_int = 0;
                let waited = unsafe { libc::waitpid(child_pid, &mut status, 0) };
                assert_ne!(waited, -1, "waitpid failed");
                assert!(
                    libc::WIFEXITED(status),
                    "child did not exit normally"
                );
                assert_eq!(
                    libc::WEXITSTATUS(status),
                    0,
                    "redirect_stdio_to_devnull failed in subprocess"
                );
            }
        }
    }
}
