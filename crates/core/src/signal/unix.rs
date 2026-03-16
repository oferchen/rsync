//! Unix signal handling implementation.
//!
//! This module provides the actual signal handling implementation for Unix systems
//! using raw libc signal handlers. Signal handlers must be async-signal-safe, so
//! they only set atomic flags and do no allocation or locking.

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::exit_code::ExitCode;

/// Reasons for shutdown triggered by signals or programmatic requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum ShutdownReason {
    /// SIGINT received (Ctrl+C).
    Interrupted = 1,
    /// SIGTERM received.
    Terminated = 2,
    /// SIGHUP received.
    HangUp = 3,
    /// SIGPIPE received (broken pipe/connection).
    PipeBroken = 4,
    /// Programmatic shutdown request.
    UserRequested = 5,
}

impl ShutdownReason {
    /// Converts a u8 code back to a ShutdownReason.
    ///
    /// Returns `None` if the code is 0 (no shutdown) or invalid.
    #[must_use]
    pub(crate) const fn from_u8(code: u8) -> Option<Self> {
        match code {
            1 => Some(Self::Interrupted),
            2 => Some(Self::Terminated),
            3 => Some(Self::HangUp),
            4 => Some(Self::PipeBroken),
            5 => Some(Self::UserRequested),
            _ => None,
        }
    }

    /// Returns the appropriate exit code for this shutdown reason.
    ///
    /// Maps shutdown reasons to rsync exit codes:
    /// - SIGINT/SIGTERM/SIGHUP -> ExitCode::Signal (20)
    /// - SIGPIPE -> ExitCode::SocketIo (10)
    /// - UserRequested -> ExitCode::Ok (0)
    #[must_use]
    pub const fn exit_code(self) -> ExitCode {
        match self {
            Self::Interrupted | Self::Terminated | Self::HangUp => ExitCode::Signal,
            Self::PipeBroken => ExitCode::SocketIo,
            Self::UserRequested => ExitCode::Ok,
        }
    }

    /// Returns a human-readable description of the shutdown reason.
    #[must_use]
    pub const fn description(self) -> &'static str {
        match self {
            Self::Interrupted => "interrupted by SIGINT",
            Self::Terminated => "terminated by SIGTERM",
            Self::HangUp => "hangup by SIGHUP",
            Self::PipeBroken => "broken pipe (SIGPIPE)",
            Self::UserRequested => "user requested shutdown",
        }
    }
}

impl std::fmt::Display for ShutdownReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.description())
    }
}

/// Signal handler state.
///
/// This type coordinates signal handling installation and provides methods
/// to check signal state during operations.
#[derive(Debug)]
pub struct SignalHandler {
    #[allow(dead_code)]
    installed: bool,
}

impl SignalHandler {
    /// Checks if a graceful shutdown has been requested.
    ///
    /// Returns `true` if any signal handler has set the shutdown flag.
    #[inline]
    #[must_use]
    pub fn is_shutdown_requested(&self) -> bool {
        super::is_shutdown_requested()
    }

    /// Checks if an immediate abort has been requested.
    ///
    /// Returns `true` if a second signal was received.
    #[inline]
    #[must_use]
    pub fn is_abort_requested(&self) -> bool {
        super::is_abort_requested()
    }

    /// Returns the reason for shutdown, if any.
    #[inline]
    #[must_use]
    pub fn shutdown_reason(&self) -> Option<ShutdownReason> {
        super::shutdown_reason()
    }
}

impl Drop for SignalHandler {
    fn drop(&mut self) {
        // We intentionally do NOT restore signal handlers on drop.
        // Once installed, signal handlers should remain active for the
        // lifetime of the process. Restoring them could leave the process
        // vulnerable to signals during cleanup.
    }
}

/// Flag to track if this is the first or second signal.
static SIGNAL_COUNT: AtomicBool = AtomicBool::new(false);

/// Signal handler for SIGINT (Ctrl+C).
///
/// First signal: Sets graceful shutdown flag.
/// Second signal: Sets abort flag for immediate termination.
extern "C" fn handle_sigint(_signum: libc::c_int) {
    // Check if this is the first or second signal
    let already_signaled = SIGNAL_COUNT.swap(true, Ordering::SeqCst);

    if already_signaled {
        // Second signal - abort immediately
        super::request_abort();
    } else {
        // First signal - graceful shutdown
        super::request_shutdown(ShutdownReason::Interrupted);
    }
}

