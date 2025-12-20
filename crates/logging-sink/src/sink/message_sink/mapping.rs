use super::MessageSink;
use crate::TryMapWriterError;
use std::mem;

impl<W> MessageSink<W> {
    /// Maps the sink's writer into a different type while preserving existing state.
    ///
    /// The helper consumes the sink, applies the provided conversion to the
    /// underlying writer, and returns a new sink that reuses the previous
    /// [`core::message::MessageScratch`]. This mirrors patterns such as `BufWriter::into_inner`
    /// where callers often want to hand ownership of the buffered writer to a
    /// higher layer without reinitialising per-sink state.
    #[must_use]
    pub fn map_writer<F, W2>(self, f: F) -> MessageSink<W2>
    where
        F: FnOnce(W) -> W2,
    {
        let MessageSink {
            writer,
            scratch,
            line_mode,
            brand,
        } = self;
        MessageSink::with_parts_and_brand(f(writer), scratch, line_mode, brand)
    }

    /// Attempts to map the sink's writer into a different type, preserving the original sink on failure.
    ///
    /// The closure returns `Ok` with the mapped writer when the conversion succeeds. On error, it
    /// must return the original writer alongside the error value so the method can reconstruct the
    /// [`MessageSink`]. This mirrors [`std::io::IntoInnerError`], allowing callers to recover
    /// without losing buffered diagnostics.
    pub fn try_map_writer<F, W2, E>(self, f: F) -> Result<MessageSink<W2>, TryMapWriterError<W, E>>
    where
        F: FnOnce(W) -> Result<W2, (W, E)>,
    {
        let MessageSink {
            writer,
            scratch,
            line_mode,
            brand,
        } = self;

        match f(writer) {
            Ok(mapped) => Ok(MessageSink::with_parts_and_brand(
                mapped, scratch, line_mode, brand,
            )),
            Err((writer, error)) => Err(TryMapWriterError::new(
                MessageSink::with_parts_and_brand(writer, scratch, line_mode, brand),
                error,
            )),
        }
    }

    /// Replaces the underlying writer while preserving the sink's scratch buffer and [`crate::line_mode::LineMode`].
    ///
    /// The previous writer is returned to the caller so buffered diagnostics can be inspected or
    /// flushed before it is dropped. This avoids rebuilding the entire [`MessageSink`] when the
    /// destination changes—for example, when switching from standard output to a log file mid-run.
    /// The method performs an in-place swap, keeping the existing [`core::message::MessageScratch`] zeroed and
    /// reusing it for subsequent writes.
    #[must_use = "the returned writer contains diagnostics produced before the replacement"]
    pub fn replace_writer(&mut self, mut writer: W) -> W {
        mem::swap(&mut self.writer, &mut writer);
        writer
    }
}
