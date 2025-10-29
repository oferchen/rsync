use std::any::type_name;
use std::collections::TryReserveError;
use std::fmt;
use std::io::{self, IoSliceMut, Write};

use rsync_protocol::{NegotiationPrologue, NegotiationPrologueSniffer};

use super::{
    BufferedCopyTooSmall, NegotiatedStream, NegotiationBuffer, NegotiationBufferAccess,
    NegotiationBufferedSlices,
};

/// Components extracted from a [`NegotiatedStream`].
///
/// # Examples
///
/// Decompose the replaying stream into its constituent pieces and resume consumption once any
/// inspection or wrapping is complete.
///
/// ```
/// use rsync_protocol::NegotiationPrologue;
/// use rsync_transport::sniff_negotiation_stream;
/// use std::io::{Cursor, Read};
///
/// let stream = sniff_negotiation_stream(Cursor::new(b"@RSYNCD: 31.0\nreply".to_vec()))
///     .expect("sniff succeeds");
/// let parts = stream.into_parts();
/// assert_eq!(parts.decision(), NegotiationPrologue::LegacyAscii);
///
/// let mut rebuilt = parts.into_stream();
/// let mut replay = Vec::new();
/// rebuilt
///     .read_to_end(&mut replay)
///     .expect("replayed bytes remain available");
/// assert_eq!(replay, b"@RSYNCD: 31.0\nreply");
/// ```
#[derive(Clone, Debug)]
pub struct NegotiatedStreamParts<R> {
    decision: NegotiationPrologue,
    buffer: NegotiationBuffer,
    inner: R,
}

impl<R> NegotiatedStreamParts<R> {
    pub(crate) fn new(decision: NegotiationPrologue, buffer: NegotiationBuffer, inner: R) -> Self {
        Self {
            decision,
            buffer,
            inner,
        }
    }

    pub(crate) fn into_components(self) -> (NegotiationPrologue, NegotiationBuffer, R) {
        let Self {
            decision,
            buffer,
            inner,
        } = self;
        (decision, buffer, inner)
    }
}

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
/// original [`NegotiatedStream`].
///
/// ```
/// use rsync_transport::sniff_negotiation_stream;
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
/// use rsync_transport::sniff_negotiation_stream;
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
    /// Inspect the preserved error alongside the sniffed negotiation bytes after a failed
    /// transport transformation.
    ///
    /// ```
    /// use rsync_transport::sniff_negotiation_stream;
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
    /// use rsync_transport::sniff_negotiation_stream;
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
    /// use rsync_transport::sniff_negotiation_stream;
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
    /// callers need to downgrade rich error types (for example to [`io::ErrorKind`]) without losing the
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
    /// Convert the stored error into an [`io::ErrorKind`] and turn the
    /// preserved [`NegotiatedStream`] into [`NegotiatedStreamParts`] for later
    /// reuse.
    ///
    /// ```
    /// use rsync_transport::sniff_negotiation_stream;
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
    /// assert_eq!(mapped.original().sniffed_prefix_len(), rsync_protocol::LEGACY_DAEMON_PREFIX_LEN);
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

impl<R> NegotiationBufferAccess for NegotiatedStreamParts<R> {
    #[inline]
    fn buffer_ref(&self) -> &NegotiationBuffer {
        &self.buffer
    }
}

impl<R> NegotiatedStreamParts<R> {
    /// Returns the negotiation style that was detected.
    #[must_use]
    pub const fn decision(&self) -> NegotiationPrologue {
        self.decision
    }

    /// Reports whether the decomposed stream originated from a binary negotiation.
    ///
    /// This mirrors [`NegotiatedStream::is_binary`], allowing callers that work
    /// with [`NegotiatedStreamParts`] to branch on the handshake style without
    /// reconstructing the wrapper or inspecting [`Self::decision`] manually.
    #[must_use]
    pub const fn is_binary(&self) -> bool {
        self.decision.is_binary()
    }

    /// Reports whether the decomposed stream originated from the legacy ASCII negotiation.
    ///
    /// The helper mirrors [`NegotiatedStream::is_legacy`], exposing the same
    /// convenience for code that operates on [`NegotiatedStreamParts`]. It
    /// returns `true` when the captured negotiation began with the canonical
    /// `@RSYNCD:` prefix.
    #[must_use]
    pub const fn is_legacy(&self) -> bool {
        self.decision.is_legacy()
    }

