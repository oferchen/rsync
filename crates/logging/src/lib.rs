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
use std::fmt;
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
    /// Reports whether the mode appends a trailing newline when rendering a message.
    ///
    /// The helper mirrors the terminology used throughout the workspace where
    /// [`LineMode::WithNewline`] matches upstream rsync's default of emitting
    /// each diagnostic on its own line. Exposing the behaviour as a method
    /// avoids requiring callers to pattern-match on the enum, simplifying
    /// integrations that need to mirror the sink's newline policy when routing
    /// messages to multiple destinations.
    ///
    /// # Examples
    ///
    /// ```
    /// use rsync_logging::LineMode;
    ///
    /// assert!(LineMode::WithNewline.append_newline());
    /// assert!(!LineMode::WithoutNewline.append_newline());
    /// ```
    #[must_use]
    pub const fn append_newline(self) -> bool {
        matches!(self, Self::WithNewline)
    }
}

impl Default for LineMode {
    fn default() -> Self {
        Self::WithNewline
    }
}

impl From<bool> for LineMode {
    /// Converts a boolean flag describing whether a trailing newline should be appended into a [`LineMode`].
    ///
    /// `true` maps to [`LineMode::WithNewline`] while `false` selects [`LineMode::WithoutNewline`],
    /// mirroring the terminology used throughout the workspace. This allows call sites that already
    /// compute newline behaviour as a boolean (for example, when matching upstream format tables) to
    /// adopt [`MessageSink`] without branching on the enum variants themselves.
    ///
    /// # Examples
    ///
    /// ```
    /// use rsync_logging::LineMode;
    ///
    /// assert_eq!(LineMode::from(true), LineMode::WithNewline);
    /// assert_eq!(LineMode::from(false), LineMode::WithoutNewline);
    /// ```
    fn from(append_newline: bool) -> Self {
        if append_newline {
            Self::WithNewline
        } else {
            Self::WithoutNewline
        }
    }
}

