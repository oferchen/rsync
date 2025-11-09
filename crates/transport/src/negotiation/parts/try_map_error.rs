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
/// use transport::sniff_negotiation_stream;
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
/// use transport::sniff_negotiation_stream;
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
    /// use transport::sniff_negotiation_stream;
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
    /// use transport::sniff_negotiation_stream;
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
    /// use transport::sniff_negotiation_stream;
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
    /// use transport::sniff_negotiation_stream;
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
