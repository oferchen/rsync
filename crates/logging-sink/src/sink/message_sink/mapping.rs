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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::line_mode::LineMode;
    use core::branding::Brand;

    fn make_sink() -> MessageSink<Vec<u8>> {
        MessageSink::new(Vec::new())
    }

    #[test]
    fn map_writer_transforms_writer() {
        let sink = MessageSink::new(vec![1, 2, 3]);
        let mapped = sink.map_writer(|v| v.into_iter().map(|x| x * 2).collect::<Vec<_>>());
        assert_eq!(mapped.writer(), &vec![2, 4, 6]);
    }

    #[test]
    fn map_writer_preserves_line_mode() {
        let sink = MessageSink::with_line_mode(Vec::<u8>::new(), LineMode::WithoutNewline);
        let mapped = sink.map_writer(|w| w);
        assert_eq!(mapped.line_mode(), LineMode::WithoutNewline);
    }

    #[test]
    fn map_writer_preserves_brand() {
        let sink = MessageSink::with_brand(Vec::<u8>::new(), Brand::Oc);
        let mapped = sink.map_writer(|w| w);
        assert_eq!(mapped.brand(), Brand::Oc);
    }

    #[test]
    fn map_writer_changes_writer_type() {
        let sink = MessageSink::new(vec![1u8, 2, 3]);
        let mapped: MessageSink<String> = sink.map_writer(|v| String::from_utf8(v).unwrap_or_default());
        assert!(mapped.writer().contains('\u{1}'));
    }

    #[test]
    fn try_map_writer_succeeds_on_ok() {
        let sink = make_sink();
        let result = sink.try_map_writer(|w| Ok::<_, (Vec<u8>, &str)>(w));
        assert!(result.is_ok());
    }

    #[test]
    fn try_map_writer_returns_mapped_sink_on_success() {
        let sink = MessageSink::new(vec![1, 2, 3]);
        let result = sink.try_map_writer(|v| {
            Ok::<_, (Vec<u8>, &str)>(v.into_iter().map(|x| x * 2).collect::<Vec<_>>())
        });
        assert!(result.is_ok());
        let mapped = result.unwrap();
        assert_eq!(mapped.writer(), &vec![2, 4, 6]);
    }

    #[test]
    fn try_map_writer_returns_error_on_failure() {
        let sink = make_sink();
        let result = sink.try_map_writer(|w| Err::<Vec<u8>, _>((w, "error")));
        assert!(result.is_err());
    }

    #[test]
    fn try_map_writer_preserves_sink_on_failure() {
        let sink = MessageSink::new(vec![1, 2, 3]);
        let result = sink.try_map_writer(|w| Err::<Vec<u8>, _>((w, "error")));
        let err = result.unwrap_err();
        assert_eq!(err.sink().writer(), &vec![1, 2, 3]);
    }

    #[test]
    fn try_map_writer_preserves_line_mode_on_success() {
        let sink = MessageSink::with_line_mode(Vec::new(), LineMode::WithoutNewline);
        let result = sink.try_map_writer(|w| Ok::<_, (Vec<u8>, &str)>(w));
        assert!(result.is_ok());
        assert_eq!(result.unwrap().line_mode(), LineMode::WithoutNewline);
    }

    #[test]
    fn try_map_writer_preserves_brand_on_success() {
        let sink = MessageSink::with_brand(Vec::new(), Brand::Oc);
        let result = sink.try_map_writer(|w| Ok::<_, (Vec<u8>, &str)>(w));
        assert!(result.is_ok());
        assert_eq!(result.unwrap().brand(), Brand::Oc);
    }

    #[test]
    fn try_map_writer_preserves_line_mode_on_failure() {
        let sink = MessageSink::with_line_mode(Vec::<u8>::new(), LineMode::WithoutNewline);
        let result = sink.try_map_writer(|w| Err::<Vec<u8>, _>((w, "error")));
        let err = result.unwrap_err();
        assert_eq!(err.sink().line_mode(), LineMode::WithoutNewline);
    }

    #[test]
    fn try_map_writer_error_contains_error_value() {
        let sink = make_sink();
        let result = sink.try_map_writer(|w| Err::<Vec<u8>, _>((w, "specific error")));
        let err = result.unwrap_err();
        assert_eq!(err.error(), &"specific error");
    }

    #[test]
    fn replace_writer_swaps_writers() {
        let mut sink = MessageSink::new(vec![1, 2, 3]);
        let old = sink.replace_writer(vec![4, 5, 6]);
        assert_eq!(old, vec![1, 2, 3]);
        assert_eq!(sink.writer(), &vec![4, 5, 6]);
    }

    #[test]
    fn replace_writer_preserves_line_mode() {
        let mut sink = MessageSink::with_line_mode(Vec::<u8>::new(), LineMode::WithoutNewline);
        let _ = sink.replace_writer(Vec::new());
        assert_eq!(sink.line_mode(), LineMode::WithoutNewline);
    }

    #[test]
    fn replace_writer_preserves_brand() {
        let mut sink = MessageSink::with_brand(Vec::<u8>::new(), Brand::Oc);
        let _ = sink.replace_writer(Vec::new());
        assert_eq!(sink.brand(), Brand::Oc);
    }

    #[test]
    fn replace_writer_returns_old_writer() {
        let mut sink = MessageSink::new(b"old content".to_vec());
        let old = sink.replace_writer(Vec::new());
        assert_eq!(old, b"old content");
    }
}
