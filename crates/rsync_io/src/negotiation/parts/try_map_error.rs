use std::any::type_name;
use std::fmt;

/// Error returned when mapping the inner transport fails.
///
/// The structure preserves the original value so callers can continue using it after handling the
/// error. This mirrors the ergonomics of APIs such as `BufReader::into_inner`, ensuring buffered
/// negotiation bytes are not lost when a transformation cannot be completed. The type implements
/// [`Clone`] when both captured components support it and provides [`From`] conversions for
/// `(error, original)` tuples, making it straightforward to shuttle the preserved pieces of state
/// between APIs without spelling out `TryMapInnerError::new`.
///
/// # Examples
///
/// Propagate a failed transport transformation without losing the replaying stream. The preserved
/// value can be recovered via [`TryMapInnerError::into_original`] and consumed just like the
/// original [`NegotiatedStream`](crate::negotiation::NegotiatedStream).
///
/// ```
/// use rsync_io::sniff_negotiation_stream;
/// use std::io::{self, Cursor, Read};
///
/// let stream = sniff_negotiation_stream(Cursor::new(b"@RSYNCD: 31.0\n".to_vec()))
///     .expect("sniff succeeds");
/// let result = stream.try_map_inner(|cursor| -> Result<Cursor<Vec<u8>>, (io::Error, Cursor<Vec<u8>>)> {
///     Err((io::Error::new(io::ErrorKind::Other, "wrap failed"), cursor))
/// });
/// let err = result.expect_err("mapping fails");
/// assert_eq!(err.error().kind(), io::ErrorKind::Other);
///
/// let mut restored = err.into_original();
/// let mut replay = Vec::new();
/// restored
///     .read_to_end(&mut replay)
///     .expect("replayed bytes remain available");
/// assert_eq!(replay, b"@RSYNCD: 31.0\n");
/// ```
///
/// The preserved error and transport type are surfaced when formatting the
/// [`TryMapInnerError`], making it easier to log failures without losing
/// context.
///
/// ```
/// use rsync_io::sniff_negotiation_stream;
/// use std::io::{self, Cursor};
///
/// let err = sniff_negotiation_stream(Cursor::new(b"@RSYNCD: 31.0\n".to_vec()))
///     .expect("sniff succeeds")
///     .try_map_inner(|cursor| -> Result<Cursor<Vec<u8>>, (io::Error, Cursor<Vec<u8>>)> {
///         Err((io::Error::new(io::ErrorKind::Other, "wrap failed"), cursor))
///     })
///     .expect_err("mapping fails");
///
/// assert!(format!("{err}").contains("wrap failed"));
/// assert!(format!("{err}").contains("Cursor"));
/// assert!(format!("{err:#}").contains("recover via into_original"));
/// ```
#[derive(Clone)]
pub struct TryMapInnerError<T, E> {
    error: E,
    original: T,
}

impl<T, E> TryMapInnerError<T, E> {
    pub(crate) fn new(error: E, original: T) -> Self {
        Self { error, original }
    }

    /// Returns a shared reference to the underlying error.
    #[must_use]
    pub const fn error(&self) -> &E {
        &self.error
    }

    /// Returns a mutable reference to the underlying error.
    ///
    /// This mirrors [`Self::error`] but allows callers to adjust the preserved error in-place before
    /// reusing the buffered transport state. Upstream rsync occasionally downgrades rich errors to
    /// more specific variants (for example timeouts) prior to surfacing them, so exposing a mutable
    /// handle keeps those transformations possible without reconstructing the entire
    /// [`TryMapInnerError`].
    #[must_use]
    pub fn error_mut(&mut self) -> &mut E {
        &mut self.error
    }

    /// Returns a shared reference to the value that failed to be mapped.
    #[must_use]
    pub const fn original(&self) -> &T {
        &self.original
    }

