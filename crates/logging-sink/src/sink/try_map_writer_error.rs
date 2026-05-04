use std::fmt;

use super::MessageSink;

/// Error returned by [`MessageSink::try_map_writer`] when the conversion closure fails.
///
/// The structure preserves ownership of the original [`MessageSink`] together with the
/// error reported by the conversion attempt. This mirrors `std::io::IntoInnerError`
/// so callers can recover the sink and either retry with a different mapping or continue
/// using the existing writer. Helper accessors expose both components without forcing
/// additional allocations, and the wrapper implements rich ergonomics such as [`Clone`],
/// [`as_ref`](Self::as_ref), and [`map_parts`](Self::map_parts) so preserved state can be
/// inspected or transformed without dropping buffered diagnostics.
pub struct TryMapWriterError<W, E> {
    sink: MessageSink<W>,
    error: E,
}

impl<W, E> Clone for TryMapWriterError<W, E>
where
    MessageSink<W>: Clone,
    E: Clone,
{
    fn clone(&self) -> Self {
        Self {
            sink: self.sink.clone(),
            error: self.error.clone(),
        }
    }
}

impl<W, E> TryMapWriterError<W, E> {
    pub(crate) const fn new(sink: MessageSink<W>, error: E) -> Self {
        Self { sink, error }
    }

    /// Returns a reference to the preserved [`MessageSink`].
    #[must_use]
    pub const fn sink(&self) -> &MessageSink<W> {
        &self.sink
    }

    /// Returns a mutable reference to the preserved [`MessageSink`].
    #[must_use]
    pub const fn sink_mut(&mut self) -> &mut MessageSink<W> {
        &mut self.sink
    }

    /// Returns a reference to the conversion error.
    #[must_use]
    pub const fn error(&self) -> &E {
        &self.error
    }

    /// Returns a mutable reference to the conversion error.
    #[must_use]
    pub const fn error_mut(&mut self) -> &mut E {
        &mut self.error
    }

    /// Returns shared references to the preserved sink and error.
    #[must_use]
    pub const fn as_ref(&self) -> (&MessageSink<W>, &E) {
        (&self.sink, &self.error)
    }

    /// Returns mutable references to the preserved sink and error.
    #[must_use]
    pub const fn as_mut(&mut self) -> (&mut MessageSink<W>, &mut E) {
        (&mut self.sink, &mut self.error)
    }

    /// Consumes the wrapper and returns the preserved sink and conversion error.
    #[must_use]
    pub fn into_parts(self) -> (MessageSink<W>, E) {
        (self.sink, self.error)
    }
}

impl<W, E> TryMapWriterError<W, E> {
    /// Consumes the wrapper and returns only the preserved [`MessageSink`].
    #[must_use]
    pub fn into_sink(self) -> MessageSink<W> {
        self.sink
    }

    /// Consumes the wrapper and returns only the conversion error.
    #[must_use]
    pub fn into_error(self) -> E {
        self.error
    }

    /// Maps the preserved sink into another type while retaining the error.
    #[must_use]
    pub fn map_sink<W2, F>(self, map: F) -> TryMapWriterError<W2, E>
    where
        F: FnOnce(MessageSink<W>) -> MessageSink<W2>,
    {
        let (sink, error) = self.into_parts();
        TryMapWriterError::new(map(sink), error)
    }

    /// Maps the preserved error into another type while retaining the sink.
    #[must_use]
    pub fn map_error<E2, F>(self, map: F) -> TryMapWriterError<W, E2>
    where
        F: FnOnce(E) -> E2,
    {
        let (sink, error) = self.into_parts();
        TryMapWriterError::new(sink, map(error))
    }

    /// Transforms both the preserved sink and error in a single pass.
    #[must_use]
    pub fn map_parts<W2, E2, F>(self, map: F) -> TryMapWriterError<W2, E2>
    where
        F: FnOnce(MessageSink<W>, E) -> (MessageSink<W2>, E2),
    {
        let (sink, error) = self.into_parts();
        let (sink, error) = map(sink, error);
        TryMapWriterError::new(sink, error)
    }
}

impl<W, E> From<(MessageSink<W>, E)> for TryMapWriterError<W, E> {
    fn from((sink, error): (MessageSink<W>, E)) -> Self {
        TryMapWriterError::new(sink, error)
    }
}

impl<W, E> fmt::Debug for TryMapWriterError<W, E>
where
    MessageSink<W>: fmt::Debug,
    E: fmt::Debug,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TryMapWriterError")
            .field("sink", &self.sink)
            .field("error", &self.error)
            .finish()
    }
}

impl<W, E> fmt::Display for TryMapWriterError<W, E>
where
    E: fmt::Display,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "failed to map message sink writer: {}", self.error)
    }
}