    /// Returns the captured negotiation prefix.
    #[must_use]
    pub fn sniffed_prefix(&self) -> &[u8] {
        NegotiationBufferAccess::sniffed_prefix(self)
    }

    /// Returns the buffered remainder.
    #[must_use]
    pub fn buffered_remainder(&self) -> &[u8] {
        NegotiationBufferAccess::buffered_remainder(self)
    }

    /// Collects the buffered remainder into an owned [`Vec<u8>`].
    ///
    /// The helper mirrors [`Self::buffered_remainder`] while returning an owned buffer. It is
    /// particularly useful for diagnostics that snapshot the unread portion of the transcript without
    /// mutating the decomposed parts.
    #[must_use = "the owned buffer contains the unread negotiation transcript"]
    pub fn buffered_remaining_to_vec(&self) -> Result<Vec<u8>, TryReserveError> {
        NegotiationBufferAccess::buffered_remaining_to_vec(self)
    }

    /// Returns the buffered bytes captured during sniffing.
    #[must_use]
    pub fn buffered(&self) -> &[u8] {
        NegotiationBufferAccess::buffered(self)
    }

    /// Collects the buffered negotiation bytes into an owned [`Vec<u8>`].
    ///
    /// The helper mirrors [`Self::buffered`] but allocates a new vector sized for the captured
    /// transcript. Allocation failures propagate as [`TryReserveError`], matching the semantics of
    /// [`NegotiatedStream::buffered_to_vec`].
    #[must_use = "the owned buffer contains the negotiation transcript"]
    pub fn buffered_to_vec(&self) -> Result<Vec<u8>, TryReserveError> {
        NegotiationBufferAccess::buffered_to_vec(self)
    }

