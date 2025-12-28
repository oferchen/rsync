use super::MessageSink;
use crate::line_mode::LineMode;

/// RAII guard that temporarily overrides a [`MessageSink`]'s [`LineMode`].
///
/// Instances of this guard are created by [`MessageSink::scoped_line_mode`]. While the guard is
/// alive, all writes issued through it or the underlying sink use the scoped line mode. Dropping the
/// guard automatically restores the previous line mode, mirroring upstream rsync's practice of
/// toggling newline behaviour when rendering progress updates. The guard implements
/// [`Deref`](std::ops::Deref) and [`DerefMut`](std::ops::DerefMut) so callers can seamlessly invoke
/// sink methods without additional boilerplate.
#[must_use = "dropping the guard immediately restores the previous line mode"]
pub struct LineModeGuard<'a, W> {
    sink: Option<&'a mut MessageSink<W>>,
    previous: LineMode,
}

impl<'a, W> LineModeGuard<'a, W> {
    pub(crate) const fn new(sink: &'a mut MessageSink<W>, previous: LineMode) -> Self {
        Self {
            sink: Some(sink),
            previous,
        }
    }

    /// Returns the [`LineMode`] that will be restored when the guard is dropped.
    #[must_use]
    pub const fn previous_line_mode(&self) -> LineMode {
        self.previous
    }

    /// Consumes the guard without restoring the previous [`LineMode`].
    ///
    /// Dropping a [`LineModeGuard`] normally reinstates the configuration that was in effect
    /// before [`MessageSink::scoped_line_mode`] was called. This helper intentionally skips that
    /// restoration so the temporary override becomes the sink's new baseline. It returns the
    /// underlying [`MessageSink`], allowing callers to continue writing messages or adjust the line
    /// mode again explicitly.
    ///
    /// # Examples
    ///
    /// Permanently adopt a newline-free mode after performing some initial writes:
    ///
    /// ```
    /// use core::message::Message;
    /// use logging_sink::{LineMode, MessageSink};
    ///
    /// let mut sink = MessageSink::new(Vec::new());
    /// {
    ///     let sink = sink
    ///         .scoped_line_mode(LineMode::WithoutNewline)
    ///         .into_inner();
    ///     sink.write(Message::info("phase one")).expect("write succeeds");
    /// }
    ///
    /// sink.write(Message::info("phase two")).expect("write succeeds");
    /// assert_eq!(sink.line_mode(), LineMode::WithoutNewline);
    /// assert_eq!(
    ///     sink.into_inner(),
    ///     b"rsync info: phase onersync info: phase two".to_vec()
    /// );
    /// ```
    pub fn into_inner(mut self) -> &'a mut MessageSink<W> {
        self.sink
            .take()
            .expect("line mode guard must own a message sink")
    }
}

impl<'a, W> Drop for LineModeGuard<'a, W> {
    fn drop(&mut self) {
        if let Some(sink) = self.sink.take() {
            sink.set_line_mode(self.previous);
        }
    }
}

impl<'a, W> std::ops::Deref for LineModeGuard<'a, W> {
    type Target = MessageSink<W>;

    fn deref(&self) -> &Self::Target {
        self.sink
            .as_deref()
            .expect("line mode guard remains active while borrowed")
    }
}

impl<'a, W> std::ops::DerefMut for LineModeGuard<'a, W> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.sink
            .as_deref_mut()
            .expect("line mode guard remains active while borrowed")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_sink() -> MessageSink<Vec<u8>> {
        MessageSink::new(Vec::new())
    }

    #[test]
    fn previous_line_mode_returns_stored_mode() {
        let mut sink = make_sink();
        let guard = LineModeGuard::new(&mut sink, LineMode::WithNewline);
        assert_eq!(guard.previous_line_mode(), LineMode::WithNewline);
    }

    #[test]
    fn previous_line_mode_returns_without_newline() {
        let mut sink = make_sink();
        let guard = LineModeGuard::new(&mut sink, LineMode::WithoutNewline);
        assert_eq!(guard.previous_line_mode(), LineMode::WithoutNewline);
    }

    #[test]
    fn into_inner_returns_sink_reference() {
        let mut sink = make_sink();
        let guard = LineModeGuard::new(&mut sink, LineMode::WithNewline);
        let inner = guard.into_inner();
        let _ = inner;
    }

    #[test]
    fn drop_restores_previous_line_mode() {
        let mut sink = make_sink();
        sink.set_line_mode(LineMode::WithNewline);
        {
            sink.set_line_mode(LineMode::WithoutNewline);
            let _guard = LineModeGuard::new(&mut sink, LineMode::WithNewline);
        }
        assert_eq!(sink.line_mode(), LineMode::WithNewline);
    }

    #[test]
    fn deref_allows_access_to_sink() {
        let mut sink = make_sink();
        let guard = LineModeGuard::new(&mut sink, LineMode::WithNewline);
        let _ = guard.line_mode();
    }

    #[test]
    fn deref_mut_allows_mutable_access() {
        let mut sink = make_sink();
        let mut guard = LineModeGuard::new(&mut sink, LineMode::WithNewline);
        guard.set_line_mode(LineMode::WithoutNewline);
    }

    #[test]
    fn into_inner_skips_restoration() {
        let mut sink = make_sink();
        sink.set_line_mode(LineMode::WithNewline);
        {
            let guard = LineModeGuard::new(&mut sink, LineMode::WithNewline);
            let inner = guard.into_inner();
            inner.set_line_mode(LineMode::WithoutNewline);
        }
        assert_eq!(sink.line_mode(), LineMode::WithoutNewline);
    }
}
