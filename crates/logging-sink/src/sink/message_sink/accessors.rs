use super::MessageSink;
use crate::LineModeGuard;
use crate::escape::{self, EscapeStyle};
use crate::line_mode::LineMode;
use core::{branding::Brand, message::MessageScratch};

impl<W> MessageSink<W> {
    /// Returns a shared reference to the underlying writer.
    ///
    /// Mirrors APIs such as [`std::io::BufWriter::get_ref`] so callers can
    /// inspect buffered diagnostics without consuming the sink.
    #[must_use]
    pub const fn writer(&self) -> &W {
        &self.writer
    }

    /// Returns a mutable reference to the underlying writer.
    ///
    /// The sink keeps ownership of the writer, so logging can continue after
    /// the mutation.
    pub const fn writer_mut(&mut self) -> &mut W {
        &mut self.writer
    }

    /// Returns the current [`LineMode`].
    #[must_use]
    pub const fn line_mode(&self) -> LineMode {
        self.line_mode
    }

    /// Updates the [`LineMode`] used for subsequent writes.
    pub const fn set_line_mode(&mut self, line_mode: LineMode) {
        self.line_mode = line_mode;
    }

    /// Temporarily overrides the sink's [`LineMode`], restoring the previous value on drop.
    ///
    /// The returned guard implements [`Deref`](std::ops::Deref) and
    /// [`DerefMut`](std::ops::DerefMut), so callers can treat it as a mutable
    /// reference to the sink. This mirrors upstream rsync's behaviour of
    /// disabling trailing newlines for progress updates while ensuring the
    /// original configuration is reinstated once the guard is dropped.
    #[must_use = "bind the guard to retain the temporary line mode override for its scope"]
    pub const fn scoped_line_mode(&mut self, line_mode: LineMode) -> LineModeGuard<'_, W> {
        let previous = self.line_mode;
        self.line_mode = line_mode;
        LineModeGuard::new(self, previous)
    }

    /// Borrows the underlying writer.
    #[must_use]
    pub const fn get_ref(&self) -> &W {
        &self.writer
    }

    /// Mutably borrows the underlying writer.
    #[must_use]
    pub const fn get_mut(&mut self) -> &mut W {
        &mut self.writer
    }

    /// Returns a shared reference to the reusable [`MessageScratch`] buffer.
    #[must_use]
    pub const fn scratch(&self) -> &MessageScratch {
        &self.scratch
    }

    /// Returns a mutable reference to the sink's [`MessageScratch`] buffer.
    ///
    /// Callers can reset or prepopulate the scratch storage before emitting
    /// diagnostics. Because the buffer is reused across writes, manual
    /// initialisation can help enforce deterministic state when toggling
    /// between sinks that share a scratch instance.
    pub const fn scratch_mut(&mut self) -> &mut MessageScratch {
        &mut self.scratch
    }

    /// Returns the brand used to render message prefixes.
    #[must_use]
    pub const fn brand(&self) -> Brand {
        self.brand
    }

    /// Updates the brand used to render subsequent messages.
    pub const fn set_brand(&mut self, brand: Brand) {
        self.brand = brand;
    }

    /// Makes every rendered message escape its non-printable bytes as a
    /// terminal line, or stop doing so.
    ///
    /// upstream: log.c:499 `rwrite()` hands every FERROR/FWARNING/FINFO
    /// line to `filtered_fwrite()` (or, for a client without `-8`, through the
    /// `ic_chck` check at log.c:463-496 first), so a control byte in a name or
    /// in a refused rule prints as `\#ooo`. A sink whose writer already
    /// escapes - the log file - leaves this off, since escaping twice turns
    /// `\#013` into `\#134#013`.
    pub const fn set_terminal_escaping(&mut self, enabled: bool) {
        self.terminal_escaping = enabled;
    }

    /// The escape style this sink applies to the messages it renders: the
    /// process terminal style when terminal escaping is on, passthrough
    /// otherwise.
    ///
    /// The terminal style is read at call time because `-8` is parsed after
    /// the sink is built.
    #[must_use]
    pub fn escape_style(&self) -> EscapeStyle {
        if self.terminal_escaping {
            escape::terminal_style()
        } else {
            EscapeStyle::passthrough()
        }
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
