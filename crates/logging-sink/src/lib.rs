#![deny(unsafe_code)]
#![deny(missing_docs)]
#![deny(rustdoc::broken_intra_doc_links)]

//! # Overview
//!
//! `logging-sink` provides reusable message sink primitives that operate on the
//! [`core::message::Message`] type shared across the Rust rsync
//! workspace. The focus is on streaming diagnostics to arbitrary writers
//! while reusing [`core::message::MessageScratch`]
//! instances so higher layers avoid repeated buffer initialisation when printing
//! large batches of messages.
//!
//! # Design
//!
//! The crate exposes [`MessageSink`], a lightweight wrapper around an
//! [`std::io::Write`] implementor. Each sink stores a
//! [`core::message::MessageScratch`] scratch buffer that is reused whenever a message is
//! rendered, matching upstream rsync's approach of keeping stack-allocated
//! buffers alive for the duration of a logging session. Callers can control
//! whether rendered messages end with a newline by selecting a [`LineMode`].
//!
//! On Unix platforms the crate also provides a [`syslog`] backend that routes
//! daemon-mode diagnostics through `syslog(3)` with a configurable facility
//! and tag, matching upstream rsync's `log.c` behaviour.
//!
//! # Invariants
//!
//! - The sink never clones message payloads; it streams the segments emitted by
//!   [`core::message::Message::render_to_writer_with_scratch`] or
//!   [`core::message::Message::render_line_to_writer_with_scratch`].
//! - Scratch buffers are reused across invocations so repeated writes avoid
//!   zeroing fresh storage.
//! - `LineMode::WithNewline` mirrors upstream rsync's default of printing each
//!   diagnostic on its own line.
//!
//! # Errors
//!
//! All operations surface [`std::io::Error`] values originating from the
//! underlying writer. When reserving buffer space fails, the error bubbles up
//! unchanged from [`core::message::Message`] rendering helpers.
//!
//! # Examples
//!
//! Stream two diagnostics into an in-memory buffer and inspect the output:
//!
//! ```ignore
//! use core::{message::Message, rsync_error, rsync_warning};
//! use logging_sink::{LineMode, MessageSink};
//!
//! let mut sink = MessageSink::new(Vec::new());
//! let vanished = rsync_warning!("some files vanished").with_code(24);
//! let partial = rsync_error!(23, "partial transfer");
//!
//! sink.write(&vanished).unwrap();
//! sink.write(&partial).unwrap();
//!
//! let output = String::from_utf8(sink.into_inner()).unwrap();
//! assert!(output.lines().all(|line| line.starts_with("rsync")));
//!
//! // Render a final message without appending a newline.
//! let mut final_sink = MessageSink::with_line_mode(Vec::new(), LineMode::WithoutNewline);
//! final_sink.write(Message::info("completed")).unwrap();
//! let buffer = final_sink.into_inner();
//! assert!(buffer.ends_with(b"completed"));
//! ```
//!
//! # See also
//!
//! - [`core::message`] for message construction and formatting helpers.
//! - `logging` crate for verbosity flags and the `info_log!`/`debug_log!` macros.

mod line_mode;
mod sink;
/// Syslog backend for daemon-mode logging.
///
/// Unix-only. Routes daemon diagnostics through the safe `syslog` crate
/// (BSD/RFC 3164 over a Unix socket, no libc FFI) when no explicit log file
/// is configured, matching upstream rsync's `log.c` behaviour.
#[cfg(unix)]
pub mod syslog;

pub use line_mode::LineMode;
pub use sink::{LineModeGuard, MessageSink, TryMapWriterError};

/// Canonical (lowercase) syslog facility names accepted in `rsyncd.conf`.
///
/// Mirrors upstream `loadparm.c`'s `enum_syslog_facility[]` table. The list is
/// platform-independent so daemon config parsing validates a `syslog facility`
/// directive identically on every target; the name-to-constant mapping used
/// when a connection actually opens syslog lives in the Unix-only
/// [`syslog::SyslogFacility`].
///
/// upstream: loadparm.c enum_syslog_facility[]
const SYSLOG_FACILITY_NAMES: &[&str] = &[
    "kern", "user", "mail", "daemon", "auth", "syslog", "lpr", "news", "uucp", "cron", "local0",
    "local1", "local2", "local3", "local4", "local5", "local6", "local7",
];

/// Returns the canonical (lowercase) form of a syslog facility name, or `None`
/// when `name` is not a recognised facility.
///
/// Matching is case-insensitive, matching upstream `loadparm.c`'s
/// `strequal()`-based enum lookup. This is the cross-platform validation entry
/// point for the daemon's `syslog facility` directive; unknown names are
/// rejected here before a module override is recorded.
///
/// upstream: loadparm.c:456 `case P_ENUM` walks `enum_syslog_facility[]`.
pub fn canonical_syslog_facility(name: &str) -> Option<&'static str> {
    SYSLOG_FACILITY_NAMES
        .iter()
        .copied()
        .find(|candidate| candidate.eq_ignore_ascii_case(name))
}

#[cfg(test)]
mod facility_tests {
    use super::*;

    #[test]
    fn canonical_facility_is_case_insensitive() {
        assert_eq!(canonical_syslog_facility("DAEMON"), Some("daemon"));
        assert_eq!(canonical_syslog_facility("Local3"), Some("local3"));
        assert_eq!(canonical_syslog_facility("local0"), Some("local0"));
    }

    #[test]
    fn canonical_facility_rejects_unknown() {
        // WHY: the daemon must reject a typo'd `syslog facility` so the module
        // silently inherits the global facility (upstream P_ENUM keep-default),
        // rather than routing logs to an unintended sink.
        assert_eq!(canonical_syslog_facility("bogus"), None);
        assert_eq!(canonical_syslog_facility("local8"), None);
        assert_eq!(canonical_syslog_facility("LOG_DAEMON"), None);
        assert_eq!(canonical_syslog_facility(""), None);
    }

    // WHY: the cross-platform name table used for config validation and the
    // Unix-only name-to-constant map used to open syslog must never drift out
    // of sync; a name accepted by one but not the other would validate a
    // directive that then silently falls back to the default facility.
    #[cfg(unix)]
    #[test]
    fn canonical_names_map_to_a_facility_constant() {
        for name in SYSLOG_FACILITY_NAMES {
            assert!(
                syslog::SyslogFacility::from_name(name).is_some(),
                "facility name '{name}' accepted by validator but unmapped in SyslogFacility"
            );
        }
    }
}