    /// Returns shared references to both the preserved error and original value.
    ///
    /// This mirrors [`Self::error`] and [`Self::original`] but surfaces both references at once,
    /// making it convenient to inspect the captured state without cloning the
    /// [`TryMapInnerError`]. The helper is particularly useful for logging and debugging flows
    /// where callers want to snapshot the buffered negotiation transcript while examining the
    /// transport error that interrupted the mapping operation.
    ///
    /// # Examples
    ///
    /// ```
    /// use rsync_io::sniff_negotiation_stream;
    /// use std::io::{self, Cursor};
    ///
    /// let stream = sniff_negotiation_stream(Cursor::new(b"@RSYNCD: 31.0\n".to_vec()))
    ///     .expect("sniff succeeds");
    /// let err = stream
    ///     .try_map_inner(|cursor| -> Result<Cursor<Vec<u8>>, (io::Error, Cursor<Vec<u8>>)> {
    ///         Err((io::Error::new(io::ErrorKind::Other, "wrap failed"), cursor))
    ///     })
    ///     .expect_err("mapping fails");
    /// let (error, original) = err.as_ref();
    /// assert_eq!(error.kind(), io::ErrorKind::Other);
    /// let (prefix, remainder) = original.buffered_split();
    /// assert_eq!(prefix, b"@RSYNCD:");
    /// assert!(remainder.is_empty());
    /// ```
    #[must_use]
    pub fn as_ref(&self) -> (&E, &T) {
        (&self.error, &self.original)
    }

    /// Returns a mutable reference to the value that failed to be mapped.
    ///
    /// The mutable accessor mirrors [`Self::original`] and allows callers to prepare the preserved
    /// transport (for example by consuming buffered bytes or tweaking adapter state) before the
    /// error is resolved. The modifications are retained when [`Self::into_original`] is invoked,
    /// matching the behaviour of upstream rsync where negotiation buffers remain usable after a
    /// failed transformation.
    #[must_use]
    pub fn original_mut(&mut self) -> &mut T {
        &mut self.original
    }

    /// Returns mutable references to both the preserved error and original value.
    ///
    /// This helper combines [`Self::error_mut`] and [`Self::original_mut`] so callers can adjust the
    /// stored error while simultaneously preparing the buffered transport state. It is useful when
    /// higher layers downgrade rich I/O errors and consume a portion of the replay buffer before
    /// resuming the transfer.
    ///
    /// # Examples
    ///
    /// ```
    /// use rsync_io::sniff_negotiation_stream;
    /// use std::io::{self, Cursor, Read};
    ///
    /// let mut err = sniff_negotiation_stream(Cursor::new(b"@RSYNCD: 31.0\n".to_vec()))
    ///     .expect("sniff succeeds")
    ///     .try_map_inner(|cursor| -> Result<Cursor<Vec<u8>>, (io::Error, Cursor<Vec<u8>>)> {
    ///         Err((io::Error::new(io::ErrorKind::Other, "wrap failed"), cursor))
    ///     })
    ///     .expect_err("mapping fails");
    /// {
    ///     let (error, original) = err.as_mut();
    ///     *error = io::Error::new(io::ErrorKind::TimedOut, "timeout");
    ///     let mut first = [0u8; 1];
    ///     original
    ///         .read_exact(&mut first)
    ///         .expect("reading from preserved stream succeeds");
    ///     assert_eq!(&first, b"@");
    /// }
    /// assert_eq!(err.error().kind(), io::ErrorKind::TimedOut);
    /// let mut replay = Vec::new();
    /// err.into_original()
    ///     .read_to_end(&mut replay)
    ///     .expect("replay succeeds");
    /// assert_eq!(replay, b"RSYNCD: 31.0\n");
    /// ```
    #[must_use]
    pub fn as_mut(&mut self) -> (&mut E, &mut T) {
        (&mut self.error, &mut self.original)
    }

