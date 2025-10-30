use std::collections::TryReserveError;
use std::io::{self, IoSliceMut, Write};

use super::super::{BufferedCopyTooSmall, NegotiationBufferAccess, NegotiationBufferedSlices};
use super::base::NegotiatedStream;

impl<R> NegotiatedStream<R> {
    /// Returns the bytes that were required to classify the negotiation prologue.
    #[must_use]
    pub fn sniffed_prefix(&self) -> &[u8] {
        NegotiationBufferAccess::sniffed_prefix(self)
    }

    /// Returns the unread bytes buffered beyond the sniffed negotiation prefix.
    #[must_use]
    pub fn buffered_remainder(&self) -> &[u8] {
        NegotiationBufferAccess::buffered_remainder(self)
    }

    /// Returns the bytes captured during negotiation sniffing, including the prefix and remainder.
    #[must_use]
    pub fn buffered(&self) -> &[u8] {
        NegotiationBufferAccess::buffered(self)
    }

    /// Collects the buffered negotiation transcript into an owned [`Vec<u8>`].
    ///
    /// The helper mirrors [`Self::buffered`] but allocates a new vector sized exactly for the
    /// captured transcript. It reserves the necessary capacity via [`Vec::try_reserve_exact`]
    /// internally, propagating allocation failures as [`TryReserveError`]. Callers that need to own
    /// the replay bytes—for example to stash them in a log or retry a handshake—can use this method
    /// without first preparing a scratch buffer.
    ///
    /// # Examples
    ///
    /// ```
    /// use rsync_transport::sniff_negotiation_stream;
    /// use std::io::Cursor;
    ///
    /// let stream = sniff_negotiation_stream(Cursor::new(b"@RSYNCD: 31.0\nreply".to_vec()))
    ///     .expect("sniff succeeds");
    /// let owned = stream.buffered_to_vec().expect("allocation succeeds");
    /// assert_eq!(owned.as_slice(), stream.buffered());
    /// ```
    #[must_use = "the owned buffer contains the negotiation transcript"]
    pub fn buffered_to_vec(&self) -> Result<Vec<u8>, TryReserveError> {
        NegotiationBufferAccess::buffered_to_vec(self)
    }

