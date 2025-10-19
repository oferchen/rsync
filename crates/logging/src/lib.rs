#![deny(unsafe_code)]
#![deny(missing_docs)]
#![deny(rustdoc::broken_intra_doc_links)]

//! # Overview
//!
//! `rsync_logging` provides reusable logging primitives that operate on the
//! [`Message`](rsync_core::message::Message) type shared across the Rust rsync
//! workspace. The initial focus is on streaming diagnostics to arbitrary writers
//! while reusing [`MessageScratch`](rsync_core::message::MessageScratch)
//! instances so higher layers avoid repeated buffer initialisation when printing
//! large batches of messages.
//!
//! # Design
//!
//! The crate exposes [`MessageSink`], a lightweight wrapper around an
//! [`io::Write`](std::io::Write) implementor. Each sink stores a
//! [`MessageScratch`] scratch buffer that is reused whenever a message is
//! rendered, matching upstream rsync's approach of keeping stack-allocated
//! buffers alive for the duration of a logging session. Callers can control
//! whether rendered messages end with a newline by selecting a [`LineMode`].
//!
//! # Invariants
//!
//! - The sink never clones message payloads; it streams the segments emitted by
//!   [`Message::render_to_writer_with_scratch`] or
//!   [`Message::render_line_to_writer_with_scratch`].
//! - Scratch buffers are reused across invocations so repeated writes avoid
//!   zeroing fresh storage.
//! - `LineMode::WithNewline` mirrors upstream rsync's default of printing each
//!   diagnostic on its own line.
//!
//! # Errors
//!
//! All operations surface [`std::io::Error`] values originating from the
//! underlying writer. When reserving buffer space fails, the error bubbles up
//! unchanged from [`Message`] rendering helpers.
//!
//! # Examples
//!
//! Stream two diagnostics into an in-memory buffer and inspect the output:
//!
//! ```
//! use rsync_core::{message::Message, rsync_error, rsync_warning};
//! use rsync_logging::{LineMode, MessageSink};
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
//! final_sink.write(&Message::info("completed")).unwrap();
//! let buffer = final_sink.into_inner();
//! assert!(buffer.ends_with(b"completed"));
//! ```
//!
//! # See also
//!
//! - [`rsync_core::message`] for message construction and formatting helpers.
//! - Future logging backends will reuse [`MessageSink`] to route diagnostics to
//!   stdout/stderr, log files, or journald.

use std::borrow::Borrow;
use std::io::{self, Write};

use rsync_core::message::{Message, MessageScratch};

/// Controls whether a [`MessageSink`] appends a trailing newline when writing messages.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LineMode {
    /// Append a newline terminator after each rendered message.
    WithNewline,
    /// Emit the rendered message without a trailing newline.
    WithoutNewline,
}

impl LineMode {
    const fn append_newline(self) -> bool {
        matches!(self, Self::WithNewline)
    }
}

impl Default for LineMode {
    fn default() -> Self {
        Self::WithNewline
    }
}

/// Streaming sink that renders [`Message`] values into an [`io::Write`] target.
///
/// The sink owns the underlying writer together with a reusable
/// [`MessageScratch`] buffer. Each call to [`write`](Self::write) renders the
/// supplied message using the configured [`LineMode`], mirroring upstream
/// rsync's line-oriented diagnostics by default. The helper keeps all state on
/// the stack, making it inexpensive to clone or move the sink when logging
/// contexts change.
///
/// # Examples
///
/// Collect diagnostics into a [`Vec<u8>`] with newline terminators:
///
/// ```
/// use rsync_core::message::Message;
/// use rsync_logging::MessageSink;
///
/// let mut sink = MessageSink::new(Vec::new());
///
/// sink.write(&Message::warning("vanished"))?;
/// sink.write(&Message::error(23, "partial"))?;
///
/// let output = String::from_utf8(sink.into_inner()).unwrap();
/// assert!(output.ends_with('\n'));
/// # Ok::<(), std::io::Error>(())
/// ```
///
/// Render a message without appending a newline:
///
/// ```
/// use rsync_core::message::Message;
/// use rsync_logging::{LineMode, MessageSink};
///
/// let mut sink = MessageSink::with_line_mode(Vec::new(), LineMode::WithoutNewline);
/// sink.write(&Message::info("ready"))?;
///
/// assert_eq!(sink.into_inner(), b"rsync info: ready".to_vec());
/// # Ok::<(), std::io::Error>(())
/// ```
///
/// Reuse an existing [`MessageScratch`] when constructing a new sink:
///
/// ```
/// use rsync_core::message::{Message, MessageScratch};
/// use rsync_logging::{LineMode, MessageSink};
///
/// let mut sink = MessageSink::with_parts(Vec::new(), MessageScratch::new(), LineMode::WithoutNewline);
/// sink.write(&Message::info("phase one"))?;
/// let (writer, scratch, mode) = sink.into_parts();
/// assert_eq!(mode, LineMode::WithoutNewline);
///
/// let mut sink = MessageSink::with_parts(writer, scratch, LineMode::WithNewline);
/// sink.write(&Message::warning("phase two"))?;
/// let output = String::from_utf8(sink.into_inner()).unwrap();
/// assert!(output.contains("phase two"));
/// # Ok::<(), std::io::Error>(())
/// ```
#[doc(alias = "--msgs2stderr")]
#[derive(Clone, Debug)]
pub struct MessageSink<W> {
    writer: W,
    scratch: MessageScratch,
    line_mode: LineMode,
}

