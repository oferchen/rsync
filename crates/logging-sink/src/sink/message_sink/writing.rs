use super::MessageSink;
use crate::line_mode::LineMode;
use core::message::{Message, MessageSegments};
use std::borrow::Borrow;
use std::io::{self, Write};

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
    /// [`std::io::Write::write_all`], making it inexpensive to reuse the same
    /// sink for ad-hoc or batched message emission.
    pub fn write<M>(&mut self, message: M) -> io::Result<()>
    where
        M: Borrow<Message>,
    {
        let branded = message.borrow().clone().with_brand(self.brand);
        self.render_message(&branded, self.line_mode.append_newline())
    }

    /// Writes `message` using an explicit [`LineMode`] without mutating the sink.
    ///
    /// The helper mirrors [`write`](Self::write) but allows callers to override
    /// the newline behaviour for a single message. This is useful when most
    /// diagnostics should follow the sink's configured mode yet specific
    /// messages must be emitted without a trailing newline (for example,
    /// progress indicators that are overwritten in-place).
    pub fn write_with_mode<M>(&mut self, message: M, line_mode: LineMode) -> io::Result<()>
    where
        M: Borrow<Message>,
    {
        let branded = message.borrow().clone().with_brand(self.brand);
        self.render_message(&branded, line_mode.append_newline())
    }

    /// Streams pre-rendered [`MessageSegments`] into the underlying writer.
    ///
    /// The helper allows callers that already rendered a [`Message`] into
    /// vectored slices (for example, to inspect or buffer them) to forward the
    /// segments without requesting another render. The sink honours its
    /// configured [`LineMode`] when deciding whether to append a trailing
    /// newline; callers must indicate whether `segments` already include a
    /// newline slice via the `segments_include_newline` flag. Passing `false`
    /// matches the common case of invoking [`Message::as_segments`] with
    /// `include_newline` set to `false`.
    pub fn write_segments(
        &mut self,
        segments: &MessageSegments<'_>,
        segments_include_newline: bool,
    ) -> io::Result<()> {
        self.write_segments_with_mode(segments, self.line_mode, segments_include_newline)
    }

    /// Writes pre-rendered [`MessageSegments`] using an explicit [`LineMode`].
    ///
    /// This mirrors [`write_segments`](Self::write_segments) but allows callers
    /// to override the newline behaviour for a single emission. The
    /// `segments_include_newline` flag indicates whether the supplied segments
    /// already contain a terminating newline (for example when rendered via
    /// [`Message::as_segments`] with `include_newline = true`). When the flag is
    /// `false` and the selected [`LineMode`] appends newlines, the sink writes
    /// the trailing newline after streaming the segments.
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
    pub fn write_all<I, M>(&mut self, messages: I) -> io::Result<()>
    where
        I: IntoIterator<Item = M>,
        M: Borrow<Message>,
    {
        let append_newline = self.line_mode.append_newline();
        for message in messages {
            let branded = message.borrow().clone().with_brand(self.brand);
            self.render_message(&branded, append_newline)?;
        }
        Ok(())
    }

    /// Writes each message from the iterator using the provided [`LineMode`].
    ///
    /// This mirrors [`write_all`](Self::write_all) but allows callers to batch
    /// messages that require a specific newline mode without mutating the sink's
    /// configuration. The helper is useful when most diagnostics should follow
    /// the sink's [`LineMode::WithNewline`] default yet a subset (such as
    /// progress updates) must be rendered without trailing newlines.
    pub fn write_all_with_mode<I, M>(&mut self, messages: I, line_mode: LineMode) -> io::Result<()>
    where
        I: IntoIterator<Item = M>,
        M: Borrow<Message>,
    {
        let append_newline = line_mode.append_newline();
        for message in messages {
            let branded = message.borrow().clone().with_brand(self.brand);
            self.render_message(&branded, append_newline)?;
        }
        Ok(())
    }

    /// Flushes the underlying writer.
    pub fn flush(&mut self) -> io::Result<()> {
        self.writer.flush()
    }
}