    /// Returns the buffered negotiation data split into vectored slices.
    ///
    /// The first slice contains the canonical legacy prefix (if present) while the second slice
    /// holds any additional payload captured alongside the prologue. Callers can forward the
    /// slices directly to [`Write::write_vectored`] without copying the buffered bytes.
    #[must_use]
    pub fn buffered_vectored(&self) -> NegotiationBufferedSlices<'_> {
        NegotiationBufferAccess::buffered_vectored(self)
    }

    /// Copies the buffered negotiation data into a caller-provided vector without consuming it.
    ///
    /// The helper mirrors [`Self::buffered`] but writes into an owned [`Vec<u8>`], allowing callers
    /// to reuse heap storage between sessions. Any additional capacity required to hold the
    /// buffered bytes is reserved via [`Vec::try_reserve`], so allocation failures are surfaced
    /// through [`TryReserveError`]. The vector is cleared before the bytes are appended, matching
    /// upstream rsync's behaviour where replay buffers replace any previous contents.
    ///
    /// # Examples
    ///
    /// ```
    /// use rsync_transport::sniff_negotiation_stream;
    /// use std::io::Cursor;
    ///
    /// let stream = sniff_negotiation_stream(Cursor::new(b"@RSYNCD: 31.0\nreply".to_vec()))
    ///     .expect("sniff succeeds");
    /// let mut replay = Vec::new();
    /// stream
    ///     .copy_buffered_into_vec(&mut replay)
    ///     .expect("vector can reserve space for replay bytes");
    /// assert_eq!(replay.as_slice(), stream.buffered());
    /// ```
    #[must_use = "the result reports whether the replay vector had sufficient capacity"]
    pub fn copy_buffered_into_vec(&self, target: &mut Vec<u8>) -> Result<usize, TryReserveError> {
        NegotiationBufferAccess::copy_buffered_into_vec(self, target)
    }

    /// Copies the unread portion of the buffered negotiation data into `target` without consuming it.
    ///
    /// The helper mirrors [`Self::copy_buffered_into_vec`] but restricts the copy to the bytes that
    /// have not yet been replayed. This is useful when higher layers only need access to the
    /// remaining transcript (for example to resume reading the legacy greeting) while ignoring the
    /// already-consumed prefix. The destination vector is cleared before the bytes are appended and
    /// resized as necessary using [`Vec::try_reserve`].
    #[must_use = "the result reports whether the replay vector had sufficient capacity"]
    pub fn copy_buffered_remaining_into_vec(
        &self,
        target: &mut Vec<u8>,
    ) -> Result<usize, TryReserveError> {
        NegotiationBufferAccess::copy_buffered_remaining_into_vec(self, target)
    }

    /// Collects the unread portion of the buffered negotiation transcript into a new [`Vec<u8>`].
    ///
    /// The helper mirrors [`Self::buffered_remainder`] but returns an owned buffer containing only
    /// the bytes that still need to be replayed. Allocation failures are reported via
    /// [`TryReserveError`], matching the semantics of [`Self::buffered_to_vec`].
    ///
    /// # Examples
    ///
    /// ```
    /// use rsync_transport::sniff_negotiation_stream;
    /// use std::io::Cursor;
    ///
    /// let stream = sniff_negotiation_stream(Cursor::new(b"@RSYNCD: 31.0\nreply".to_vec()))
    ///     .expect("sniff succeeds");
    /// let remainder = stream
    ///     .buffered_remaining_to_vec()
    ///     .expect("allocation succeeds");
    /// assert_eq!(remainder.as_slice(), stream.buffered_remainder());
    /// ```
    #[must_use = "the owned buffer contains the unread negotiation transcript"]
    pub fn buffered_remaining_to_vec(&self) -> Result<Vec<u8>, TryReserveError> {
        NegotiationBufferAccess::buffered_remaining_to_vec(self)
    }

    /// Appends the buffered negotiation data to a caller-provided vector without consuming it.
    ///
    /// The helper is identical to [`Self::copy_buffered_into_vec`] except that it preserves any
    /// existing contents in `target`. This is useful for log buffers that accumulate handshake
    /// transcripts alongside additional context. The vector reserves enough capacity to fit the
    /// buffered bytes via [`Vec::try_reserve`]; on success the method returns the number of bytes
    /// appended. The replay cursor remains unchanged so callers may continue reading from the
    /// stream afterwards.
    ///
    /// # Examples
    ///
    /// ```
    /// use rsync_transport::sniff_negotiation_stream;
    /// use std::io::Cursor;
    ///
    /// let stream = sniff_negotiation_stream(Cursor::new(b"@RSYNCD: 31.0\nreply".to_vec()))
    ///     .expect("sniff succeeds");
    /// let mut log = b"log: ".to_vec();
    /// stream
    ///     .extend_buffered_into_vec(&mut log)
    ///     .expect("vector can reserve space for replay bytes");
    /// assert_eq!(log, b"log: @RSYNCD:");
    /// ```
    #[must_use = "the result reports whether additional capacity was successfully reserved"]
    pub fn extend_buffered_into_vec(&self, target: &mut Vec<u8>) -> Result<usize, TryReserveError> {
        NegotiationBufferAccess::extend_buffered_into_vec(self, target)
    }

    /// Appends the unread portion of the buffered negotiation transcript to `target` without consuming it.
    ///
    /// The helper is the remaining-byte counterpart to [`Self::extend_buffered_into_vec`]. It reserves
    /// space for and appends only the bytes that have not yet been replayed, leaving any previously
    /// consumed prefix untouched. This mirrors upstream rsync's behaviour where diagnostics frequently
    /// record just the pending handshake payload.
    #[must_use = "the result reports whether additional capacity was successfully reserved"]
    pub fn extend_buffered_remaining_into_vec(
        &self,
        target: &mut Vec<u8>,
    ) -> Result<usize, TryReserveError> {
        NegotiationBufferAccess::extend_buffered_remaining_into_vec(self, target)
    }

    /// Returns the sniffed negotiation prefix together with any buffered remainder.
    ///
    /// The tuple mirrors the view exposed by
    /// [`rsync_protocol::NegotiationPrologueSniffer::buffered_split`],
    /// allowing higher layers to borrow both slices simultaneously when staging replay
    /// buffers. The first element contains the portion of the canonical prefix that has not
    /// yet been replayed, while the second slice exposes any additional payload that
    /// arrived alongside the detection bytes and remains buffered.
    #[must_use]
    pub fn buffered_split(&self) -> (&[u8], &[u8]) {
        NegotiationBufferAccess::buffered_split(self)
    }

    /// Returns the total number of buffered bytes staged for replay.
    #[must_use]
    pub fn buffered_len(&self) -> usize {
        NegotiationBufferAccess::buffered_len(self)
    }

    /// Returns how many buffered bytes have already been replayed.
    ///
    /// The counter starts at zero and increases as callers consume data through [`Read::read`],
    /// [`Read::read_vectored`], or [`BufRead::consume`]. Once it matches [`Self::buffered_len`], the
    /// replay buffer has been exhausted and subsequent reads operate on the inner transport.
    ///
    /// # Examples
    ///
    /// ```
    /// use rsync_transport::sniff_negotiation_stream;
    /// use std::io::{Cursor, Read};
    ///
    /// let mut stream = sniff_negotiation_stream(Cursor::new(b"@RSYNCD: 31.0\nreply".to_vec()))
    ///     .expect("sniff succeeds");
    /// assert_eq!(stream.buffered_consumed(), 0);
    ///
    /// let mut buf = [0u8; 4];
    /// stream.read(&mut buf).expect("buffered bytes are available");
    /// assert_eq!(stream.buffered_consumed(), 4);
    /// ```
    #[must_use]
    pub fn buffered_consumed(&self) -> usize {
        NegotiationBufferAccess::buffered_consumed(self)
    }

    /// Returns the portion of the buffered negotiation transcript that has already been replayed.
    ///
    /// The slice mirrors [`Self::buffered_consumed`] but exposes the actual bytes that were drained
    /// from the replay buffer. This is useful for diagnostics that need to log the full transcript or
    /// for higher layers that compare the consumed prefix against known greetings without
    /// recalculating slice ranges manually. The returned slice is empty until data is read from the
    /// [`NegotiatedStream`].
    #[must_use]
    pub fn buffered_consumed_slice(&self) -> &[u8] {
        NegotiationBufferAccess::buffered_consumed_slice(self)
    }

    /// Returns the length of the sniffed negotiation prefix.
    ///
    /// The value is the number of bytes that were required to classify the
    /// negotiation prologue. For legacy ASCII handshakes it matches the length
    /// of the canonical `@RSYNCD:` prefix. The method complements
    /// [`Self::sniffed_prefix_remaining`], which tracks how many of those bytes
    /// have yet to be replayed.
    #[must_use]
    pub const fn sniffed_prefix_len(&self) -> usize {
        self.buffer_storage().sniffed_prefix_len()
    }

    /// Returns how many bytes from the sniffed negotiation prefix remain buffered.
    ///
    /// The value decreases as callers consume the replay data (for example via
    /// [`Read::read`] or [`BufRead::consume`]). A return value of zero indicates that
    /// the entire detection prefix has been drained and subsequent reads operate
    /// directly on the inner transport.
    #[must_use]
    pub fn sniffed_prefix_remaining(&self) -> usize {
        NegotiationBufferAccess::sniffed_prefix_remaining(self)
    }

    /// Reports whether the canonical legacy negotiation prefix has been fully buffered.
    ///
    /// For legacy daemon sessions this becomes `true` once the entire `@RSYNCD:` marker has been
    /// captured during negotiation sniffing. Binary negotiations never observe that prefix, so the
    /// method returns `false` for them. The return value is unaffected by how many buffered bytes
    /// have already been consumed, allowing higher layers to detect whether they may replay the
    /// prefix into the legacy greeting parser without issuing additional reads from the transport.
    #[must_use]
    pub fn legacy_prefix_complete(&self) -> bool {
        NegotiationBufferAccess::legacy_prefix_complete(self)
    }

    /// Returns the remaining number of buffered bytes that have not yet been read.
    #[must_use]
    pub fn buffered_remaining(&self) -> usize {
        NegotiationBufferAccess::buffered_remaining(self)
    }

    /// Returns the portion of the buffered negotiation data that has not been consumed yet.
    ///
    /// The slice begins at the current replay cursor and includes any unread bytes from the
    /// sniffed negotiation prefix followed by buffered payload that arrived alongside the
    /// detection prologue. It mirrors [`BufRead::fill_buf`] behaviour without requiring a mutable
    /// reference, which is useful when callers only need to inspect the remaining transcript for
    /// logging or diagnostics.
    ///
    /// # Examples
    ///
    /// ```
    /// use rsync_transport::sniff_negotiation_stream;
    /// use std::io::{Cursor, Read};
    ///
    /// let mut stream =
    ///     sniff_negotiation_stream(Cursor::new(b"@RSYNCD: 31.0\nreply".to_vec()))
    ///         .expect("sniff succeeds");
    /// let expected = stream.buffered().to_vec();
    /// assert_eq!(stream.buffered_remaining_slice(), expected.as_slice());
    ///
    /// let mut consumed = [0u8; 4];
    /// stream
    ///     .read_exact(&mut consumed)
    ///     .expect("buffered bytes are readable");
    /// assert_eq!(stream.buffered_remaining_slice(), &expected[4..]);
    /// ```
    #[must_use]
    pub fn buffered_remaining_slice(&self) -> &[u8] {
        NegotiationBufferAccess::buffered_remaining_slice(self)
    }

    /// Returns the unread portion of the buffered negotiation data as vectored slices.
    ///
    /// The slices mirror [`Self::buffered_remaining_slice`] but expose the replay data in a form
    /// that integrates with vectored writers. When the sniffed prefix has been partially consumed,
    /// the first slice covers the remaining prefix bytes while the second slice contains any
    /// buffered payload.
    #[must_use]
    pub fn buffered_remaining_vectored(&self) -> NegotiationBufferedSlices<'_> {
        NegotiationBufferAccess::buffered_remaining_vectored(self)
    }

    /// Copies the buffered negotiation prefix and any captured remainder into `target`.
    ///
    /// The helper provides read-only access to the bytes that were observed while detecting the
    /// negotiation style without consuming them. Callers can therefore inspect, log, or cache the
    /// handshake transcript while continuing to rely on the replaying [`Read`] implementation for
    /// subsequent parsing. The destination vector is cleared before new data is written; its
    /// capacity is grown as needed and the resulting length is returned for convenience.
    ///
    /// # Errors
    ///
    /// Propagates [`TryReserveError`] when the destination vector cannot be grown to hold the
    /// buffered bytes. On failure `target` retains its previous contents so callers can recover or
    /// surface the allocation error.
    #[must_use = "the returned length reports how many bytes were copied and whether allocation succeeded"]
    pub fn copy_buffered_into(&self, target: &mut Vec<u8>) -> Result<usize, TryReserveError> {
        NegotiationBufferAccess::copy_buffered_into(self, target)
    }

    /// Copies the buffered negotiation data into the provided vectored buffers without consuming it.
    ///
    /// The helper mirrors [`Self::copy_buffered_into_slice`] but operates on a slice of
    /// [`IoSliceMut`], allowing callers to scatter the replay bytes across multiple scratch buffers
    /// without reallocating. This is particularly useful when staging transcript snapshots inside
    /// pre-allocated ring buffers or logging structures while keeping the replay cursor untouched.
    ///
    /// # Errors
    ///
    /// Returns [`BufferedCopyTooSmall`] when the combined capacity of `bufs` is smaller than the
    /// buffered negotiation payload. On success the buffers are populated sequentially and the total
    /// number of written bytes is returned.
    ///
    /// # Examples
    ///
    /// ```
    /// use rsync_transport::sniff_negotiation_stream;
    /// use std::io::{Cursor, IoSliceMut};
    ///
    /// let stream = sniff_negotiation_stream(Cursor::new(b"@RSYNCD: 31.0\nreply".to_vec()))
    ///     .expect("sniff succeeds");
    /// let expected = stream.buffered().to_vec();
    /// let mut head = [0u8; 12];
    /// let mut tail = [0u8; 32];
    /// let mut bufs = [IoSliceMut::new(&mut head), IoSliceMut::new(&mut tail)];
    /// let copied = stream
    ///     .copy_buffered_into_vectored(&mut bufs)
    ///     .expect("buffers are large enough");
    ///
    /// let prefix_len = head.len().min(copied);
    /// let mut assembled = Vec::new();
    /// assembled.extend_from_slice(&head[..prefix_len]);
    /// let remainder_len = copied - prefix_len;
    /// if remainder_len > 0 {
    ///     assembled.extend_from_slice(&tail[..remainder_len]);
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

    /// Copies the unread portion of the buffered negotiation data into the provided vectored buffers without consuming it.
    #[must_use = "the return value conveys whether the provided buffers were large enough"]
    pub fn copy_buffered_remaining_into_vectored(
        &self,
        bufs: &mut [IoSliceMut<'_>],
    ) -> Result<usize, BufferedCopyTooSmall> {
        NegotiationBufferAccess::copy_buffered_remaining_into_vectored(self, bufs)
    }

    /// Copies the buffered negotiation data into the caller-provided slice without consuming it.
    ///
    /// This mirrors [`Self::copy_buffered_into`] but avoids reallocating a [`Vec`]. Callers that
    /// stage replay data on the stack or inside fixed-size scratch buffers can therefore reuse
    /// their storage while keeping the sniffed bytes untouched. When the destination cannot hold
    /// the buffered data, a [`BufferedCopyTooSmall`] error is returned and the slice remains
    /// unchanged.
    #[must_use = "the result indicates if the destination slice could hold the buffered bytes"]
    pub fn copy_buffered_into_slice(
        &self,
        target: &mut [u8],
    ) -> Result<usize, BufferedCopyTooSmall> {
        NegotiationBufferAccess::copy_buffered_into_slice(self, target)
    }

    /// Copies the unread portion of the buffered negotiation data into `target` without consuming it.
    ///
    /// Unlike [`Self::copy_buffered_into_slice`], which copies the entire transcript, this helper
    /// restricts the operation to bytes that have not yet been replayed. The slice remains unchanged
    /// when it is too small to hold the remaining payload.
    #[must_use = "the result indicates if the destination slice could hold the remaining buffered bytes"]
    pub fn copy_buffered_remaining_into_slice(
        &self,
        target: &mut [u8],
    ) -> Result<usize, BufferedCopyTooSmall> {
        NegotiationBufferAccess::copy_buffered_remaining_into_slice(self, target)
    }

    /// Copies the buffered negotiation data into a caller-provided array without consuming it.
    ///
    /// The helper mirrors [`Self::copy_buffered_into_slice`] but accepts a fixed-size array
    /// directly, allowing stack-allocated scratch storage to be reused without converting it into a
    /// mutable slice at the call site.
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
    ///
    /// The buffered bytes are written exactly once, mirroring upstream rsync's behaviour when the
    /// handshake transcript is echoed into logs or diagnostics. Any I/O error reported by the
    /// writer is propagated unchanged.
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
}
