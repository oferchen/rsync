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
//! [`Message`] via [`ExitCodeMessage::to_message`] or the blanket
//! [`From<ExitCodeMessage>`](ExitCodeMessage#impl-From%3CExitCodeMessage%3E-for-Message)
//! implementation. When only the severity is required, use
//! [`exit_code_severity`] to query the table without materialising a full
//! [`Message`]. This mirrors the behaviour of upstream rsync where exit code 24
//! emits a warning while all other entries are treated as errors.
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
//! assert!(rendered.contains(&format!(
//!     "[receiver={}]",
//!     rsync_core::version::RUST_VERSION
//! )));
//! ```
//!
//! Convert a template directly into a [`Message`] without calling
//! [`ExitCodeMessage::to_message`].
//!
//! ```
//! use rsync_core::message::{strings::exit_code_message, Message, Severity};
//!
//! let template = exit_code_message(23).expect("exit code 23 is known");
//! let message: Message = template.into();
//!
//! assert_eq!(message.code(), Some(23));
//! assert_eq!(message.severity(), Severity::Error);
//! ```
//!
//! Inspect the severity classification without constructing a full
//! [`ExitCodeMessage`].
//!
//! ```
//! use rsync_core::message::strings::exit_code_severity;
//! use rsync_core::message::Severity;
//!
//! assert_eq!(exit_code_severity(24), Some(Severity::Warning));
//! assert_eq!(exit_code_severity(23), Some(Severity::Error));
//! assert_eq!(exit_code_severity(7), None);
//! ```
//!
//! # See also
//!
//! - [`crate::message`] for the `Message` type used to render the output.
//! - [`crate::message::Message::from_exit_code`] builds the canonical diagnostic directly.

use super::{Message, Severity};

/// Sorted table mirroring upstream `rerr_names` entries.
const EXIT_CODE_TABLE: [ExitCodeMessage; 25] = [
    ExitCodeMessage::new(Severity::Error, 1, "syntax or usage error"),
    ExitCodeMessage::new(Severity::Error, 2, "protocol incompatibility"),
    ExitCodeMessage::new(
        Severity::Error,
        3,
        "errors selecting input/output files, dirs",
    ),
    ExitCodeMessage::new(Severity::Error, 4, "requested action not supported"),
    ExitCodeMessage::new(Severity::Error, 5, "error starting client-server protocol"),
    ExitCodeMessage::new(Severity::Error, 10, "error in socket IO"),
    ExitCodeMessage::new(Severity::Error, 11, "error in file IO"),
    ExitCodeMessage::new(Severity::Error, 12, "error in rsync protocol data stream"),
    ExitCodeMessage::new(Severity::Error, 13, "errors with program diagnostics"),
    ExitCodeMessage::new(Severity::Error, 14, "error in IPC code"),
    ExitCodeMessage::new(Severity::Error, 15, "sibling process crashed"),
    ExitCodeMessage::new(Severity::Error, 16, "sibling process terminated abnormally"),
    ExitCodeMessage::new(Severity::Error, 19, "received SIGUSR1"),
    ExitCodeMessage::new(Severity::Error, 20, "received SIGINT, SIGTERM, or SIGHUP"),
    ExitCodeMessage::new(Severity::Error, 21, "waitpid() failed"),
    ExitCodeMessage::new(Severity::Error, 22, "error allocating core memory buffers"),
    ExitCodeMessage::new(
        Severity::Error,
        23,
        "some files/attrs were not transferred (see previous errors)",
    ),
    ExitCodeMessage::new(
        Severity::Warning,
        24,
        "some files vanished before they could be transferred",
    ),
    ExitCodeMessage::new(
        Severity::Error,
        25,
        "the --max-delete limit stopped deletions",
    ),
    ExitCodeMessage::new(Severity::Error, 30, "timeout in data send/receive"),
    ExitCodeMessage::new(Severity::Error, 35, "timeout waiting for daemon connection"),
    ExitCodeMessage::new(Severity::Error, 124, "remote shell failed"),
    ExitCodeMessage::new(Severity::Error, 125, "remote shell killed"),
    ExitCodeMessage::new(Severity::Error, 126, "remote command could not be run"),
    ExitCodeMessage::new(Severity::Error, 127, "remote command not found"),
];

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
    #[must_use = "the constructed Message should be rendered so the exit-code diagnostic reaches the user"]
    pub fn to_message(self) -> Message {
        message_from_template(self)
    }
}

impl From<ExitCodeMessage> for Message {
    fn from(value: ExitCodeMessage) -> Self {
        message_from_template(value)
    }
}

fn message_from_template(template: ExitCodeMessage) -> Message {
    Message::new(template.severity, template.text).with_code(template.code)
}