    /// Returns the buffered negotiation data split into vectored slices.
    #[must_use]
    pub fn buffered_vectored(&self) -> NegotiationBufferedSlices<'_> {
        NegotiationBufferAccess::buffered_vectored(self)
    }

    /// Rehydrates a [`NegotiationPrologueSniffer`] using the captured negotiation snapshot.
    ///
    /// The method mirrors the state captured during the initial prologue sniff,
    /// allowing callers to rebuild a sniffer without replaying the underlying
    /// transport. This keeps the high-level session APIs aligned with the
    /// protocol crate helpers that operate on sniffers.
    #[must_use = "the result indicates whether the sniffer could be rehydrated without reallocating"]
    pub fn rehydrate_sniffer(
        &self,
        sniffer: &mut NegotiationPrologueSniffer,
    ) -> Result<(), TryReserveError> {
        sniffer.rehydrate_from_parts(
            self.decision,
            self.buffer.sniffed_prefix_len(),
            self.buffer.buffered(),
        )
    }

    /// Copies the buffered negotiation data into a caller-provided vector without consuming it.
    ///
    /// The helper mirrors [`Self::buffered`] but writes the bytes into an owned [`Vec<u8>`], making
    /// it straightforward to persist handshake transcripts or reuse heap storage across sessions.
    /// The vector is cleared before the data is appended. Any additional capacity required to
    /// complete the copy is reserved using [`Vec::try_reserve`], with allocation failures reported
    /// via [`TryReserveError`].
    ///
    /// # Examples
    ///
    /// ```
    /// use rsync_transport::sniff_negotiation_stream;
    /// use std::io::Cursor;
    ///
    /// let parts = sniff_negotiation_stream(Cursor::new(b"@RSYNCD: 31.0\nreply".to_vec()))
    ///     .expect("sniff succeeds")
    ///     .into_parts();
    /// let mut replay = Vec::with_capacity(4);
    /// parts
    ///     .copy_buffered_into_vec(&mut replay)
    ///     .expect("vector can reserve space for replay bytes");
    /// assert_eq!(replay.as_slice(), parts.buffered());
    /// ```
    #[must_use = "the result reports whether the replay vector had sufficient capacity"]
    pub fn copy_buffered_into_vec(&self, target: &mut Vec<u8>) -> Result<usize, TryReserveError> {
        NegotiationBufferAccess::copy_buffered_into_vec(self, target)
    }

    /// Copies the unread portion of the buffered negotiation data into a caller-provided vector without consuming it.
    #[must_use = "the result reports whether the replay vector had sufficient capacity"]
    pub fn copy_buffered_remaining_into_vec(
        &self,
        target: &mut Vec<u8>,
    ) -> Result<usize, TryReserveError> {
        NegotiationBufferAccess::copy_buffered_remaining_into_vec(self, target)
    }

    /// Appends the buffered negotiation data to a caller-provided vector without consuming it.
    ///
    /// The helper mirrors [`NegotiatedStream::extend_buffered_into_vec`] but operates on decomposed
    /// stream parts. Callers that temporarily separate the components can therefore continue to
    /// accumulate handshake transcripts inside pre-existing log buffers. Additional capacity is
    /// reserved via [`Vec::try_reserve`]; on success the appended length matches the buffered
    /// payload and the replay cursor remains untouched.
    #[must_use = "the result reports whether additional capacity was successfully reserved"]
    pub fn extend_buffered_into_vec(&self, target: &mut Vec<u8>) -> Result<usize, TryReserveError> {
        NegotiationBufferAccess::extend_buffered_into_vec(self, target)
    }

    /// Appends the unread portion of the buffered negotiation transcript to `target` without consuming it.
    #[must_use = "the result reports whether additional capacity was successfully reserved"]
    pub fn extend_buffered_remaining_into_vec(
        &self,
        target: &mut Vec<u8>,
    ) -> Result<usize, TryReserveError> {
        NegotiationBufferAccess::extend_buffered_remaining_into_vec(self, target)
    }

    /// Returns the sniffed negotiation prefix together with any buffered remainder.
    ///
    /// The tuple mirrors [`NegotiatedStream::buffered_split`], giving callers convenient access
    /// to both slices when rebuilding replay buffers without cloning the stored negotiation
    /// bytes.
    #[must_use]
    pub fn buffered_split(&self) -> (&[u8], &[u8]) {
        NegotiationBufferAccess::buffered_split(self)
    }

    /// Returns the total number of bytes captured during sniffing.
    #[must_use]
    pub fn buffered_len(&self) -> usize {
        NegotiationBufferAccess::buffered_len(self)
    }

    /// Returns how many buffered bytes had already been replayed when the stream was decomposed.
    ///
    /// The counter mirrors [`NegotiatedStream::buffered_consumed`], allowing callers to observe the
    /// replay position without reconstructing the wrapper. It is useful when higher layers need to
    /// resume consumption from the point where the stream was split into parts.
    ///
    /// # Examples
    ///
    /// ```
    /// use rsync_transport::sniff_negotiation_stream;
    /// use std::io::{Cursor, Read};
    ///
    /// let mut stream = sniff_negotiation_stream(Cursor::new(b"@RSYNCD: 31.0\nreply".to_vec()))
    ///     .expect("sniff succeeds");
    /// let mut buf = [0u8; 5];
    /// stream.read(&mut buf).expect("buffered bytes are available");
    /// let consumed = stream.buffered_consumed();
    ///
    /// let parts = stream.into_parts();
    /// assert_eq!(parts.buffered_consumed(), consumed);
    /// ```
    #[must_use]
    pub fn buffered_consumed(&self) -> usize {
        NegotiationBufferAccess::buffered_consumed(self)
    }

    /// Returns the portion of the buffered negotiation transcript that had already been replayed.
    ///
    /// This mirrors [`NegotiatedStream::buffered_consumed_slice`] but operates on the decomposed
    /// parts, allowing diagnostics to print the consumed prefix without rebuilding the stream.
    #[must_use]
    pub fn buffered_consumed_slice(&self) -> &[u8] {
        NegotiationBufferAccess::buffered_consumed_slice(self)
    }

    /// Returns how many buffered bytes remain unread.
    #[must_use]
    pub fn buffered_remaining(&self) -> usize {
        NegotiationBufferAccess::buffered_remaining(self)
    }

    /// Returns the portion of the buffered negotiation data that has not been consumed yet.
    ///
    /// The slice starts at the current replay cursor and mirrors what would be produced next if the
    /// parts were turned back into a [`NegotiatedStream`] and read from. This is useful when the
    /// parts are temporarily inspected for diagnostics without rebuilding the wrapper.
    #[must_use]
    pub fn buffered_remaining_slice(&self) -> &[u8] {
        NegotiationBufferAccess::buffered_remaining_slice(self)
    }

    /// Returns the unread portion of the buffered negotiation data as vectored slices.
    #[must_use]
    pub fn buffered_remaining_vectored(&self) -> NegotiationBufferedSlices<'_> {
        NegotiationBufferAccess::buffered_remaining_vectored(self)
    }

    /// Returns the length of the sniffed negotiation prefix.
    #[must_use]
    pub const fn sniffed_prefix_len(&self) -> usize {
        self.buffer.sniffed_prefix_len()
    }

    /// Returns how many bytes from the sniffed negotiation prefix remain buffered.
    ///
    /// This mirrors [`NegotiatedStream::sniffed_prefix_remaining`], giving callers
    /// access to the replay position after extracting the parts without
    /// reconstructing a [`NegotiatedStream`].
    #[must_use]
    pub fn sniffed_prefix_remaining(&self) -> usize {
        NegotiationBufferAccess::sniffed_prefix_remaining(self)
    }

    /// Reports whether the canonical legacy negotiation prefix has been fully buffered.
    ///
    /// The method mirrors [`NegotiatedStream::legacy_prefix_complete`], making it possible to query
    /// the sniffed prefix state even after the stream has been decomposed into parts. It is `true`
    /// for legacy ASCII negotiations once the entire `@RSYNCD:` marker has been captured and `false`
    /// otherwise (including for binary sessions).
    #[must_use]
    pub fn legacy_prefix_complete(&self) -> bool {
        NegotiationBufferAccess::legacy_prefix_complete(self)
    }

    /// Returns the inner reader.
    #[must_use]
    pub const fn inner(&self) -> &R {
        &self.inner
    }

    /// Returns the inner reader mutably.
    #[must_use]
    pub fn inner_mut(&mut self) -> &mut R {
        &mut self.inner
    }

    /// Releases the parts structure and returns the inner reader.
    #[must_use]
    pub fn into_inner(self) -> R {
        self.inner
    }

    /// Transforms the inner reader while keeping the sniffed negotiation state intact.
    ///
    /// This mirrors [`NegotiatedStream::map_inner`] but operates on the extracted
    /// parts, allowing the caller to temporarily take ownership of the inner
    /// reader, wrap it, and later rebuild the replaying stream without cloning
    /// the buffered negotiation bytes. The supplied mapping closure is expected
    /// to retain any pertinent state (for example, the current read position) on
    /// the replacement reader before it is returned.
    #[must_use]
    pub fn map_inner<F, T>(self, map: F) -> NegotiatedStreamParts<T>
    where
        F: FnOnce(R) -> T,
    {
        let Self {
            decision,
            buffer,
            inner,
        } = self;

        NegotiatedStreamParts {
            decision,
            buffer,
            inner: map(inner),
        }
    }

    /// Copies the buffered negotiation bytes into the destination vector without consuming them.
    ///
    /// This mirrors [`NegotiatedStream::copy_buffered_into`], allowing callers that temporarily
    /// decompose the stream into parts to observe the sniffed prefix and remainder while preserving
    /// the replay state. The destination is cleared before data is appended; if additional capacity
    /// is required a [`TryReserveError`] is returned and the original contents remain untouched.
    #[must_use = "the returned length reports how many bytes were copied and whether allocation succeeded"]
    pub fn copy_buffered_into(&self, target: &mut Vec<u8>) -> Result<usize, TryReserveError> {
        NegotiationBufferAccess::copy_buffered_into(self, target)
    }

    /// Copies the buffered negotiation data into the provided vectored buffers without consuming it.
    ///
    /// The helper mirrors [`Self::copy_buffered_into_slice`] while operating on a mutable slice of
    /// [`IoSliceMut`]. This is useful when the stream has been decomposed into parts but callers
    /// still need to scatter the sniffed negotiation transcript across multiple scratch buffers
    /// without cloning the stored bytes.
    ///
    /// # Errors
    ///
    /// Returns [`BufferedCopyTooSmall`] if the combined capacity of `bufs` is smaller than the
    /// buffered negotiation payload.
    ///
    /// # Examples
    ///
    /// ```
    /// use rsync_transport::sniff_negotiation_stream;
    /// use std::io::{Cursor, IoSliceMut};
    ///
    /// let parts = sniff_negotiation_stream(Cursor::new(b"@RSYNCD: 31.0\nreply".to_vec()))
    ///     .expect("sniff succeeds")
    ///     .into_parts();
    /// let expected = parts.buffered().to_vec();
    /// let mut first = [0u8; 10];
    /// let mut second = [0u8; 32];
    /// let mut bufs = [IoSliceMut::new(&mut first), IoSliceMut::new(&mut second)];
    /// let copied = parts
    ///     .copy_buffered_into_vectored(&mut bufs)
    ///     .expect("buffers are large enough");
    ///
    /// let prefix_len = first.len().min(copied);
    /// let mut assembled = Vec::new();
    /// assembled.extend_from_slice(&first[..prefix_len]);
    /// let remainder_len = copied - prefix_len;
    /// if remainder_len > 0 {
    ///     assembled.extend_from_slice(&second[..remainder_len]);
    /// }
    /// assert_eq!(assembled, expected);
    /// ```
    #[must_use = "the return value conveys whether the provided buffers were large enough"]
    pub fn copy_buffered_into_vectored(
        &self,
        bufs: &mut [IoSliceMut<'_>],
    ) -> Result<usize, BufferedCopyTooSmall> {
        NegotiationBufferAccess::copy_buffered_into_vectored(self, bufs)
    }

    /// Copies the buffered negotiation data into the caller-provided slice without consuming it.
    #[must_use = "the result indicates if the destination slice could hold the buffered bytes"]
    pub fn copy_buffered_into_slice(
        &self,
        target: &mut [u8],
    ) -> Result<usize, BufferedCopyTooSmall> {
        NegotiationBufferAccess::copy_buffered_into_slice(self, target)
    }

    /// Copies the unread portion of the buffered negotiation data into `target` without consuming it.
    #[must_use = "the result indicates if the destination slice could hold the remaining buffered bytes"]
    pub fn copy_buffered_remaining_into_slice(
        &self,
        target: &mut [u8],
    ) -> Result<usize, BufferedCopyTooSmall> {
        NegotiationBufferAccess::copy_buffered_remaining_into_slice(self, target)
    }

    /// Copies the buffered negotiation data into a caller-provided array without consuming it.
    #[must_use = "the result indicates if the destination array could hold the buffered bytes"]
    pub fn copy_buffered_into_array<const N: usize>(
        &self,
        target: &mut [u8; N],
    ) -> Result<usize, BufferedCopyTooSmall> {
        NegotiationBufferAccess::copy_buffered_into_array(self, target)
    }

    /// Copies the unread portion of the buffered negotiation data into a caller-provided array without consuming it.
    #[must_use = "the result indicates if the destination array could hold the remaining buffered bytes"]
    pub fn copy_buffered_remaining_into_array<const N: usize>(
        &self,
        target: &mut [u8; N],
    ) -> Result<usize, BufferedCopyTooSmall> {
        NegotiationBufferAccess::copy_buffered_remaining_into_array(self, target)
    }

    /// Streams the buffered negotiation data into the provided writer without consuming it.
    #[must_use = "the returned length reports how many bytes were written and surfaces I/O failures"]
    pub fn copy_buffered_into_writer<W: Write>(&self, target: &mut W) -> io::Result<usize> {
        NegotiationBufferAccess::copy_buffered_into_writer(self, target)
    }

    /// Streams the unread portion of the buffered negotiation data into the provided writer.
    #[must_use = "the returned length reports how many bytes were written and surfaces I/O failures"]
    pub fn copy_buffered_remaining_into_writer<W: Write>(
        &self,
        target: &mut W,
    ) -> io::Result<usize> {
        NegotiationBufferAccess::copy_buffered_remaining_into_writer(self, target)
    }

    /// Copies the unread portion of the buffered negotiation data into the provided vectored buffers without consuming it.
    #[must_use = "the return value conveys whether the provided buffers were large enough"]
    pub fn copy_buffered_remaining_into_vectored(
        &self,
        bufs: &mut [IoSliceMut<'_>],
    ) -> Result<usize, BufferedCopyTooSmall> {
        NegotiationBufferAccess::copy_buffered_remaining_into_vectored(self, bufs)
    }

    /// Attempts to transform the inner reader while preserving the buffered negotiation state.
    ///
    /// When the mapping fails the original reader is returned alongside the error, ensuring callers
    /// retain access to the sniffed bytes without needing to re-run negotiation detection.
    #[must_use = "the result contains either the mapped parts or the preserved error and original parts"]
    pub fn try_map_inner<F, T, E>(
        self,
        map: F,
    ) -> Result<NegotiatedStreamParts<T>, TryMapInnerError<NegotiatedStreamParts<R>, E>>
    where
        F: FnOnce(R) -> Result<T, (E, R)>,
    {
        let Self {
            decision,
            buffer,
            inner,
        } = self;

        match map(inner) {
            Ok(mapped) => Ok(NegotiatedStreamParts {
                decision,
                buffer,
                inner: mapped,
            }),
            Err((error, original)) => Err(TryMapInnerError::new(
                error,
                NegotiatedStreamParts {
                    decision,
                    buffer,
                    inner: original,
                },
            )),
        }
    }

    /// Clones the decomposed negotiation state using a caller-provided duplication strategy.
    ///
    /// The helper mirrors [`NegotiatedStream::try_clone_with`] while operating on extracted parts.
    /// It is particularly useful when the inner transport exposes an inherent
    /// [`try_clone`](std::net::TcpStream::try_clone)-style API instead of [`Clone`]. The buffered
    /// negotiation bytes are copied so the original and cloned parts can be converted into replaying
    /// streams without affecting each other's progress. Errors from `clone_inner` are propagated
    /// unchanged.
    ///
    /// # Examples
    ///
    /// ```
    /// use rsync_transport::sniff_negotiation_stream;
    /// use std::io::{Cursor, Read};
    ///
    /// let parts = sniff_negotiation_stream(Cursor::new(b"@RSYNCD: 30.0\nhello".to_vec()))
    ///     .expect("sniff succeeds")
    ///     .into_parts();
    /// let mut cloned = parts
    ///     .try_clone_with(|cursor| -> std::io::Result<_> { Ok(cursor.clone()) })
    ///     .expect("cursor clone succeeds");
    ///
    /// let mut replay = Vec::new();
    /// cloned
    ///     .into_stream()
    ///     .read_to_end(&mut replay)
    ///     .expect("cloned parts replay buffered bytes");
    /// assert_eq!(replay, b"@RSYNCD: 30.0\nhello");
    /// ```
    #[doc(alias = "try_clone")]
    #[must_use = "the result reports whether cloning the inner reader succeeded"]
    pub fn try_clone_with<F, T, E>(&self, clone_inner: F) -> Result<NegotiatedStreamParts<T>, E>
    where
        F: FnOnce(&R) -> Result<T, E>,
    {
        let inner = clone_inner(&self.inner)?;
        Ok(NegotiatedStreamParts {
            decision: self.decision,
            buffer: self.buffer.clone(),
            inner,
        })
    }

    /// Reassembles a [`NegotiatedStream`] from the extracted components.
    ///
    /// Callers can temporarily inspect or adjust the buffered negotiation
    /// state (for example, updating transport-level settings on the inner
    /// reader) and then continue consuming data through the replaying wrapper
    /// without cloning the sniffed bytes. The same reconstruction is available
    /// through [`From`] and [`Into`], allowing callers to rebuild the replaying
    /// stream via trait-based conversions.
    ///
    /// ```
    /// use rsync_transport::{sniff_negotiation_stream, NegotiatedStream};
    /// use std::io::Cursor;
    ///
    /// let parts = sniff_negotiation_stream(Cursor::new(b"@RSYNCD: 31.0\n".to_vec()))
    ///     .expect("sniff succeeds")
    ///     .into_parts();
    /// let stream = NegotiatedStream::from(parts);
    /// assert!(stream.is_legacy());
    /// ```
    #[must_use]
    pub fn into_stream(self) -> NegotiatedStream<R> {
        NegotiatedStream::from_buffer(self.inner, self.decision, self.buffer)
    }

    /// Releases the parts structure and returns the raw negotiation components together with the reader.
    ///
    /// The returned tuple includes the detected [`NegotiationPrologue`], the length of the sniffed
    /// prefix, the number of buffered bytes that were already consumed, the owned buffer containing the
    /// sniffed data, and the inner reader. This mirrors the layout used by [`NegotiatedStream::into_raw_parts`]
    /// while avoiding an intermediate reconstruction of the wrapper when only the raw buffers are needed.
    #[must_use]
    pub fn into_raw_parts(self) -> (NegotiationPrologue, usize, usize, Vec<u8>, R) {
        let (decision, buffer, inner) = self.into_components();
        let (sniffed_prefix_len, buffered_pos, buffered) = buffer.into_raw_parts();
        (decision, sniffed_prefix_len, buffered_pos, buffered, inner)
    }
}

impl<R> From<NegotiatedStream<R>> for NegotiatedStreamParts<R> {
    fn from(stream: NegotiatedStream<R>) -> Self {
        stream.into_parts()
    }
}

impl<R> From<NegotiatedStreamParts<R>> for NegotiatedStream<R> {
    fn from(parts: NegotiatedStreamParts<R>) -> Self {
        parts.into_stream()
    }
}
