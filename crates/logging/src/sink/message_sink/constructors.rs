use super::MessageSink;
use crate::line_mode::LineMode;
use core::{branding::Brand, message::MessageScratch};

impl<W> MessageSink<W> {
    /// Creates a new sink that appends a newline after each rendered message.
    #[must_use]
    pub fn new(writer: W) -> Self {
        Self::with_line_mode_and_brand(writer, LineMode::WithNewline, Brand::Upstream)
    }

    /// Creates a sink that appends a newline after each message while using `brand` when rendering.
    #[must_use]
    pub fn with_brand(writer: W, brand: Brand) -> Self {
        Self::with_line_mode_and_brand(writer, LineMode::WithNewline, brand)
    }

    /// Creates a sink with the provided [`LineMode`].
    #[must_use]
    pub fn with_line_mode(writer: W, line_mode: LineMode) -> Self {
        Self::with_line_mode_and_brand(writer, line_mode, Brand::Upstream)
    }

    /// Creates a sink with the provided [`LineMode`] and [`Brand`].
    #[must_use]
    pub fn with_line_mode_and_brand(writer: W, line_mode: LineMode, brand: Brand) -> Self {
        Self::with_parts_and_brand(writer, MessageScratch::new(), line_mode, brand)
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
        Self::with_parts_and_brand(writer, scratch, line_mode, Brand::Upstream)
    }

    /// Creates a sink from explicit [`MessageScratch`], [`LineMode`], and [`Brand`].
    #[must_use]
    pub fn with_parts_and_brand(
        writer: W,
        scratch: MessageScratch,
        line_mode: LineMode,
        brand: Brand,
    ) -> Self {
        Self {
            writer,
            scratch,
            line_mode,
            brand,
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
    pub fn into_parts(self) -> (W, MessageScratch, LineMode, Brand) {
        (self.writer, self.scratch, self.line_mode, self.brand)
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
