use super::MessageSink;
use crate::line_mode::LineMode;
use rsync_core::message::MessageScratch;

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
