#![deny(unsafe_code)]
#![deny(missing_docs)]
#![deny(rustdoc::broken_intra_doc_links)]

//! crates/logging-sink/src/lib.rs
//!
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

pub use line_mode::LineMode;
pub use sink::{LineModeGuard, MessageSink, TryMapWriterError};