/// Signal handler for SIGTERM.
///
/// First signal: Sets graceful shutdown flag.
/// Second signal: Sets abort flag for immediate termination.
extern "C" fn handle_sigterm(_signum: libc::c_int) {
    let already_signaled = SIGNAL_COUNT.swap(true, Ordering::SeqCst);

    if already_signaled {
        super::request_abort();
    } else {
        super::request_shutdown(ShutdownReason::Terminated);
    }
}

/// Signal handler for SIGHUP.
///
/// First signal: Sets graceful shutdown flag.
/// Second signal: Sets abort flag for immediate termination.
extern "C" fn handle_sighup(_signum: libc::c_int) {
    let already_signaled = SIGNAL_COUNT.swap(true, Ordering::SeqCst);

    if already_signaled {
        super::request_abort();
    } else {
        super::request_shutdown(ShutdownReason::HangUp);
    }
}

/// Signal handler for SIGPIPE.
///
/// Treats broken pipe as immediate graceful shutdown (connection lost).
extern "C" fn handle_sigpipe(_signum: libc::c_int) {
    super::request_shutdown(ShutdownReason::PipeBroken);
}

/// Installs signal handlers for graceful shutdown.
///
/// This function installs handlers for SIGINT, SIGTERM, SIGHUP, and SIGPIPE.
/// Signal handlers use atomic operations only and are async-signal-safe.
///
/// # Errors
///
/// Returns an error if signal handler installation fails. This is rare and
/// typically indicates a serious system problem.
///
/// # Examples
///
/// ```no_run
/// use core::signal::install_signal_handlers;
///
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let handler = install_signal_handlers()?;
///
/// // Check for shutdown during operations
/// if handler.is_shutdown_requested() {
///     println!("Shutting down gracefully...");
/// }
/// # Ok(())
/// # }
/// ```
pub fn install_signal_handlers() -> io::Result<SignalHandler> {
    unsafe {
        // Install SIGINT handler
        let mut sa_int: libc::sigaction = std::mem::zeroed();
        sa_int.sa_sigaction = handle_sigint as libc::sighandler_t;
        sa_int.sa_flags = libc::SA_RESTART; // Restart interrupted syscalls
        libc::sigemptyset(&mut sa_int.sa_mask as *mut libc::sigset_t);

        if libc::sigaction(libc::SIGINT, &sa_int, std::ptr::null_mut()) != 0 {
            return Err(io::Error::last_os_error());
        }

        // Install SIGTERM handler
        let mut sa_term: libc::sigaction = std::mem::zeroed();
        sa_term.sa_sigaction = handle_sigterm as libc::sighandler_t;
        sa_term.sa_flags = libc::SA_RESTART;
        libc::sigemptyset(&mut sa_term.sa_mask as *mut libc::sigset_t);

        if libc::sigaction(libc::SIGTERM, &sa_term, std::ptr::null_mut()) != 0 {
            return Err(io::Error::last_os_error());
        }

        // Install SIGHUP handler
        let mut sa_hup: libc::sigaction = std::mem::zeroed();
        sa_hup.sa_sigaction = handle_sighup as libc::sighandler_t;
        sa_hup.sa_flags = libc::SA_RESTART;
        libc::sigemptyset(&mut sa_hup.sa_mask as *mut libc::sigset_t);

        if libc::sigaction(libc::SIGHUP, &sa_hup, std::ptr::null_mut()) != 0 {
            return Err(io::Error::last_os_error());
        }

        // Install SIGPIPE handler (ignore by catching it)
        let mut sa_pipe: libc::sigaction = std::mem::zeroed();
        sa_pipe.sa_sigaction = handle_sigpipe as libc::sighandler_t;
        sa_pipe.sa_flags = libc::SA_RESTART;
        libc::sigemptyset(&mut sa_pipe.sa_mask as *mut libc::sigset_t);

        if libc::sigaction(libc::SIGPIPE, &sa_pipe, std::ptr::null_mut()) != 0 {
            return Err(io::Error::last_os_error());
        }
    }

    Ok(SignalHandler { installed: true })
}

