#![deny(unsafe_code)]

//! Daemon error reporting helpers.
//!
//! The [`DaemonError`] type centralises exit-code handling and formatted
//! diagnostics for the daemon entry points. Keeping the implementation in a
//! dedicated module allows the sprawling runtime logic in `lib.rs` to focus on
//! protocol and configuration handling while still constructing consistent
//! messages that honour workspace branding conventions.
//!
//! # Exit Code Integration
//!
//! `DaemonError` uses the centralized [`ExitCode`] enum from the `core` crate
//! internally, ensuring consistent exit code handling across the workspace.
//! The i32 interface is preserved for backward compatibility.
//!
//! Note: This module uses manual `Error` and `Display` implementations rather
//! than thiserror because the workspace's `core` crate shadows Rust's primitive
//! `core`, which conflicts with thiserror's macro expansion.

use std::error::Error;
use std::fmt;

use core::exit_code::{ExitCode, HasExitCode};
use core::message::Message;

/// Error returned when daemon orchestration fails.
///
/// Uses the centralized [`ExitCode`] enum internally for type-safe exit code
/// handling while maintaining backward compatibility with i32 interfaces.
#[derive(Clone, Debug)]
pub struct DaemonError {
    exit_code: ExitCode,
    message: Message,
}

impl DaemonError {
    /// Creates a new [`DaemonError`] with a typed exit code.
    ///
    /// This is the preferred constructor when the exit code is known at compile time.
    pub(crate) const fn with_code(exit_code: ExitCode, message: Message) -> Self {
        Self { exit_code, message }
    }

    /// Creates a new [`DaemonError`] from the supplied message and i32 exit code.
    ///
    /// Unknown exit codes are mapped to [`ExitCode::PartialTransfer`] as a fallback.
    /// For type-safe construction, prefer [`with_code`](Self::with_code).
    pub(crate) fn new(exit_code: i32, message: Message) -> Self {
        let code = ExitCode::from_i32(exit_code).unwrap_or(ExitCode::PartialTransfer);
        Self::with_code(code, message)
    }

    /// Returns the typed exit code associated with this error.
    #[must_use]
    pub const fn code(&self) -> ExitCode {
        self.exit_code
    }

    /// Returns the exit code as i32 for backward compatibility.
    #[must_use]
    pub const fn exit_code(&self) -> i32 {
        self.exit_code.as_i32()
    }

    /// Returns the formatted diagnostic message that should be emitted.
    pub const fn message(&self) -> &Message {
        &self.message
    }
}

impl HasExitCode for DaemonError {
    fn exit_code(&self) -> ExitCode {
        self.exit_code
    }
}

impl fmt::Display for DaemonError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.message.fmt(f)
    }
}

impl Error for DaemonError {}

#[cfg(test)]
mod tests {
    use super::*;
    use core::message::Role;
    use core::rsync_error;

    mod daemon_error_tests {
        use super::*;

        #[test]
        fn new_and_exit_code() {
            let message = rsync_error!(5, "test daemon error").with_role(Role::Daemon);
            let error = DaemonError::new(5, message);

            assert_eq!(error.exit_code(), 5);
        }

        #[test]
        fn message_accessor() {
            let message = rsync_error!(10, "socket error").with_role(Role::Daemon);
            let error = DaemonError::new(10, message);

            let _ = error.message(); // Just verify accessor works
        }

        #[test]
        fn clone() {
            let message = rsync_error!(1, "cloneable error").with_role(Role::Daemon);
            let error = DaemonError::new(1, message);
            let cloned = error.clone();

            assert_eq!(error.exit_code(), cloned.exit_code());
        }

        #[test]
        fn debug_format() {
            let message = rsync_error!(2, "debug test").with_role(Role::Daemon);
            let error = DaemonError::new(2, message);
            let debug = format!("{error:?}");

            assert!(debug.contains("DaemonError"));
            assert!(debug.contains("exit_code"));
        }

        #[test]
        fn display_format() {
            let message = rsync_error!(3, "display message").with_role(Role::Daemon);
            let error = DaemonError::new(3, message);
            let display = format!("{error}");

            assert!(display.contains("display message"));
        }

        #[test]
        fn error_trait() {
            let message = rsync_error!(4, "error trait test").with_role(Role::Daemon);
            let error = DaemonError::new(4, message);

            // Verify it implements std::error::Error
            let _: &dyn std::error::Error = &error;
        }

        #[test]
        fn different_exit_codes() {
            for code in [0, 1, 2, 10, 23, 127] {
                let message = rsync_error!(code, "test {}", code).with_role(Role::Daemon);
                let error = DaemonError::new(code, message);
                assert_eq!(error.exit_code(), code);
            }
        }

        #[test]
        fn with_code_constructor() {
            let code = ExitCode::Protocol;
            let message = rsync_error!(code.as_i32(), "protocol error").with_role(Role::Daemon);
            let error = DaemonError::with_code(code, message);

            assert_eq!(error.code(), ExitCode::Protocol);
            assert_eq!(error.exit_code(), 2);
        }

        #[test]
        fn code_returns_typed_exit_code() {
            let code = ExitCode::FileIo;
            let message = rsync_error!(code.as_i32(), "io error").with_role(Role::Daemon);
            let error = DaemonError::with_code(code, message);

            assert_eq!(error.code(), ExitCode::FileIo);
        }

        #[test]
        fn new_uses_fallback_for_unknown_code() {
            let message = rsync_error!(999, "unknown code").with_role(Role::Daemon);
            let error = DaemonError::new(999, message);

            // Unknown exit codes fall back to PartialTransfer
            assert_eq!(error.code(), ExitCode::PartialTransfer);
        }

        #[test]
        fn has_exit_code_trait() {
            let code = ExitCode::Syntax;
            let message = rsync_error!(code.as_i32(), "syntax error").with_role(Role::Daemon);
            let error = DaemonError::with_code(code, message);

            // Test the HasExitCode trait
            let trait_code: ExitCode = HasExitCode::exit_code(&error);
            assert_eq!(trait_code, code);
        }
    }
}
