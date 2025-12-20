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
    pub fn sink(&self) -> &MessageSink<W> {
        &self.sink
    }

    /// Returns a mutable reference to the preserved [`MessageSink`].
    #[must_use]
    pub fn sink_mut(&mut self) -> &mut MessageSink<W> {
        &mut self.sink
    }

    /// Returns a reference to the conversion error.
    #[must_use]
    pub fn error(&self) -> &E {
        &self.error
    }

    /// Returns a mutable reference to the conversion error.
    #[must_use]
    pub fn error_mut(&mut self) -> &mut E {
        &mut self.error
    }

    /// Returns shared references to the preserved sink and error.
    #[must_use]
    pub fn as_ref(&self) -> (&MessageSink<W>, &E) {
        (&self.sink, &self.error)
    }

    /// Returns mutable references to the preserved sink and error.
    #[must_use]
    pub fn as_mut(&mut self) -> (&mut MessageSink<W>, &mut E) {
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