    /// Consumes the error, returning both the preserved error and original value.
    ///
    /// The helper mirrors [`Self::into_original`] but also yields the captured error so callers can
    /// regain ownership of the replayable transport and the failure that interrupted the mapping in
    /// a single pattern match. This matches upstream rsync's practice of pairing recovered streams
    /// with the diagnostics that triggered the recovery path.
    ///
    /// # Examples
    ///
    /// ```
    /// use rsync_io::sniff_negotiation_stream;
    /// use std::io::{self, Cursor, Read};
    ///
    /// let err = sniff_negotiation_stream(Cursor::new(b"@RSYNCD: 31.0\n".to_vec()))
    ///     .expect("sniff succeeds")
    ///     .try_map_inner(|cursor| -> Result<Cursor<Vec<u8>>, (io::Error, Cursor<Vec<u8>>)> {
    ///         Err((io::Error::new(io::ErrorKind::Other, "wrap failed"), cursor))
    ///     })
    ///     .expect_err("mapping fails");
    ///
    /// let (error, mut original) = err.into_parts();
    /// assert_eq!(error.kind(), io::ErrorKind::Other);
    ///
    /// let mut replay = Vec::new();
    /// original
    ///     .read_to_end(&mut replay)
    ///     .expect("original stream remains readable");
    /// assert_eq!(replay, b"@RSYNCD: 31.0\n");
    /// ```
    #[must_use]
    pub fn into_parts(self) -> (E, T) {
        (self.error, self.original)
    }

    /// Returns ownership of the error, discarding the original value.
    #[must_use]
    pub fn into_error(self) -> E {
        self.error
    }

    /// Returns ownership of the original value, discarding the error.
    #[must_use]
    pub fn into_original(self) -> T {
        self.original
    }

    /// Maps the preserved value into another type while retaining the error.
    #[must_use]
    pub fn map_original<U, F>(self, map: F) -> TryMapInnerError<U, E>
    where
        F: FnOnce(T) -> U,
    {
        let (error, original) = self.into_parts();
        TryMapInnerError::new(error, map(original))
    }

    /// Maps the preserved error into another type while retaining the original value.
    ///
    /// This mirrors [`Self::map_original`] but transforms the stored error instead. It is useful when
    /// callers need to downgrade rich error types (for example to [`std::io::ErrorKind`]) without losing the
    /// buffered transport state captured by [`TryMapInnerError`].
    #[must_use]
    pub fn map_error<E2, F>(self, map: F) -> TryMapInnerError<T, E2>
    where
        F: FnOnce(E) -> E2,
    {
        let (error, original) = self.into_parts();
        TryMapInnerError::new(map(error), original)
    }

    /// Transforms both the preserved error and original value in a single pass.
    ///
    /// The helper complements [`Self::map_error`] and [`Self::map_original`] by
    /// allowing callers to adjust both captured pieces of state atomically. This
    /// matches the needs of higher layers that downcast rich I/O errors while
    /// simultaneously rewrapping the buffered transport. The closure receives
    /// ownership of the stored error and original value and returns their
    /// replacements. The resulting [`TryMapInnerError`] retains the transformed
    /// components so callers can continue working with the preserved transport
    /// data just as they would with the original error.
    ///
    /// # Examples
    ///
    /// ```
    /// use rsync_io::sniff_negotiation_stream;
    /// use std::io::{self, Cursor};
    ///
    /// let stream = sniff_negotiation_stream(Cursor::new(b"@RSYNCD: 31.0\n".to_vec()))
    ///     .expect("sniff succeeds");
    /// let err = stream
    ///     .try_map_inner(|cursor| -> Result<Cursor<Vec<u8>>, (io::Error, Cursor<Vec<u8>>)> {
    ///         Err((io::Error::other("wrap failed"), cursor))
    ///     })
    ///     .expect_err("mapping fails");
    ///
    /// let mapped = err.map_parts(|error, stream| (error.kind(), stream.into_parts()));
    /// assert_eq!(mapped.error(), &io::ErrorKind::Other);
    /// assert_eq!(mapped.original().sniffed_prefix_len(), protocol::LEGACY_DAEMON_PREFIX_LEN);
    /// ```
    #[must_use]
    pub fn map_parts<U, E2, F>(self, map: F) -> TryMapInnerError<U, E2>
    where
        F: FnOnce(E, T) -> (E2, U),
    {
        let (error, original) = self.into_parts();
        let (error, original) = map(error, original);
        TryMapInnerError::new(error, original)
    }
}