impl<W, E> std::error::Error for TryMapWriterError<W, E>
where
    E: std::error::Error + fmt::Debug + 'static,
    MessageSink<W>: fmt::Debug,
{
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;

    fn make_sink() -> MessageSink<Vec<u8>> {
        MessageSink::new(Vec::new())
    }

    #[test]
    fn new_creates_error_with_sink_and_error() {
        let sink = make_sink();
        let err = TryMapWriterError::new(sink, "test error");
        assert_eq!(err.error(), &"test error");
    }

    #[test]
    fn sink_returns_reference() {
        let sink = make_sink();
        let err = TryMapWriterError::new(sink, "error");
        let _ = err.sink();
    }

    #[test]
    fn sink_mut_returns_mutable_reference() {
        let sink = make_sink();
        let mut err = TryMapWriterError::new(sink, "error");
        let _ = err.sink_mut();
    }

    #[test]
    fn error_returns_reference() {
        let sink = make_sink();
        let err = TryMapWriterError::new(sink, 42);
        assert_eq!(err.error(), &42);
    }

    #[test]
    fn error_mut_returns_mutable_reference() {
        let sink = make_sink();
        let mut err = TryMapWriterError::new(sink, 42);
        *err.error_mut() = 100;
        assert_eq!(err.error(), &100);
    }

    #[test]
    fn as_ref_returns_both_references() {
        let sink = make_sink();
        let err = TryMapWriterError::new(sink, "error");
        let (sink_ref, error_ref) = err.as_ref();
        let _ = sink_ref;
        assert_eq!(error_ref, &"error");
    }

    #[test]
    fn as_mut_returns_both_mutable_references() {
        let sink = make_sink();
        let mut err = TryMapWriterError::new(sink, 42);
        let (_sink_mut, error_mut) = err.as_mut();
        *error_mut = 100;
        assert_eq!(err.error(), &100);
    }

    #[test]
    fn into_parts_consumes_and_returns_both() {
        let sink = make_sink();
        let err = TryMapWriterError::new(sink, "error");
        let (_sink, error) = err.into_parts();
        assert_eq!(error, "error");
    }

    #[test]
    fn into_sink_consumes_and_returns_sink() {
        let sink = make_sink();
        let err = TryMapWriterError::new(sink, "error");
        let _ = err.into_sink();
    }

    #[test]
    fn into_error_consumes_and_returns_error() {
        let sink = make_sink();
        let err = TryMapWriterError::new(sink, "test error");
        assert_eq!(err.into_error(), "test error");
    }

    #[test]
    fn map_sink_transforms_sink() {
        let sink = make_sink();
        let err: TryMapWriterError<Vec<u8>, &str> = TryMapWriterError::new(sink, "error");
        let mapped = err.map_sink(|s| s.map_writer(io::Cursor::new));
        assert_eq!(mapped.error(), &"error");
    }

    #[test]
    fn map_error_transforms_error() {
        let sink = make_sink();
        let err = TryMapWriterError::new(sink, 42);
        let mapped = err.map_error(|e| e * 2);
        assert_eq!(mapped.error(), &84);
    }

    #[test]
    fn map_parts_transforms_both() {
        let sink = make_sink();
        let err = TryMapWriterError::new(sink, 10);
        let mapped = err.map_parts(|s, e| (s, e + 5));
        assert_eq!(mapped.error(), &15);
    }

    #[test]
    fn from_tuple_creates_error() {
        let sink = make_sink();
        let err: TryMapWriterError<Vec<u8>, &str> = (sink, "error").into();
        assert_eq!(err.error(), &"error");
    }

    #[test]
    fn debug_format_contains_fields() {
        let sink = make_sink();
        let err = TryMapWriterError::new(sink, "test error");
        let debug = format!("{err:?}");
        assert!(debug.contains("TryMapWriterError"));
        assert!(debug.contains("sink"));
        assert!(debug.contains("error"));
    }

    #[test]
    fn display_format_shows_message() {
        let sink = make_sink();
        let err = TryMapWriterError::new(sink, "inner error");
        let display = format!("{err}");
        assert!(display.contains("failed to map"));
        assert!(display.contains("inner error"));
    }

    #[test]
    fn clone_creates_copy() {
        let sink = make_sink();
        let err = TryMapWriterError::new(sink, "error");
        let cloned = err.clone();
        assert_eq!(cloned.error(), &"error");
    }

    #[derive(Debug, Clone)]
    struct TestError(&'static str);

    impl fmt::Display for TestError {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(f, "{}", self.0)
        }
    }

    impl std::error::Error for TestError {}

    #[test]
    fn error_source_returns_inner_error() {
        let sink = make_sink();
        let inner = TestError("inner");
        let err = TryMapWriterError::new(sink, inner);
        let source = std::error::Error::source(&err);
        assert!(source.is_some());
    }
}