/// Returns the canonical template for the provided exit code, if known.
///
/// # Examples
///
/// ```
/// use rsync_core::message::strings::exit_code_message;
///
/// let template = exit_code_message(35).expect("code 35 is mapped");
/// assert_eq!(template.code(), 35);
///
/// // `exit_code_message` is a `const fn`, so lookups can happen at compile time.
/// const REMOTE_SHELL_FAILURE: Option<rsync_core::message::strings::ExitCodeMessage> =
///     exit_code_message(124);
/// assert!(REMOTE_SHELL_FAILURE.is_some());
/// ```
#[doc(alias = "rerr_names")]
#[must_use]
pub const fn exit_code_message(code: i32) -> Option<ExitCodeMessage> {
    let mut low = 0;
    let mut high = EXIT_CODE_TABLE.len();

    while low < high {
        let mid = low + (high - low) / 2;
        let entry = EXIT_CODE_TABLE[mid];
        let entry_code = entry.code();

        if entry_code < code {
            low = mid + 1;
        } else if entry_code > code {
            high = mid;
        } else {
            return Some(entry);
        }
    }

    None
}

/// Returns the severity classification for the provided exit code, if known.
///
/// The helper allows higher layers to branch on rsync's canonical error vs
/// warning split without constructing a full [`ExitCodeMessage`]. The result is
/// derived from the same lookup table used by [`exit_code_message`], ensuring
/// both helpers stay in sync.
///
/// # Examples
///
/// ```
/// use rsync_core::message::strings::exit_code_severity;
/// use rsync_core::message::Severity;
///
/// assert_eq!(exit_code_severity(24), Some(Severity::Warning));
/// assert_eq!(exit_code_severity(23), Some(Severity::Error));
/// assert_eq!(exit_code_severity(0), None);
/// ```
#[must_use]
pub const fn exit_code_severity(code: i32) -> Option<Severity> {
    match exit_code_message(code) {
        Some(template) => Some(template.severity()),
        None => None,
    }
}

/// Returns the full table of known exit-code templates.
///
/// The slice mirrors upstream rsync's `rerr_names` array, including the
/// downgraded severity for exit code 24. Callers can iterate over the entries to
/// build aggregated documentation or validation tables without hard-coding the
/// mapping in multiple places.
///
/// # Examples
///
/// ```
/// use rsync_core::message::strings::exit_code_messages;
///
/// let templates = exit_code_messages();
/// assert!(templates.iter().any(|entry| entry.code() == 24));
/// ```
#[must_use]
pub const fn exit_code_messages() -> &'static [ExitCodeMessage] {
    &EXIT_CODE_TABLE
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
        for code in [-1, 0, 6, 200, 255] {
            assert!(
                exit_code_message(code).is_none(),
                "unexpected mapping for {code}"
            );
        }
    }

    #[test]
    fn exit_code_table_is_strictly_increasing() {
        let mut previous = None;

        for entry in EXIT_CODE_TABLE {
            if let Some(prev) = previous {
                assert!(
                    prev < entry.code(),
                    "exit code table must be sorted without duplicates",
                );
            }

            previous = Some(entry.code());
        }
    }

    #[test]
    fn exit_code_messages_exposes_full_table() {
        let slice = exit_code_messages();
        assert_eq!(slice.len(), EXIT_CODE_TABLE.len());
        assert_eq!(slice.first(), EXIT_CODE_TABLE.first());
        assert_eq!(slice.last(), EXIT_CODE_TABLE.last());
    }

    #[test]
    fn exit_code_severity_matches_templates() {
        assert_eq!(exit_code_severity(24), Some(Severity::Warning));
        assert_eq!(exit_code_severity(23), Some(Severity::Error));
        assert_eq!(exit_code_severity(7), None);

        for entry in EXIT_CODE_TABLE {
            assert_eq!(exit_code_severity(entry.code()), Some(entry.severity()));
        }
    }

    #[test]
    fn exit_code_message_converts_into_message() {
        let template = exit_code_message(25).expect("code 25 is mapped");

        let from_method = template.to_message();
        let via_into: Message = template.into();

        assert_eq!(via_into.code(), Some(25));
        assert_eq!(via_into.severity(), Severity::Error);
        assert_eq!(via_into.text(), from_method.text());
    }

    #[test]
    fn only_exit_code_twenty_four_is_a_warning() {
        let mut warnings = EXIT_CODE_TABLE
            .iter()
            .filter(|entry| entry.severity() == Severity::Warning);

        let warning = warnings
            .next()
            .expect("exit code 24 must produce a warning entry");
        assert_eq!(warning.code(), 24);
        assert!(
            warnings.next().is_none(),
            "exit code table must not contain additional warning severities"
        );
    }

    #[test]
    fn exit_code_message_is_const_evaluable() {
        const TEMPLATE: Option<ExitCodeMessage> = exit_code_message(124);
        let template = TEMPLATE.expect("code 124 is mapped");
        assert_eq!(template.code(), 124);
        assert_eq!(template.severity(), Severity::Error);
    }
}