impl From<LineMode> for bool {
    /// Converts a [`LineMode`] back into a boolean flag describing whether a trailing newline is appended.
    ///
    /// The conversion delegates to [`LineMode::append_newline`], ensuring the mapping remains consistent even
    /// if future variants are introduced. This is primarily useful in formatting pipelines that need to feed
    /// newline preferences into APIs expecting a boolean without reimplementing the enum-to-bool logic.
    ///
    /// # Examples
    ///
    /// ```
    /// use rsync_logging::LineMode;
    ///
    /// let append_newline: bool = LineMode::WithNewline.into();
    /// assert!(append_newline);
    ///
    /// let append_newline: bool = LineMode::WithoutNewline.into();
    /// assert!(!append_newline);
    /// ```
    fn from(mode: LineMode) -> Self {
        mode.append_newline()
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

/// Error returned by [`MessageSink::try_map_writer`] when the conversion closure fails.
///
/// The structure preserves ownership of the original [`MessageSink`] together with the
/// error reported by the conversion attempt. This mirrors `std::io::IntoInnerError`
/// so callers can recover the sink and either retry with a different mapping or continue
/// using the existing writer. Helper accessors expose both components without forcing
/// additional allocations, and the wrapper implements rich ergonomics such as [`Clone`],
/// [`as_ref`](Self::as_ref), and [`map_parts`](Self::map_parts) so preserved state can be
/// inspected or transformed without dropping buffered diagnostics.
pub struct TryMapWriterError<W, E> {
    sink: MessageSink<W>,
    error: E,
}

impl<W, E> Clone for TryMapWriterError<W, E>
where
    MessageSink<W>: Clone,
    E: Clone,
{
    fn clone(&self) -> Self {
        Self {
            sink: self.sink.clone(),
            error: self.error.clone(),
        }
    }
}

impl<W, E> TryMapWriterError<W, E> {
    const fn new(sink: MessageSink<W>, error: E) -> Self {
        Self { sink, error }
    }

    /// Returns a reference to the preserved [`MessageSink`].
    #[must_use]
    pub fn sink(&self) -> &MessageSink<W> {
        &self.sink
    }

    /// Returns a mutable reference to the preserved [`MessageSink`].
    #[must_use]
    pub fn sink_mut(&mut self) -> &mut MessageSink<W> {
        &mut self.sink
    }

    /// Returns a reference to the conversion error.
    #[must_use]
    pub fn error(&self) -> &E {
        &self.error
    }

    /// Returns a mutable reference to the conversion error.
    #[must_use]
    pub fn error_mut(&mut self) -> &mut E {
        &mut self.error
    }

    /// Returns shared references to the preserved sink and error.
    #[must_use]
    pub fn as_ref(&self) -> (&MessageSink<W>, &E) {
        (&self.sink, &self.error)
    }

    /// Returns mutable references to the preserved sink and error.
    #[must_use]
    pub fn as_mut(&mut self) -> (&mut MessageSink<W>, &mut E) {
        (&mut self.sink, &mut self.error)
    }

    /// Consumes the wrapper and returns the preserved sink and conversion error.
    #[must_use]
    pub fn into_parts(self) -> (MessageSink<W>, E) {
        (self.sink, self.error)
    }
}

impl<W, E> TryMapWriterError<W, E> {
    /// Consumes the wrapper and returns only the preserved [`MessageSink`].
    #[must_use]
    pub fn into_sink(self) -> MessageSink<W> {
        self.sink
    }

    /// Consumes the wrapper and returns only the conversion error.
    #[must_use]
    pub fn into_error(self) -> E {
        self.error
    }

    /// Maps the preserved sink into another type while retaining the error.
    #[must_use]
    pub fn map_sink<W2, F>(self, map: F) -> TryMapWriterError<W2, E>
    where
        F: FnOnce(MessageSink<W>) -> MessageSink<W2>,
    {
        let (sink, error) = self.into_parts();
        TryMapWriterError::new(map(sink), error)
    }

    /// Maps the preserved error into another type while retaining the sink.
    #[must_use]
    pub fn map_error<E2, F>(self, map: F) -> TryMapWriterError<W, E2>
    where
        F: FnOnce(E) -> E2,
    {
        let (sink, error) = self.into_parts();
        TryMapWriterError::new(sink, map(error))
    }

    /// Transforms both the preserved sink and error in a single pass.
    #[must_use]
    pub fn map_parts<W2, E2, F>(self, map: F) -> TryMapWriterError<W2, E2>
    where
        F: FnOnce(MessageSink<W>, E) -> (MessageSink<W2>, E2),
    {
        let (sink, error) = self.into_parts();
        let (sink, error) = map(sink, error);
        TryMapWriterError::new(sink, error)
    }
}

impl<W, E> From<(MessageSink<W>, E)> for TryMapWriterError<W, E> {
    fn from((sink, error): (MessageSink<W>, E)) -> Self {
        TryMapWriterError::new(sink, error)
    }
}

impl<W, E> fmt::Debug for TryMapWriterError<W, E>
where
    MessageSink<W>: fmt::Debug,
    E: fmt::Debug,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TryMapWriterError")
            .field("sink", &self.sink)
            .field("error", &self.error)
            .finish()
    }
}

impl<W, E> fmt::Display for TryMapWriterError<W, E>
where
    E: fmt::Display,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "failed to map message sink writer: {}", self.error)
    }
}