impl<T, E: fmt::Debug> fmt::Debug for TryMapInnerError<T, E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let alternate = f.alternate();
        let mut builder = f.debug_struct("TryMapInnerError");
        builder.field("error", &self.error);
        builder.field("original_type", &type_name::<T>());
        if alternate {
            builder.field(
                "recovery",
                &"call into_original() to regain the preserved transport",
            );
        }
        builder.finish()
    }
}

impl<T, E: fmt::Display> fmt::Display for TryMapInnerError<T, E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if f.alternate() {
            write!(
                f,
                "failed to map inner value: {} (original type: {}; recover via into_original())",
                self.error,
                type_name::<T>()
            )
        } else {
            write!(
                f,
                "failed to map inner value: {} (original type: {})",
                self.error,
                type_name::<T>()
            )
        }
    }
}

impl<T, E> std::error::Error for TryMapInnerError<T, E>
where
    E: std::error::Error + 'static,
{
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.error)
    }
}

impl<T, E> From<(E, T)> for TryMapInnerError<T, E> {
    /// Creates an error wrapper from an `(error, original)` tuple.
    #[inline]
    fn from(parts: (E, T)) -> Self {
        Self::new(parts.0, parts.1)
    }
}

impl<T, E> From<TryMapInnerError<T, E>> for (E, T) {
    /// Decomposes the wrapper into its preserved error and original value.
    #[inline]
    fn from(error: TryMapInnerError<T, E>) -> Self {
        error.into_parts()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;

    // ==== Construction ====

    #[test]
    fn new_stores_error_and_original() {
        let err = TryMapInnerError::new("error message", 42);
        assert_eq!(err.error(), &"error message");
        assert_eq!(err.original(), &42);
    }

    #[test]
    fn from_tuple_creates_error() {
        let err: TryMapInnerError<i32, &str> = ("error", 100).into();
        assert_eq!(err.error(), &"error");
        assert_eq!(err.original(), &100);
    }

    // ==== Accessors ====

    #[test]
    fn error_returns_shared_reference() {
        let err = TryMapInnerError::new("test error", "original");
        assert_eq!(*err.error(), "test error");
    }

    #[test]
    fn error_mut_allows_modification() {
        let mut err = TryMapInnerError::new(String::from("initial"), 0);
        *err.error_mut() = String::from("modified");
        assert_eq!(err.error(), "modified");
    }

    #[test]
    fn original_returns_shared_reference() {
        let err = TryMapInnerError::new("err", vec![1, 2, 3]);
        assert_eq!(err.original(), &vec![1, 2, 3]);
    }

    #[test]
    fn original_mut_allows_modification() {
        let mut err = TryMapInnerError::new("err", vec![1, 2, 3]);
        err.original_mut().push(4);
        assert_eq!(err.original(), &vec![1, 2, 3, 4]);
    }

    // ==== as_ref and as_mut ====

    #[test]
    fn as_ref_returns_both_references() {
        let err = TryMapInnerError::new("the error", "the original");
        let (e, o) = err.as_ref();
        assert_eq!(*e, "the error");
        assert_eq!(*o, "the original");
    }

    #[test]
    fn as_mut_returns_mutable_references() {
        let mut err = TryMapInnerError::new(String::from("err"), String::from("orig"));
        {
            let (e, o) = err.as_mut();
            e.push_str("_modified");
            o.push_str("_changed");
        }
        assert_eq!(err.error(), "err_modified");
        assert_eq!(err.original(), "orig_changed");
    }

    // ==== into_* methods ====

    #[test]
    fn into_parts_returns_both_owned() {
        let err = TryMapInnerError::new("my error", 99);
        let (e, o) = err.into_parts();
        assert_eq!(e, "my error");
        assert_eq!(o, 99);
    }

    #[test]
    fn into_error_returns_error_only() {
        let err = TryMapInnerError::new("dropped original", "will be gone");
        assert_eq!(err.into_error(), "dropped original");
    }

    #[test]
    fn into_original_returns_original_only() {
        let err = TryMapInnerError::new("will be gone", "kept original");
        assert_eq!(err.into_original(), "kept original");
    }

    // ==== Mapping methods ====

    #[test]
    fn map_original_transforms_original() {
        let err = TryMapInnerError::new("error", 10);
        let mapped = err.map_original(|n| n * 2);
        assert_eq!(mapped.error(), &"error");
        assert_eq!(mapped.original(), &20);
    }

    #[test]
    fn map_error_transforms_error() {
        let err = TryMapInnerError::new("original error", 42);
        let mapped = err.map_error(|s| s.len());
        assert_eq!(mapped.error(), &14); // "original error".len()
        assert_eq!(mapped.original(), &42);
    }

    #[test]
    fn map_parts_transforms_both() {
        let err = TryMapInnerError::new("err", 5);
        let mapped = err.map_parts(|e, o| (e.len(), o * 10));
        assert_eq!(mapped.error(), &3); // "err".len()
        assert_eq!(mapped.original(), &50);
    }

    #[test]
    fn map_original_changes_type() {
        let err: TryMapInnerError<i32, &str> = TryMapInnerError::new("err", 42);
        let mapped: TryMapInnerError<String, &str> = err.map_original(|n| n.to_string());
        assert_eq!(mapped.original(), "42");
    }

    // ==== From/Into conversions ====

    #[test]
    fn into_tuple_from_error() {
        let err = TryMapInnerError::new("err", 123);
        let (e, o): (&str, i32) = err.into();
        assert_eq!(e, "err");
        assert_eq!(o, 123);
    }

    // ==== Clone ====

    #[test]
    fn clone_produces_independent_copy() {
        let err = TryMapInnerError::new(String::from("err"), vec![1, 2, 3]);
        let cloned = err.clone();
        assert_eq!(err.error(), cloned.error());
        assert_eq!(err.original(), cloned.original());
    }

    // ==== Debug ====

    #[test]
    fn debug_contains_error_and_type() {
        let err = TryMapInnerError::new("debug test", 42i32);
        let debug = format!("{err:?}");
        assert!(debug.contains("TryMapInnerError"));
        assert!(debug.contains("debug test"));
        assert!(debug.contains("i32"));
    }

    #[test]
    fn debug_alternate_includes_recovery_hint() {
        let err = TryMapInnerError::new("test", 0i32);
        let debug = format!("{err:#?}");
        assert!(debug.contains("into_original"));
    }

    // ==== Display ====

    #[test]
    fn display_contains_error_message() {
        let err = TryMapInnerError::new("display test error", 0i32);
        let display = format!("{err}");
        assert!(display.contains("display test error"));
        assert!(display.contains("i32"));
    }

    #[test]
    fn display_alternate_includes_recovery_hint() {
        let err = TryMapInnerError::new("test", 0i32);
        let display = format!("{err:#}");
        assert!(display.contains("recover via into_original"));
    }

    // ==== Error trait ====

    #[test]
    fn error_source_returns_inner_error() {
        let inner = io::Error::new(io::ErrorKind::NotFound, "file not found");
        let err = TryMapInnerError::new(inner, "preserved");
        let source = std::error::Error::source(&err).unwrap();
        let downcasted = source.downcast_ref::<io::Error>().unwrap();
        assert_eq!(downcasted.kind(), io::ErrorKind::NotFound);
    }

    // ==== Edge cases ====

    #[test]
    fn works_with_unit_types() {
        let err = TryMapInnerError::new((), ());
        assert_eq!(err.error(), &());
        assert_eq!(err.original(), &());
    }

    #[test]
    fn works_with_complex_types() {
        let err = TryMapInnerError::new(
            vec![1, 2, 3],
            std::collections::HashMap::<String, i32>::new(),
        );
        assert_eq!(err.error(), &vec![1, 2, 3]);
        assert!(err.original().is_empty());
    }

    #[test]
    fn chain_multiple_maps() {
        let err = TryMapInnerError::new(1, 2);
        let mapped = err
            .map_error(|e| e + 10)
            .map_original(|o| o * 5)
            .map_parts(|e, o| (e * 2, o + 1));
        assert_eq!(mapped.error(), &22); // (1 + 10) * 2
        assert_eq!(mapped.original(), &11); // (2 * 5) + 1
    }
}
