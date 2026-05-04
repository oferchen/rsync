use super::MessageSink;
use crate::line_mode::LineMode;
use core::{branding::Brand, message::MessageScratch};

impl<W> MessageSink<W> {
    /// Creates a new sink that appends a newline after each rendered message.
    #[must_use]
    pub const fn new(writer: W) -> Self {
        Self::with_line_mode_and_brand(writer, LineMode::WithNewline, Brand::Upstream)
    }

    /// Creates a sink that appends a newline after each message while using `brand` when rendering.
    #[must_use]
    pub const fn with_brand(writer: W, brand: Brand) -> Self {
        Self::with_line_mode_and_brand(writer, LineMode::WithNewline, brand)
    }

    /// Creates a sink with the provided [`LineMode`].
    #[must_use]
    pub const fn with_line_mode(writer: W, line_mode: LineMode) -> Self {
        Self::with_line_mode_and_brand(writer, line_mode, Brand::Upstream)
    }

    /// Creates a sink with the provided [`LineMode`] and [`Brand`].
    #[must_use]
    pub const fn with_line_mode_and_brand(writer: W, line_mode: LineMode, brand: Brand) -> Self {
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
    pub const fn with_parts(writer: W, scratch: MessageScratch, line_mode: LineMode) -> Self {
        Self::with_parts_and_brand(writer, scratch, line_mode, Brand::Upstream)
    }

    /// Creates a sink from explicit [`MessageScratch`], [`LineMode`], and [`Brand`].
    #[must_use]
    pub const fn with_parts_and_brand(
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_creates_sink_with_newline_mode() {
        let sink: MessageSink<Vec<u8>> = MessageSink::new(Vec::new());
        assert_eq!(sink.line_mode(), LineMode::WithNewline);
    }

    #[test]
    fn new_creates_sink_with_upstream_brand() {
        let sink: MessageSink<Vec<u8>> = MessageSink::new(Vec::new());
        assert_eq!(sink.brand(), Brand::Upstream);
    }

    #[test]
    fn with_brand_sets_brand() {
        let sink: MessageSink<Vec<u8>> = MessageSink::with_brand(Vec::new(), Brand::Oc);
        assert_eq!(sink.brand(), Brand::Oc);
    }

    #[test]
    fn with_brand_uses_newline_mode() {
        let sink: MessageSink<Vec<u8>> = MessageSink::with_brand(Vec::new(), Brand::Oc);
        assert_eq!(sink.line_mode(), LineMode::WithNewline);
    }

    #[test]
    fn with_line_mode_sets_line_mode() {
        let sink: MessageSink<Vec<u8>> =
            MessageSink::with_line_mode(Vec::new(), LineMode::WithoutNewline);
        assert_eq!(sink.line_mode(), LineMode::WithoutNewline);
    }

    #[test]
    fn with_line_mode_uses_upstream_brand() {
        let sink: MessageSink<Vec<u8>> =
            MessageSink::with_line_mode(Vec::new(), LineMode::WithoutNewline);
        assert_eq!(sink.brand(), Brand::Upstream);
    }

    #[test]
    fn with_line_mode_and_brand_sets_both() {
        let sink: MessageSink<Vec<u8>> =
            MessageSink::with_line_mode_and_brand(Vec::new(), LineMode::WithoutNewline, Brand::Oc);
        assert_eq!(sink.line_mode(), LineMode::WithoutNewline);
        assert_eq!(sink.brand(), Brand::Oc);
    }

    #[test]
    fn with_parts_sets_all_values() {
        let scratch = MessageScratch::new();
        let sink: MessageSink<Vec<u8>> =
            MessageSink::with_parts(Vec::new(), scratch, LineMode::WithoutNewline);
        assert_eq!(sink.line_mode(), LineMode::WithoutNewline);
        assert_eq!(sink.brand(), Brand::Upstream);
    }

    #[test]
    fn with_parts_and_brand_sets_all_values() {
        let scratch = MessageScratch::new();
        let sink: MessageSink<Vec<u8>> = MessageSink::with_parts_and_brand(
            Vec::new(),
            scratch,
            LineMode::WithoutNewline,
            Brand::Oc,
        );
        assert_eq!(sink.line_mode(), LineMode::WithoutNewline);
        assert_eq!(sink.brand(), Brand::Oc);
    }

    #[test]
    fn into_inner_returns_writer() {
        let sink: MessageSink<Vec<u8>> = MessageSink::new(vec![1, 2, 3]);
        let writer = sink.into_inner();
        assert_eq!(writer, vec![1, 2, 3]);
    }

    #[test]
    fn into_parts_returns_all_components() {
        let sink: MessageSink<Vec<u8>> = MessageSink::with_line_mode_and_brand(
            vec![1, 2, 3],
            LineMode::WithoutNewline,
            Brand::Oc,
        );
        let (writer, _scratch, line_mode, brand) = sink.into_parts();
        assert_eq!(writer, vec![1, 2, 3]);
        assert_eq!(line_mode, LineMode::WithoutNewline);
        assert_eq!(brand, Brand::Oc);
    }

    #[test]
    fn default_uses_default_writer() {
        let sink: MessageSink<Vec<u8>> = MessageSink::default();
        assert!(sink.into_inner().is_empty());
    }

    #[test]
    fn default_uses_newline_mode() {
        let sink: MessageSink<Vec<u8>> = MessageSink::default();
        assert_eq!(sink.line_mode(), LineMode::WithNewline);
    }
}
