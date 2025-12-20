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