impl<W, E> std::error::Error for TryMapWriterError<W, E>
where
    E: std::error::Error + fmt::Debug + 'static,
    MessageSink<W>: fmt::Debug,
{
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.error)
    }
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

    /// Maps the sink's writer into a different type while preserving the existing
    /// scratch buffer and [`LineMode`].
    ///
    /// The helper consumes the sink, applies the provided conversion to the
    /// underlying writer, and returns a new sink that reuses the previous
    /// [`MessageScratch`]. This mirrors patterns such as `BufWriter::into_inner`
    /// where callers often want to hand ownership of the buffered writer to a
    /// higher layer without reinitialising per-sink state.
    ///
    /// # Examples
    ///
    /// ```
    /// # use rsync_core::message::Message;
    /// # use rsync_logging::{LineMode, MessageSink};
    /// # use std::io::Cursor;
    /// let sink = MessageSink::with_line_mode(Vec::new(), LineMode::WithoutNewline);
    /// let mut sink = sink.map_writer(Cursor::new);
    /// sink.write(&Message::info("ready"))?;
    /// let cursor = sink.into_inner();
    /// assert_eq!(cursor.into_inner(), b"rsync info: ready".to_vec());
    /// # Ok::<(), std::io::Error>(())
    /// ```
    #[must_use]
    pub fn map_writer<F, W2>(self, f: F) -> MessageSink<W2>
    where
        F: FnOnce(W) -> W2,
    {
        let MessageSink {
            writer,
            scratch,
            line_mode,
        } = self;
        MessageSink::with_parts(f(writer), scratch, line_mode)
    }

    /// Attempts to map the sink's writer into a different type, preserving the original sink on
    /// failure.
    ///
    /// The closure returns `Ok` with the mapped writer when the conversion succeeds. On error, it
    /// must return the original writer alongside the error value so the method can reconstruct the
    /// [`MessageSink`]. This mirrors [`std::io::IntoInnerError`], allowing callers to recover
    /// without losing buffered diagnostics.
    ///
    /// # Examples
    ///
    /// Convert the writer into a `Cursor<Vec<u8>>` while keeping the scratch buffer and line mode:
    ///
    /// ```
    /// # use rsync_core::message::Message;
    /// # use rsync_logging::MessageSink;
    /// # use std::io::Cursor;
    /// let sink = MessageSink::new(Vec::new());
    /// let mut sink = sink
    ///     .try_map_writer(|writer| -> Result<Cursor<Vec<u8>>, (Vec<u8>, &'static str)> {
    ///         Ok(Cursor::new(writer))
    ///     })
    ///     .expect("mapping succeeds");
    /// sink.write(&Message::info("ready"))?;
    /// assert_eq!(sink.into_inner().into_inner(), b"rsync info: ready\n".to_vec());
    /// # Ok::<(), std::io::Error>(())
    /// ```
    ///
    /// Recover the original sink when the conversion fails:
    ///
    /// ```
    /// # use rsync_core::message::Message;
    /// # use rsync_logging::{LineMode, MessageSink};
    /// let sink = MessageSink::with_line_mode(Vec::new(), LineMode::WithoutNewline);
    /// let err = sink
    ///     .try_map_writer(|writer| -> Result<Vec<u8>, (Vec<u8>, &'static str)> {
    ///         Err((writer, "permission denied"))
    ///     })
    ///     .unwrap_err();
    /// let (mut sink, error) = err.into_parts();
    /// assert_eq!(error, "permission denied");
    /// sink.write(&Message::info("still working"))?;
    /// assert_eq!(sink.into_inner(), b"rsync info: still working".to_vec());
    /// # Ok::<(), std::io::Error>(())
    /// ```
    pub fn try_map_writer<F, W2, E>(self, f: F) -> Result<MessageSink<W2>, TryMapWriterError<W, E>>
    where
        F: FnOnce(W) -> Result<W2, (W, E)>,
    {
        let MessageSink {
            writer,
            scratch,
            line_mode,
        } = self;

        match f(writer) {
            Ok(mapped) => Ok(MessageSink::with_parts(mapped, scratch, line_mode)),
            Err((writer, error)) => Err(TryMapWriterError::new(
                MessageSink::with_parts(writer, scratch, line_mode),
                error,
            )),
        }
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
    fn render_message(&mut self, message: &Message, append_newline: bool) -> io::Result<()> {
        if append_newline {
            message.render_line_to_writer_with_scratch(&mut self.scratch, &mut self.writer)
        } else {
            message.render_to_writer_with_scratch(&mut self.scratch, &mut self.writer)
        }
    }

    /// Writes a single message using the sink's current [`LineMode`].
    pub fn write(&mut self, message: &Message) -> io::Result<()> {
        self.render_message(message, self.line_mode.append_newline())
    }

    /// Writes `message` using an explicit [`LineMode`] without mutating the sink.
    ///
    /// The helper mirrors [`write`](Self::write) but allows callers to override the
    /// newline behaviour for a single message. This is useful when most
    /// diagnostics should follow the sink's configured mode yet specific
    /// messages must be emitted without a trailing newline (for example,
    /// progress indicators that are overwritten in-place).
    ///
    /// # Examples
    ///
    /// Render a final message without a newline while keeping the sink's
    /// default `LineMode::WithNewline` for subsequent writes:
    ///
    /// ```
    /// use rsync_core::message::Message;
    /// use rsync_logging::{LineMode, MessageSink};
    ///
    /// let mut sink = MessageSink::new(Vec::new());
    /// sink.write(&Message::info("phase one"))?;
    /// sink.write_with_mode(&Message::info("progress"), LineMode::WithoutNewline)?;
    /// sink.write(&Message::info("phase two"))?;
    ///
    /// let output = String::from_utf8(sink.into_inner()).unwrap();
    /// let mut lines = output.lines();
    /// assert_eq!(lines.next(), Some("rsync info: phase one"));
    /// assert_eq!(
    ///     lines.next(),
    ///     Some("rsync info: progressrsync info: phase two"),
    /// );
    /// // The progress message was rendered without a newline, so it shares the
    /// // line with the final status update.
    /// assert!(lines.next().is_none());
    /// # Ok::<(), std::io::Error>(())
    /// ```
    pub fn write_with_mode(&mut self, message: &Message, line_mode: LineMode) -> io::Result<()> {
        self.render_message(message, line_mode.append_newline())
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
        let append_newline = self.line_mode.append_newline();
        for message in messages {
            self.render_message(message.borrow(), append_newline)?;
        }
        Ok(())
    }

    /// Writes each message from the iterator using the provided [`LineMode`].
    ///
    /// This mirrors [`write_all`](Self::write_all) but allows callers to batch messages that
    /// require a specific newline mode without mutating the sink's configuration. The helper is
    /// useful when most diagnostics should follow the sink's [`LineMode::WithNewline`] default yet a
    /// subset (such as progress updates) must be rendered without trailing newlines.
    ///
    /// # Examples
    ///
    /// Render a batch of progress messages without altering the sink's line mode:
    ///
    /// ```
    /// use rsync_core::message::Message;
    /// use rsync_logging::{LineMode, MessageSink};
    ///
    /// let mut sink = MessageSink::new(Vec::new());
    /// let progress = [
    ///     Message::info("progress 1"),
    ///     Message::info("progress 2"),
    /// ];
    ///
    /// sink.write_all_with_mode(progress.iter(), LineMode::WithoutNewline)?;
    /// assert_eq!(sink.line_mode(), LineMode::WithNewline);
    /// let output = sink.into_inner();
    /// assert_eq!(output, b"rsync info: progress 1rsync info: progress 2".to_vec());
    /// # Ok::<(), std::io::Error>(())
    /// ```
    pub fn write_all_with_mode<I, M>(&mut self, messages: I, line_mode: LineMode) -> io::Result<()>
    where
        I: IntoIterator<Item = M>,
        M: Borrow<Message>,
    {
        let append_newline = line_mode.append_newline();
        for message in messages {
            self.render_message(message.borrow(), append_newline)?;
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
    use std::io::{self, Cursor, Write};

    #[derive(Default)]
    struct TrackingWriter {
        buffer: Vec<u8>,
        flush_calls: usize,
    }

    impl Write for TrackingWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.buffer.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            self.flush_calls += 1;
            Ok(())
        }
    }

    struct FailingFlushWriter;

    impl Write for FailingFlushWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Err(io::Error::other("flush failed"))
        }
    }

    #[test]
    fn line_mode_bool_conversions_round_trip() {
        assert_eq!(LineMode::from(true), LineMode::WithNewline);
        assert_eq!(LineMode::from(false), LineMode::WithoutNewline);

        let append: bool = LineMode::WithNewline.into();
        assert!(append);

        let append: bool = LineMode::WithoutNewline.into();
        assert!(!append);
    }

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
    fn map_writer_preserves_configuration() {
        use std::io::Cursor;

        let sink = MessageSink::with_line_mode(Vec::new(), LineMode::WithoutNewline);
        let mut sink = sink.map_writer(Cursor::new);
        assert_eq!(sink.line_mode(), LineMode::WithoutNewline);

        sink.write(&Message::info("ready")).expect("write succeeds");

        let cursor = sink.into_inner();
        assert_eq!(cursor.into_inner(), b"rsync info: ready".to_vec());
    }

    #[test]
    fn try_map_writer_transforms_writer() {
        use std::io::Cursor;

        let sink = MessageSink::with_line_mode(Vec::new(), LineMode::WithoutNewline);
        let mut sink = sink
            .try_map_writer(
                |writer| -> Result<Cursor<Vec<u8>>, (Vec<u8>, &'static str)> {
                    Ok(Cursor::new(writer))
                },
            )
            .expect("mapping succeeds");
        assert_eq!(sink.line_mode(), LineMode::WithoutNewline);

        sink.write(&Message::info("ready")).expect("write succeeds");

        let cursor = sink.into_inner();
        assert_eq!(cursor.into_inner(), b"rsync info: ready".to_vec());
    }

    #[test]
    fn try_map_writer_preserves_sink_on_error() {
        let sink = MessageSink::new(Vec::new());
        let err = sink
            .try_map_writer(|writer| -> Result<Vec<u8>, (Vec<u8>, &'static str)> {
                Err((writer, "conversion failed"))
            })
            .unwrap_err();
        let (mut sink, error) = err.into_parts();

        assert_eq!(error, "conversion failed");

        sink.write(&Message::info("still running"))
            .expect("write succeeds");

        let output = String::from_utf8(sink.into_inner()).expect("utf-8");
        assert_eq!(output, "rsync info: still running\n");
    }

    #[test]
    fn try_map_writer_error_clone_preserves_state() {
        let mut original =
            TryMapWriterError::new(MessageSink::new(Vec::<u8>::new()), String::from("failure"));
        let mut cloned = original.clone();

        original
            .sink_mut()
            .write(&Message::info("original"))
            .expect("write succeeds");
        cloned
            .sink_mut()
            .write(&Message::info("clone"))
            .expect("write succeeds");

        assert_eq!(original.error(), "failure");
        assert_eq!(cloned.error(), "failure");

        let (original_sink, original_error) = original.into_parts();
        let (cloned_sink, cloned_error) = cloned.into_parts();

        assert_eq!(original_error, "failure");
        assert_eq!(cloned_error, "failure");

        let original_rendered = String::from_utf8(original_sink.into_inner()).expect("utf-8");
        let cloned_rendered = String::from_utf8(cloned_sink.into_inner()).expect("utf-8");

        assert!(original_rendered.contains("original"));
        assert!(cloned_rendered.contains("clone"));
    }

    #[test]
    fn try_map_writer_error_as_ref_and_as_mut_provide_access() {
        let mut err =
            TryMapWriterError::new(MessageSink::new(Vec::<u8>::new()), String::from("failure"));
        let (sink_ref, error_ref) = err.as_ref();
        assert_eq!(sink_ref.line_mode(), LineMode::WithNewline);
        assert_eq!(error_ref, "failure");

        {
            let (sink_mut, error_mut) = err.as_mut();
            sink_mut.set_line_mode(LineMode::WithoutNewline);
            error_mut.push('!');
        }

        assert_eq!(err.sink().line_mode(), LineMode::WithoutNewline);
        assert_eq!(err.error(), "failure!");
    }

    #[test]
    fn try_map_writer_error_map_helpers_transform_components() {
        let err =
            TryMapWriterError::new(MessageSink::new(Vec::<u8>::new()), String::from("failure"));

        let mapped_sink = err.clone().map_sink(|mut sink| {
            sink.set_line_mode(LineMode::WithoutNewline);
            sink
        });
        assert_eq!(mapped_sink.sink().line_mode(), LineMode::WithoutNewline);
        assert_eq!(mapped_sink.error(), "failure");

        let mapped_error = err.clone().map_error(|error| error.len());
        assert_eq!(*mapped_error.error(), "failure".len());
        assert_eq!(mapped_error.sink().line_mode(), LineMode::WithNewline);

        let mut mapped_parts = err.map_parts(|sink, error| {
            let sink = sink.map_writer(Cursor::new);
            let len = error.len();
            (sink, len)
        });
        assert_eq!(*mapped_parts.error(), "failure".len());

        mapped_parts
            .sink_mut()
            .write(&Message::info("mapped"))
            .expect("write succeeds");

        let cursor = mapped_parts.into_sink().into_inner();
        let rendered = String::from_utf8(cursor.into_inner()).expect("utf-8");
        assert!(rendered.contains("mapped"));
    }

    #[test]
    fn write_with_mode_overrides_line_mode_for_single_message() {
        let mut sink = MessageSink::new(Vec::new());
        sink.write(&Message::info("phase one"))
            .expect("write succeeds");
        sink.write_with_mode(&Message::info("progress"), LineMode::WithoutNewline)
            .expect("write succeeds");
        sink.write(&Message::info("phase two"))
            .expect("write succeeds");

        assert_eq!(sink.line_mode(), LineMode::WithNewline);

        let output = sink.into_inner();
        let rendered = String::from_utf8(output).expect("utf-8");
        let mut lines = rendered.lines();
        assert_eq!(lines.next(), Some("rsync info: phase one"));
        assert_eq!(
            lines.next(),
            Some("rsync info: progressrsync info: phase two"),
        );
        assert!(lines.next().is_none());
    }

    #[test]
    fn write_with_mode_respects_explicit_newline() {
        let mut sink = MessageSink::with_line_mode(Vec::new(), LineMode::WithoutNewline);
        sink.write_with_mode(&Message::warning("vanished"), LineMode::WithNewline)
            .expect("write succeeds");

        assert_eq!(sink.line_mode(), LineMode::WithoutNewline);

        let buffer = sink.into_inner();
        let rendered = String::from_utf8(buffer).expect("utf-8");
        assert_eq!(rendered, "rsync warning: vanished\n");
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
    fn write_all_with_mode_uses_explicit_line_mode() {
        let mut sink = MessageSink::new(Vec::new());
        let progress = [Message::info("p1"), Message::info("p2")];

        sink.write_all_with_mode(progress.iter(), LineMode::WithoutNewline)
            .expect("batch write succeeds");

        assert_eq!(sink.line_mode(), LineMode::WithNewline);

        let output = sink.into_inner();
        assert_eq!(output, b"rsync info: p1rsync info: p2".to_vec());
    }

    #[test]
    fn write_all_with_mode_accepts_owned_messages() {
        let mut sink = MessageSink::with_line_mode(Vec::new(), LineMode::WithNewline);
        let messages = vec![Message::info("one"), Message::info("two")];

        sink.write_all_with_mode(messages, LineMode::WithoutNewline)
            .expect("batch write succeeds");

        assert_eq!(sink.line_mode(), LineMode::WithNewline);

        let output = sink.into_inner();
        assert_eq!(output, b"rsync info: onersync info: two".to_vec());
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

    #[test]
    fn set_line_mode_updates_behavior() {
        let mut sink = MessageSink::new(Vec::new());
        assert_eq!(sink.line_mode(), LineMode::WithNewline);

        sink.set_line_mode(LineMode::WithoutNewline);
        assert_eq!(sink.line_mode(), LineMode::WithoutNewline);

        sink.write(&Message::info("ready")).expect("write succeeds");

        let buffer = sink.into_inner();
        assert_eq!(buffer, b"rsync info: ready".to_vec());
    }

    #[test]
    fn flush_delegates_to_inner_writer() {
        let writer = TrackingWriter::default();
        let mut sink = MessageSink::with_line_mode(writer, LineMode::WithNewline);

        sink.flush().expect("flush succeeds");

        let writer = sink.into_inner();
        assert_eq!(writer.flush_calls, 1);
        assert!(writer.buffer.is_empty());
    }

    #[test]
    fn flush_propagates_writer_errors() {
        let mut sink = MessageSink::with_line_mode(FailingFlushWriter, LineMode::WithNewline);

        let err = sink.flush().expect_err("flush should propagate error");
        assert_eq!(err.kind(), io::ErrorKind::Other);
    }
}
