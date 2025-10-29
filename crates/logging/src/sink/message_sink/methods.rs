use std::borrow::Borrow;
use std::io::{self, Write};
use std::mem;

use super::MessageSink;
use crate::line_mode::LineMode;
use crate::{LineModeGuard, TryMapWriterError};
use rsync_core::message::{Message, MessageScratch, MessageSegments};

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

    /// Returns a shared reference to the underlying writer.
    ///
    /// The reference allows callers to inspect buffered diagnostics without
    /// consuming the sink. This mirrors APIs such as
    /// [`std::io::BufWriter::get_ref`], making it convenient to peek at
    /// in-memory buffers (for example, when testing message renderers) while
    /// continuing to reuse the same [`MessageSink`].
    ///
    /// # Examples
    ///
    /// ```
    /// use rsync_logging::MessageSink;
    ///
    /// let sink = MessageSink::new(Vec::<u8>::new());
    /// assert!(sink.writer().is_empty());
    /// ```
    #[must_use]
    pub fn writer(&self) -> &W {
        &self.writer
    }

    /// Returns a mutable reference to the underlying writer.
    ///
    /// This is useful when integrations need to adjust writer state before
    /// emitting additional diagnostics. The sink keeps ownership of the writer,
    /// so logging can continue after the mutation.
    ///
    /// # Examples
    ///
    /// ```
    /// use rsync_logging::MessageSink;
    ///
    /// let mut sink = MessageSink::new(Vec::<u8>::new());
    /// sink.writer_mut().extend_from_slice(b"prefill");
    /// assert_eq!(sink.writer().as_slice(), b"prefill");
    /// ```
    pub fn writer_mut(&mut self) -> &mut W {
        &mut self.writer
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

    /// Temporarily overrides the sink's [`LineMode`], restoring the previous value on drop.
    ///
    /// The returned guard implements [`Deref`](std::ops::Deref) and [`DerefMut`](std::ops::DerefMut),
    /// allowing callers to treat it as a mutable reference to the sink. This mirrors upstream rsync's
    /// behaviour of disabling trailing newlines for progress updates while ensuring the original
    /// configuration is reinstated once the guard is dropped. The guard carries a `#[must_use]`
    /// attribute so ignoring the return value triggers a lint, preventing accidental one-line
    /// overrides that would immediately revert to the previous mode.
    ///
    /// # Examples
    ///
    /// ```
    /// use rsync_core::message::Message;
    /// use rsync_logging::{LineMode, MessageSink};
    ///
    /// let mut sink = MessageSink::new(Vec::new());
    /// {
    ///     let mut guard = sink.scoped_line_mode(LineMode::WithoutNewline);
    ///     guard.write(Message::info("phase one")).unwrap();
    ///     guard.write(Message::info("phase two")).unwrap();
    /// }
    /// sink.write(Message::info("done")).unwrap();
    /// let output = String::from_utf8(sink.into_inner()).unwrap();
    /// assert!(output.starts_with("rsync info: phase one"));
    /// assert!(output.ends_with("done\n"));
    /// ```
    #[must_use = "bind the guard to retain the temporary line mode override for its scope"]
    pub fn scoped_line_mode(&mut self, line_mode: LineMode) -> LineModeGuard<'_, W> {
        let previous = self.line_mode;
        self.line_mode = line_mode;
        LineModeGuard::new(self, previous)
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

    /// Returns a shared reference to the reusable [`MessageScratch`] buffer.
    ///
    /// This enables integrations that need to inspect or duplicate the scratch
    /// storage (for example, when constructing additional sinks that should
    /// share the same initial digits) without consuming the sink. The returned
    /// reference is valid for the lifetime of `self` and matches the buffer used
    /// internally by [`write`](Self::write) and related helpers.
    ///
    /// # Examples
    ///
    /// ```
    /// use rsync_core::message::MessageScratch;
    /// use rsync_logging::MessageSink;
    ///
    /// let sink = MessageSink::new(Vec::<u8>::new());
    /// let scratch: *const MessageScratch = sink.scratch();
    /// assert!(!scratch.is_null());
    /// ```
    #[must_use]
    pub const fn scratch(&self) -> &MessageScratch {
        &self.scratch
    }

    /// Returns a mutable reference to the sink's [`MessageScratch`] buffer.
    ///
    /// Callers can reset or prepopulate the scratch storage before emitting
    /// diagnostics. Because the buffer is reused across writes, manually
    /// initialising it can help enforce deterministic state when toggling
    /// between sinks that share a scratch instance.
    ///
    /// # Examples
    ///
    /// ```
    /// use rsync_core::message::{Message, MessageScratch};
    /// use rsync_logging::MessageSink;
    ///
    /// let mut sink = MessageSink::new(Vec::<u8>::new());
    /// *sink.scratch_mut() = MessageScratch::new();
    /// sink.write(Message::info("ready"))?;
    /// # Ok::<(), std::io::Error>(())
    /// ```
    pub fn scratch_mut(&mut self) -> &mut MessageScratch {
        &mut self.scratch
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
    /// sink.write(Message::info("ready"))?;
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
    /// let sink = MessageSink::new(Vec::<u8>::new());
    /// let mut sink = sink
    ///     .try_map_writer(|writer| -> Result<Cursor<Vec<u8>>, (Vec<u8>, &'static str)> {
    ///         Ok(Cursor::new(writer))
    ///     })
    ///     .expect("mapping succeeds");
    /// sink.write(Message::info("ready"))?;
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
    /// sink.write(Message::info("still working"))?;
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

    /// Replaces the underlying writer while preserving the sink's scratch buffer and [`LineMode`].
    ///
    /// The previous writer is returned to the caller so buffered diagnostics can be inspected or
    /// flushed before it is dropped. This avoids rebuilding the entire [`MessageSink`] when the
    /// destination changes—for example, when switching from standard output to a log file mid-run.
    /// The method performs an in-place swap, keeping the existing [`MessageScratch`] zeroed and
    /// reusing it for subsequent writes.
    ///
    /// # Examples
    ///
    /// ```
    /// use rsync_core::message::Message;
    /// use rsync_logging::MessageSink;
    ///
    /// let mut sink = MessageSink::new(Vec::<u8>::new());
    /// sink.write(Message::info("phase one"))?;
    /// let previous = sink.replace_writer(Vec::new());
    /// assert_eq!(String::from_utf8(previous).unwrap(), "rsync info: phase one\n");
    ///
    /// sink.write(Message::info("phase two"))?;
    /// assert_eq!(
    ///     String::from_utf8(sink.into_inner()).unwrap(),
    ///     "rsync info: phase two\n"
    /// );
    /// # Ok::<(), std::io::Error>(())
    /// ```
    #[must_use = "the returned writer contains diagnostics produced before the replacement"]
    pub fn replace_writer(&mut self, mut writer: W) -> W {
        mem::swap(&mut self.writer, &mut writer);
        writer
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
    ///
    /// The method accepts borrowed or owned [`Message`] values via
    /// [`Borrow<Message>`], allowing call sites to forward diagnostics without
    /// cloning. This matches the flexibility offered by
    /// [`std::io::Write::write_all`], making it
    /// inexpensive to reuse the same sink for ad-hoc or batched message
    /// emission.
    ///
    /// # Examples
    ///
    /// ```
    /// use rsync_core::message::Message;
    /// use rsync_logging::MessageSink;
    ///
    /// let mut sink = MessageSink::new(Vec::new());
    /// sink.write(Message::info("borrowed"))?;
    /// sink.write(Message::warning("owned"))?;
    ///
    /// let rendered = String::from_utf8(sink.into_inner()).unwrap();
    /// assert_eq!(rendered.lines().count(), 2);
    /// # Ok::<(), std::io::Error>(())
    /// ```
    pub fn write<M>(&mut self, message: M) -> io::Result<()>
    where
        M: Borrow<Message>,
    {
        self.render_message(message.borrow(), self.line_mode.append_newline())
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
    /// sink.write(Message::info("phase one"))?;
    /// sink.write_with_mode(Message::info("progress"), LineMode::WithoutNewline)?;
    /// sink.write(Message::info("phase two"))?;
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
    pub fn write_with_mode<M>(&mut self, message: M, line_mode: LineMode) -> io::Result<()>
    where
        M: Borrow<Message>,
    {
        self.render_message(message.borrow(), line_mode.append_newline())
    }

    /// Streams pre-rendered [`MessageSegments`] into the underlying writer.
    ///
    /// The helper allows callers that already rendered a [`Message`] into vectored
    /// slices (for example, to inspect or buffer them) to forward the segments
    /// without requesting another render. The sink honours its configured
    /// [`LineMode`] when deciding whether to append a trailing newline; callers
    /// must indicate whether `segments` already include a newline slice via the
    /// `segments_include_newline` flag. Passing `false` matches the common case of
    /// invoking [`Message::as_segments`] with `include_newline` set to `false`.
    ///
    /// # Examples
    ///
    /// Forward vectored message segments and let the sink append the newline:
    ///
    /// ```
    /// use rsync_core::message::{Message, MessageScratch};
    /// use rsync_logging::MessageSink;
    ///
    /// let message = Message::info("phase complete");
    /// let mut scratch = MessageScratch::new();
    /// let segments = message.as_segments(&mut scratch, false);
    /// let mut sink = MessageSink::new(Vec::new());
    ///
    /// sink.write_segments(&segments, false)?;
    ///
    /// assert_eq!(
    ///     String::from_utf8(sink.into_inner()).unwrap(),
    ///     "rsync info: phase complete\n"
    /// );
    /// # Ok::<(), std::io::Error>(())
    /// ```
    pub fn write_segments(
        &mut self,
        segments: &MessageSegments<'_>,
        segments_include_newline: bool,
    ) -> io::Result<()> {
        self.write_segments_with_mode(segments, self.line_mode, segments_include_newline)
    }

    /// Writes pre-rendered [`MessageSegments`] using an explicit [`LineMode`].
    ///
    /// This mirrors [`write_segments`](Self::write_segments) but allows callers to
    /// override the newline behaviour for a single emission. The
    /// `segments_include_newline` flag indicates whether the supplied segments
    /// already contain a terminating newline (for example when rendered via
    /// [`Message::as_segments`] with `include_newline = true`). When the flag is
    /// `false` and the selected [`LineMode`] appends newlines, the sink writes the
    /// trailing newline after streaming the segments.
    pub fn write_segments_with_mode(
        &mut self,
        segments: &MessageSegments<'_>,
        line_mode: LineMode,
        segments_include_newline: bool,
    ) -> io::Result<()> {
        segments.write_to(&mut self.writer)?;

        if line_mode.append_newline() && !segments_include_newline {
            self.writer.write_all(b"\n")?;
        }

        Ok(())
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