/// Blocks the current thread waiting for a signal.
///
/// This function is useful for daemon processes that need to wait for
/// shutdown signals. It blocks until a signal is received and returns
/// the shutdown reason.
///
/// # Examples
///
/// ```no_run
/// use core::signal::{install_signal_handlers, wait_for_signal};
///
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// install_signal_handlers()?;
///
/// println!("Daemon running, waiting for signal...");
/// let reason = wait_for_signal();
/// println!("Received signal: {}", reason);
/// # Ok(())
/// # }
/// ```
pub fn wait_for_signal() -> ShutdownReason {
    loop {
        if let Some(reason) = super::shutdown_reason() {
            return reason;
        }

        // Sleep briefly to avoid busy-waiting
        // In a real implementation, this would use sigwait() or similar
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shutdown_reason_exit_codes() {
        assert_eq!(ShutdownReason::Interrupted.exit_code(), ExitCode::Signal);
        assert_eq!(ShutdownReason::Terminated.exit_code(), ExitCode::Signal);
        assert_eq!(ShutdownReason::HangUp.exit_code(), ExitCode::Signal);
        assert_eq!(ShutdownReason::PipeBroken.exit_code(), ExitCode::SocketIo);
        assert_eq!(ShutdownReason::UserRequested.exit_code(), ExitCode::Ok);
    }

    #[test]
    fn shutdown_reason_descriptions() {
        assert_eq!(
            ShutdownReason::Interrupted.description(),
            "interrupted by SIGINT"
        );
        assert_eq!(
            ShutdownReason::Terminated.description(),
            "terminated by SIGTERM"
        );
        assert_eq!(ShutdownReason::HangUp.description(), "hangup by SIGHUP");
        assert_eq!(
            ShutdownReason::PipeBroken.description(),
            "broken pipe (SIGPIPE)"
        );
        assert_eq!(
            ShutdownReason::UserRequested.description(),
            "user requested shutdown"
        );
    }

    #[test]
    fn shutdown_reason_display() {
        assert_eq!(
            format!("{}", ShutdownReason::Interrupted),
            "interrupted by SIGINT"
        );
        assert_eq!(
            format!("{}", ShutdownReason::Terminated),
            "terminated by SIGTERM"
        );
    }

    #[test]
    fn shutdown_reason_from_u8_roundtrips() {
        for reason in [
            ShutdownReason::Interrupted,
            ShutdownReason::Terminated,
            ShutdownReason::HangUp,
            ShutdownReason::PipeBroken,
            ShutdownReason::UserRequested,
        ] {
            let code = reason as u8;
            assert_eq!(ShutdownReason::from_u8(code), Some(reason));
        }
    }

    #[test]
    fn shutdown_reason_from_u8_invalid() {
        assert_eq!(ShutdownReason::from_u8(0), None);
        assert_eq!(ShutdownReason::from_u8(99), None);
        assert_eq!(ShutdownReason::from_u8(255), None);
    }

    #[test]
    fn signal_handler_methods() {
        super::super::reset_for_testing();
        let handler = SignalHandler { installed: true };

        assert!(!handler.is_shutdown_requested());
        assert!(!handler.is_abort_requested());
        assert!(handler.shutdown_reason().is_none());

        super::super::request_shutdown(ShutdownReason::Interrupted);
        assert!(handler.is_shutdown_requested());
        assert_eq!(handler.shutdown_reason(), Some(ShutdownReason::Interrupted));
    }

    #[test]
    fn install_signal_handlers_succeeds() {
        // This test actually installs signal handlers
        let result = install_signal_handlers();
        assert!(result.is_ok());

        let handler = result.unwrap();
        assert!(handler.installed);
    }

    #[test]
    fn signal_count_tracks_multiple_signals() {
        // Reset state
        SIGNAL_COUNT.store(false, Ordering::SeqCst);
        super::super::reset_for_testing();

        // Simulate first SIGINT
        handle_sigint(libc::SIGINT);
        assert!(super::super::is_shutdown_requested());
        assert!(!super::super::is_abort_requested());

        // Simulate second SIGINT
        handle_sigint(libc::SIGINT);
        assert!(super::super::is_abort_requested());
    }

    #[test]
    fn sigpipe_causes_immediate_shutdown() {
        super::super::reset_for_testing();

        handle_sigpipe(libc::SIGPIPE);
        assert!(super::super::is_shutdown_requested());
        assert_eq!(
            super::super::shutdown_reason(),
            Some(ShutdownReason::PipeBroken)
        );
    }
}
