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
    pub(crate) fn new(sink: &'a mut MessageSink<W>, previous: LineMode) -> Self {
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
    /// use logging::{LineMode, MessageSink};
    ///
    /// let mut sink = MessageSink::new(Vec::new());
    /// {
    ///     let sink = sink
    ///         .scoped_line_mode(LineMode::WithoutNewline)
    ///         .into_inner();
    ///     sink.write(Message::info("phase one"))?;
    /// }
    ///
    /// sink.write(Message::info("phase two"))?;
    /// assert_eq!(sink.line_mode(), LineMode::WithoutNewline);
    /// assert_eq!(
    ///     sink.into_inner(),
    ///     b"rsync info: phase onersync info: phase two".to_vec()
    /// );
    /// # Ok::<(), std::io::Error>(())
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
