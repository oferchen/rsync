use std::collections::TryReserveError;
use std::io::{self, IoSliceMut, Write};

use rsync_protocol::{NegotiationPrologue, NegotiationPrologueSniffer};

use super::super::{
    BufferedCopyTooSmall, NegotiatedStream, NegotiationBuffer, NegotiationBufferAccess,
    NegotiationBufferedSlices,
};
use super::try_map_error::TryMapInnerError;

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
