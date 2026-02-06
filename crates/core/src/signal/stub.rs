//! Stub signal handling for non-Unix platforms.
//!
//! This module provides a minimal signal handling implementation for platforms
//! that don't support Unix signals (primarily Windows). It provides the same
//! API but with limited functionality.

use std::io;

use crate::exit_code::ExitCode;

/// Reasons for shutdown (stub implementation).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum ShutdownReason {
    /// Interrupt signal (Ctrl+C on Windows).
    Interrupted = 1,
    /// Termination request.
    Terminated = 2,
    /// Hangup (not supported on Windows).
    HangUp = 3,
    /// Broken pipe (not supported on Windows).
    PipeBroken = 4,
    /// Programmatic shutdown request.
    UserRequested = 5,
}

impl ShutdownReason {
    /// Converts a u8 code back to a ShutdownReason.
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
            Self::Interrupted => "interrupted",
            Self::Terminated => "terminated",
            Self::HangUp => "hangup",
            Self::PipeBroken => "broken pipe",
            Self::UserRequested => "user requested shutdown",
        }
    }
}

impl std::fmt::Display for ShutdownReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.description())
    }
}

/// Signal handler state (stub implementation).
#[derive(Debug)]
pub struct SignalHandler {
    installed: bool,
}

impl SignalHandler {
    /// Checks if a graceful shutdown has been requested.
    #[inline]
    #[must_use]
    pub fn is_shutdown_requested(&self) -> bool {
        super::is_shutdown_requested()
    }

    /// Checks if an immediate abort has been requested.
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

/// Installs signal handlers (stub implementation).
///
/// On non-Unix platforms, this function does minimal setup and returns
/// a handler that can be used to check for programmatic shutdown requests.
///
/// # Errors
///
/// This stub implementation never returns an error.
pub fn install_signal_handlers() -> io::Result<SignalHandler> {
    // On Windows, we could use SetConsoleCtrlHandler here
    // For now, just return a stub handler
    Ok(SignalHandler { installed: true })
}

/// Blocks waiting for a signal (stub implementation).
///
/// This stub implementation waits for programmatic shutdown requests only.
pub fn wait_for_signal() -> ShutdownReason {
    loop {
        if let Some(reason) = super::shutdown_reason() {
            return reason;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_signal_handlers_succeeds() {
        let result = install_signal_handlers();
        assert!(result.is_ok());
    }

    #[test]
    fn signal_handler_methods_work() {
        super::super::reset_for_testing();
        let handler = SignalHandler { installed: true };

        assert!(!handler.is_shutdown_requested());
        assert!(!handler.is_abort_requested());
        assert!(handler.shutdown_reason().is_none());
    }

    #[test]
    fn shutdown_reason_descriptions() {
        assert_eq!(ShutdownReason::Interrupted.description(), "interrupted");
        assert_eq!(ShutdownReason::Terminated.description(), "terminated");
    }
}
