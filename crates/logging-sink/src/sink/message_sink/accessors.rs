use super::MessageSink;
use crate::LineModeGuard;
use crate::line_mode::LineMode;
use core::{branding::Brand, message::MessageScratch};

impl<W> MessageSink<W> {
    /// Returns a shared reference to the underlying writer.
    ///
    /// The reference allows callers to inspect buffered diagnostics without
    /// consuming the sink. This mirrors APIs such as
    /// [`std::io::BufWriter::get_ref`], making it convenient to peek at
    /// in-memory buffers (for example, when testing message renderers) while
    /// continuing to reuse the same [`MessageSink`].
    #[must_use]
    pub fn writer(&self) -> &W {
        &self.writer
    }

    /// Returns a mutable reference to the underlying writer.
    ///
    /// This is useful when integrations need to adjust writer state before
    /// emitting additional diagnostics. The sink keeps ownership of the writer,
    /// so logging can continue after the mutation.
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
    /// The returned guard implements [`Deref`](std::ops::Deref) and
    /// [`DerefMut`](std::ops::DerefMut), allowing callers to treat it as a
    /// mutable reference to the sink. This mirrors upstream rsync's behaviour of
    /// disabling trailing newlines for progress updates while ensuring the
    /// original configuration is reinstated once the guard is dropped. The guard
    /// carries a `#[must_use]` attribute so ignoring the return value triggers a
    /// lint, preventing accidental one-line overrides that would immediately
    /// revert to the previous mode.
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
    /// internally by [`write`](super::MessageSink::write) and related helpers.
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
    pub fn scratch_mut(&mut self) -> &mut MessageScratch {
        &mut self.scratch
    }

    /// Returns the brand used to render message prefixes.
    #[must_use]
    pub const fn brand(&self) -> Brand {
        self.brand
    }

    /// Updates the brand used to render subsequent messages.
    pub fn set_brand(&mut self, brand: Brand) {
        self.brand = brand;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_sink() -> MessageSink<Vec<u8>> {
        MessageSink::new(Vec::new())
    }

    #[test]
    fn writer_returns_reference() {
        let sink = make_sink();
        let writer = sink.writer();
        assert!(writer.is_empty());
    }

    #[test]
    fn writer_mut_returns_mutable_reference() {
        let mut sink = make_sink();
        sink.writer_mut().push(1);
        assert_eq!(sink.writer(), &vec![1]);
    }

    #[test]
    fn line_mode_returns_current_mode() {
        let sink = make_sink();
        assert_eq!(sink.line_mode(), LineMode::WithNewline);
    }

    #[test]
    fn set_line_mode_changes_mode() {
        let mut sink = make_sink();
        sink.set_line_mode(LineMode::WithoutNewline);
        assert_eq!(sink.line_mode(), LineMode::WithoutNewline);
    }

    #[test]
    fn scoped_line_mode_changes_temporarily() {
        let mut sink = make_sink();
        {
            let _guard = sink.scoped_line_mode(LineMode::WithoutNewline);
            // Mode is changed within the guard scope
        }
        assert_eq!(sink.line_mode(), LineMode::WithNewline);
    }

    #[test]
    fn get_ref_returns_reference() {
        let sink = make_sink();
        let writer = sink.get_ref();
        assert!(writer.is_empty());
    }

    #[test]
    fn get_mut_returns_mutable_reference() {
        let mut sink = make_sink();
        sink.get_mut().push(42);
        assert_eq!(sink.get_ref(), &vec![42]);
    }

    #[test]
    fn scratch_returns_reference() {
        let sink = make_sink();
        let _scratch = sink.scratch();
    }

    #[test]
    fn scratch_mut_returns_mutable_reference() {
        let mut sink = make_sink();
        let _scratch = sink.scratch_mut();
        // Just verify we can get a mutable reference
    }

    #[test]
    fn brand_returns_current_brand() {
        let sink = make_sink();
        assert_eq!(sink.brand(), Brand::Upstream);
    }

    #[test]
    fn set_brand_changes_brand() {
        let mut sink = make_sink();
        sink.set_brand(Brand::Oc);
        assert_eq!(sink.brand(), Brand::Oc);
    }

    #[test]
    fn writer_and_get_ref_are_equivalent() {
        let sink = make_sink();
        assert_eq!(sink.writer(), sink.get_ref());
    }

    #[test]
    fn writer_mut_and_get_mut_are_equivalent() {
        let mut sink = make_sink();
        sink.writer_mut().push(1);
        sink.get_mut().push(2);
        assert_eq!(sink.writer(), &vec![1, 2]);
    }
}
