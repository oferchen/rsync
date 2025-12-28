#![deny(unsafe_code)]

//! Daemon error reporting helpers.
//!
//! The [`DaemonError`] type centralises exit-code handling and formatted
//! diagnostics for the daemon entry points. Keeping the implementation in a
//! dedicated module allows the sprawling runtime logic in `lib.rs` to focus on
//! protocol and configuration handling while still constructing consistent
//! messages that honour workspace branding conventions.
//!
//! Note: This module uses manual `Error` and `Display` implementations rather
//! than thiserror because the workspace's `core` crate shadows Rust's primitive
//! `core`, which conflicts with thiserror's macro expansion.

use std::error::Error;
use std::fmt;

use core::message::Message;

/// Error returned when daemon orchestration fails.
#[derive(Clone, Debug)]
pub struct DaemonError {
    exit_code: i32,
    message: Message,
}

impl DaemonError {
    /// Creates a new [`DaemonError`] from the supplied message and exit code.
    pub(crate) const fn new(exit_code: i32, message: Message) -> Self {
        Self { exit_code, message }
    }

    /// Returns the exit code associated with this error.
    #[must_use]
    pub const fn exit_code(&self) -> i32 {
        self.exit_code
    }

    /// Returns the formatted diagnostic message that should be emitted.
    pub const fn message(&self) -> &Message {
        &self.message
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
    }
}