impl<W> MessageSink<W> {
    /// Creates a new sink that appends a newline after each rendered message.
    #[must_use]
    pub fn new(writer: W) -> Self {
        Self::with_line_mode(writer, LineMode::WithNewline)
    }

    /// Creates a sink with the provided [`LineMode`].
    #[must_use]
    pub fn with_line_mode(writer: W, line_mode: LineMode) -> Self {
        Self::with_parts(writer, MessageScratch::new(), line_mode)
    }

    /// Creates a sink from an explicit [`MessageScratch`] and [`LineMode`].
    ///
    /// Higher layers that manage scratch buffers manually can reuse their
    /// allocations across sinks by passing the existing scratch value into this
    /// constructor. The [`MessageScratch`] is stored by value, mirroring the
    /// ownership model used throughout the workspace to avoid hidden
    /// allocations.
    #[must_use]
    pub fn with_parts(writer: W, scratch: MessageScratch, line_mode: LineMode) -> Self {
        Self {
            writer,
            scratch,
            line_mode,
        }
    }

    /// Returns the current [`LineMode`].
    #[must_use]
    pub const fn line_mode(&self) -> LineMode {
        self.line_mode
    }

    /// Updates the [`LineMode`] used for subsequent writes.
    pub fn set_line_mode(&mut self, line_mode: LineMode) {
        self.line_mode = line_mode;
    }

    /// Borrows the underlying writer.
    #[must_use]
    pub fn get_ref(&self) -> &W {
        &self.writer
    }

    /// Mutably borrows the underlying writer.
    #[must_use]
    pub fn get_mut(&mut self) -> &mut W {
        &mut self.writer
    }

    /// Consumes the sink and returns the wrapped writer.
    #[must_use]
    pub fn into_inner(self) -> W {
        self.writer
    }

    /// Consumes the sink and returns the writer, scratch buffer, and line mode.
    ///
    /// The returned [`MessageScratch`] can be reused to build another
    /// [`MessageSink`] via [`with_parts`](Self::with_parts), avoiding repeated
    /// zeroing of scratch storage when logging contexts are recycled.
    #[must_use]
    pub fn into_parts(self) -> (W, MessageScratch, LineMode) {
        (self.writer, self.scratch, self.line_mode)
    }
}

impl<W> Default for MessageSink<W>
where
    W: Default,
{
    fn default() -> Self {
        Self::new(W::default())
    }
}

