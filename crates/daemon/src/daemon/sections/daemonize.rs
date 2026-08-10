/// Detaches the current process from the terminal by forking, creating a new
/// session, and redirecting stdin/stdout/stderr to `/dev/null`.
///
/// Delegates to `platform::daemonize::become_daemon()`.
///
/// upstream: clientserver.c:1463 - `become_daemon()`
#[cfg(unix)]
fn become_daemon() -> Result<(), DaemonError> {
    platform::daemonize::become_daemon().map_err(|e| daemonize_error("become_daemon", e))
}

/// Creates a [`DaemonError`] for daemonization failures.
#[cfg(unix)]
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

#[cfg(all(test, unix))]
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
}
