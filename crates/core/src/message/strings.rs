#![allow(clippy::doc_markdown)]

//! # Overview
//!
//! The `strings` module centralises user-visible text tied to rsync's exit codes.
//! Upstream `log.c` uses the same table (`rerr_names`) when printing
//! `rsync error:` and `rsync warning:` diagnostics. Providing the mapping here
//! lets higher layers select the canonical wording without duplicating the
//! strings in multiple call sites.
//!
//! # Design
//!
//! The module exposes [`ExitCodeMessage`], a light-weight descriptor capturing
//! the severity, numeric exit code, and upstream text. Callers obtain instances
//! through [`exit_code_message`] and can immediately convert them into a
//! [`Message`] via [`ExitCodeMessage::to_message`]. This mirrors the behaviour
//! of upstream rsync where exit code 24 emits a warning while all other entries
//! are treated as errors.
//!
//! # Invariants
//!
//! - The mapping matches rsync 3.4.1's `rerr_names` table byte-for-byte.
//! - Exit code 24 is the only warning; all other known codes remain errors.
//! - Unknown codes return `None`, leaving higher layers to supply bespoke text.
//!
//! # Errors
//!
//! The helpers themselves never fail. Converting a template into a [`Message`]
//! only allocates when the caller subsequently renders the message into an
//! owned [`String`].
//!
//! # Examples
//!
//! Look up an exit code and render the canonical warning message.
//!
//! ```
//! use rsync_core::message::strings::exit_code_message;
//!
//! let template = exit_code_message(24).expect("exit code 24 is known");
//! let rendered = template
//!     .to_message()
//!     .with_role(rsync_core::message::Role::Receiver)
//!     .to_string();
//!
//! assert!(rendered.contains("rsync warning: some files vanished"));
//! assert!(rendered.contains("(code 24)"));
//! assert!(rendered.contains("[receiver=3.4.1-rust]"));
//! ```
//!
//! # See also
//!
//! - [`crate::message`] for the `Message` type used to render the output.

use super::{Message, Severity};

/// Template describing the canonical wording for a particular exit code.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExitCodeMessage {
    severity: Severity,
    code: i32,
    text: &'static str,
}

impl ExitCodeMessage {
    /// Creates a new template using the provided severity, numeric code, and text.
    #[must_use]
    pub const fn new(severity: Severity, code: i32, text: &'static str) -> Self {
        Self {
            severity,
            code,
            text,
        }
    }

    /// Returns the severity that upstream rsync uses for the exit code.
    #[must_use]
    pub const fn severity(self) -> Severity {
        self.severity
    }

    /// Returns the numeric exit code associated with the template.
    #[must_use]
    pub const fn code(self) -> i32 {
        self.code
    }

    /// Returns the canonical diagnostic text.
    #[must_use]
    pub const fn text(self) -> &'static str {
        self.text
    }

    /// Converts the template into a [`Message`] that mirrors upstream output.
    #[must_use]
    pub fn to_message(self) -> Message {
        match self.severity {
            Severity::Info => Message::info(self.text).with_code(self.code),
            Severity::Warning => Message::warning(self.text).with_code(self.code),
            Severity::Error => Message::error(self.code, self.text),
        }
    }
}

/// Returns the canonical template for the provided exit code, if known.
#[must_use]
pub fn exit_code_message(code: i32) -> Option<ExitCodeMessage> {
    use Severity::{Error, Warning};

    let severity = match code {
        24 => Warning,
        1 | 2 | 3 | 4 | 5 | 10 | 11 | 12 | 13 | 14 | 15 | 16 | 19 | 20 | 21 | 22 | 23 | 25 | 30
        | 35 | 124 | 125 | 126 | 127 => Error,
        _ => return None,
    };

    let text = match code {
        1 => "syntax or usage error",
        2 => "protocol incompatibility",
        3 => "errors selecting input/output files, dirs",
        4 => "requested action not supported",
        5 => "error starting client-server protocol",
        10 => "error in socket IO",
        11 => "error in file IO",
        12 => "error in rsync protocol data stream",
        13 => "errors with program diagnostics",
        14 => "error in IPC code",
        15 => "sibling process crashed",
        16 => "sibling process terminated abnormally",
        19 => "received SIGUSR1",
        20 => "received SIGINT, SIGTERM, or SIGHUP",
        21 => "waitpid() failed",
        22 => "error allocating core memory buffers",
        23 => "some files/attrs were not transferred (see previous errors)",
        24 => "some files vanished before they could be transferred",
        25 => "the --max-delete limit stopped deletions",
        30 => "timeout in data send/receive",
        35 => "timeout waiting for daemon connection",
        124 => "remote shell failed",
        125 => "remote shell killed",
        126 => "remote command could not be run",
        127 => "remote command not found",
        _ => unreachable!("severity guard should have filtered unknown codes"),
    };

    Some(ExitCodeMessage::new(severity, code, text))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_exit_code_returns_template() {
        let template = exit_code_message(23).expect("code 23 is mapped");
        assert_eq!(template.severity(), Severity::Error);
        assert_eq!(
            template.text(),
            "some files/attrs were not transferred (see previous errors)"
        );

        let message = template.to_message();
        assert_eq!(message.code(), Some(23));
        assert_eq!(message.severity(), Severity::Error);
        assert!(message.to_string().contains("(code 23)"));
    }

    #[test]
    fn vanished_files_exit_code_is_warning() {
        let template = exit_code_message(24).expect("code 24 is mapped");
        assert_eq!(template.severity(), Severity::Warning);
        assert_eq!(template.code(), 24);

        let rendered = template.to_message().to_string();
        assert!(rendered.starts_with("rsync warning:"));
        assert!(rendered.contains("(code 24)"));
    }

    #[test]
    fn unknown_exit_code_returns_none() {
        assert!(exit_code_message(-1).is_none());
        assert!(exit_code_message(0).is_none());
        assert!(exit_code_message(255).is_none());
    }
}