impl<W> MessageSink<W>
where
    W: Write,
{
    /// Writes a single message to the underlying writer.
    pub fn write(&mut self, message: &Message) -> io::Result<()> {
        if self.line_mode.append_newline() {
            message.render_line_to_writer_with_scratch(&mut self.scratch, &mut self.writer)
        } else {
            message.render_to_writer_with_scratch(&mut self.scratch, &mut self.writer)
        }
    }

    /// Writes each message from the iterator to the underlying writer.
    ///
    /// The iterator may yield borrowed or owned [`Message`] values. Items that
    /// implement [`Borrow<Message>`] are accepted to avoid forcing callers to
    /// materialise intermediate references when they already own the messages.
    /// This keeps the method ergonomic for code that batches diagnostics in
    /// collections such as [`Vec<Message>`] or arrays.
    ///
    /// # Examples
    ///
    /// Write a slice of borrowed messages:
    ///
    /// ```
    /// use rsync_core::message::Message;
    /// use rsync_logging::MessageSink;
    ///
    /// let mut sink = MessageSink::new(Vec::new());
    /// let messages = [
    ///     Message::info("phase one"),
    ///     Message::warning("phase two"),
    ///     Message::error(23, "partial transfer"),
    /// ];
    ///
    /// sink.write_all(messages.iter())?;
    /// let buffer = String::from_utf8(sink.into_inner()).unwrap();
    /// assert_eq!(buffer.lines().count(), messages.len());
    /// # Ok::<(), std::io::Error>(())
    /// ```
    ///
    /// Consume owned messages without taking manual references:
    ///
    /// ```
    /// use rsync_core::message::Message;
    /// use rsync_logging::MessageSink;
    ///
    /// let mut sink = MessageSink::new(Vec::new());
    /// let messages = vec![
    ///     Message::info("phase one"),
    ///     Message::warning("phase two"),
    ///     Message::error(23, "partial transfer"),
    /// ];
    ///
    /// let count = messages.len();
    /// sink.write_all(messages)?;
    /// let buffer = String::from_utf8(sink.into_inner()).unwrap();
    /// assert_eq!(buffer.lines().count(), count);
    /// # Ok::<(), std::io::Error>(())
    /// ```
    pub fn write_all<I, M>(&mut self, messages: I) -> io::Result<()>
    where
        I: IntoIterator<Item = M>,
        M: Borrow<Message>,
    {
        for message in messages {
            self.write(message.borrow())?;
        }
        Ok(())
    }

    /// Flushes the underlying writer.
    pub fn flush(&mut self) -> io::Result<()> {
        self.writer.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsync_core::message::Message;

    #[test]
    fn sink_appends_newlines_by_default() {
        let mut sink = MessageSink::new(Vec::new());
        sink.write(&Message::warning("vanished"))
            .expect("write succeeds");
        sink.write(&Message::error(23, "partial"))
            .expect("write succeeds");

        let output = String::from_utf8(sink.into_inner()).expect("utf-8");
        let mut lines = output.lines();
        assert_eq!(lines.next(), Some("rsync warning: vanished"));
        assert_eq!(lines.next(), Some("rsync error: partial (code 23)"));
        assert!(lines.next().is_none());
    }

    #[test]
    fn sink_without_newline_preserves_output() {
        let mut sink = MessageSink::with_line_mode(Vec::new(), LineMode::WithoutNewline);
        sink.write(&Message::info("ready")).expect("write succeeds");

        let output = sink.into_inner();
        assert_eq!(output, b"rsync info: ready".to_vec());
    }

    #[test]
    fn write_all_streams_every_message() {
        let mut sink = MessageSink::with_line_mode(Vec::new(), LineMode::WithNewline);
        let messages = [
            Message::info("phase 1"),
            Message::warning("transient"),
            Message::error(10, "socket"),
        ];
        let expected = messages.len();
        sink.write_all(messages).expect("batch write succeeds");

        let output = String::from_utf8(sink.into_inner()).expect("utf-8");
        assert_eq!(output.lines().count(), expected);
    }

    #[test]
    fn write_all_accepts_owned_messages() {
        let mut sink = MessageSink::with_line_mode(Vec::new(), LineMode::WithNewline);
        let messages = vec![
            Message::info("phase 1"),
            Message::warning("transient"),
            Message::error(10, "socket"),
        ];
        let expected = messages.len();

        sink.write_all(messages).expect("batch write succeeds");

        let output = String::from_utf8(sink.into_inner()).expect("utf-8");
        assert_eq!(output.lines().count(), expected);
    }

    #[test]
    fn into_parts_allows_reusing_scratch() {
        let mut sink =
            MessageSink::with_parts(Vec::new(), MessageScratch::new(), LineMode::WithoutNewline);
        sink.write(&Message::info("first")).expect("write succeeds");

        let (writer, scratch, mode) = sink.into_parts();
        assert_eq!(mode, LineMode::WithoutNewline);

        let mut sink = MessageSink::with_parts(writer, scratch, LineMode::WithNewline);
        sink.write(&Message::warning("second"))
            .expect("write succeeds");

        let output = String::from_utf8(sink.into_inner()).expect("utf-8");
        assert!(output.starts_with("rsync info: first"));
        assert!(output.contains("rsync warning: second"));
        assert!(output.ends_with('\n'));
    }
}
