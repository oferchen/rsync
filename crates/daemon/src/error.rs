#![deny(unsafe_code)]

//! Daemon error reporting helpers.
//!
//! The [`DaemonError`] type centralises exit-code handling and formatted
//! diagnostics for the daemon entry points. Keeping the implementation in a
//! dedicated module allows the sprawling runtime logic in `lib.rs` to focus on
//! protocol and configuration handling while still constructing consistent
//! messages that honour workspace branding conventions.

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
    pub(crate) fn new(exit_code: i32, message: Message) -> Self {
        Self { exit_code, message }
    }

    /// Returns the exit code associated with this error.
    #[must_use]
    pub const fn exit_code(&self) -> i32 {
        self.exit_code
    }

    /// Returns the formatted diagnostic message that should be emitted.
    pub fn message(&self) -> &Message {
        &self.message
    }
}

impl fmt::Display for DaemonError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.message.fmt(f)
    }
}

impl Error for DaemonError {}
