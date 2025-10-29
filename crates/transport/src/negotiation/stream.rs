use std::collections::TryReserveError;
use std::fmt;
use std::io::{self, BufRead, IoSlice, IoSliceMut, Read, Write};

use rsync_protocol::{
    LEGACY_DAEMON_PREFIX_LEN, LegacyDaemonGreeting, LegacyDaemonMessage, NegotiationPrologue,
    ProtocolVersion, parse_legacy_daemon_greeting_bytes,
    parse_legacy_daemon_greeting_bytes_details, parse_legacy_daemon_message_bytes,
    parse_legacy_error_message_bytes, parse_legacy_warning_message_bytes,
};

use super::parts::{NegotiatedStreamParts, TryMapInnerError};
use super::{
    BufferedCopyTooSmall, NegotiationBuffer, NegotiationBufferAccess, NegotiationBufferedSlices,
    map_line_reserve_error_for_io,
};

/// Result produced when sniffing the negotiation prologue from a transport stream.
///
/// The structure owns the underlying reader together with the bytes that were
/// consumed while determining whether the peer speaks the legacy ASCII
/// `@RSYNCD:` protocol or the binary negotiation introduced in protocol 30. The
/// buffered data is replayed before any further reads from the inner stream,
/// mirroring upstream rsync's behavior where the detection prefix is fed back
/// into the parsing logic.
///
/// When the inner reader implements [`Clone`], the entire [`NegotiatedStream`]
/// can be cloned. The clone retains the buffered negotiation bytes and replay
/// cursor so both instances continue independently—matching upstream helpers
/// that occasionally need to inspect the handshake transcript while preserving
/// the original transport for continued use.
#[derive(Clone, Debug)]
pub struct NegotiatedStream<R> {
    inner: R,
    decision: NegotiationPrologue,
    buffer: NegotiationBuffer,
}

pub const NEGOTIATION_PROLOGUE_UNDETERMINED_MSG: &str =
    "connection closed before rsync negotiation prologue was determined";

impl<R> NegotiationBufferAccess for NegotiatedStream<R> {
    #[inline]
    fn buffer_ref(&self) -> &NegotiationBuffer {
        &self.buffer
    }
}
impl<R> NegotiatedStream<R> {
    /// Returns the negotiation style determined while sniffing the transport.
    #[must_use]
    pub const fn decision(&self) -> NegotiationPrologue {
        self.decision
    }

    /// Reports whether the sniffed negotiation selected the binary protocol.
    ///
    /// The helper mirrors [`NegotiationPrologue::is_binary`] while avoiding the
    /// need for callers to inspect [`Self::decision`] directly. Binary sessions
    /// correspond to remote-shell style negotiations introduced in protocol 30.
    /// When the stream was negotiated through the legacy ASCII daemon flow the
    /// method returns `false`.
    #[must_use]
    pub const fn is_binary(&self) -> bool {
        self.decision.is_binary()
    }

    /// Reports whether the sniffed negotiation selected the legacy ASCII protocol.
    ///
    /// The helper mirrors [`NegotiationPrologue::is_legacy`] so higher layers can
    /// branch on the handshake style without matching on [`Self::decision`]. The
    /// method returns `true` when the transport presented the canonical
    /// `@RSYNCD:` prefix and `false` for binary negotiations.
    #[must_use]
    pub const fn is_legacy(&self) -> bool {
        self.decision.is_legacy()
    }

    /// Ensures the sniffed negotiation matches the expected style.
    ///
    /// The helper mirrors the checks performed by the binary and legacy
    /// handshake wrappers. When the sniffed style matches the expectation the
    /// call succeeds. If the negotiation remains undecided it returns
    /// [`io::ErrorKind::UnexpectedEof`] with the canonical transport error
    /// message. Otherwise it produces an [`io::ErrorKind::InvalidData`] error
    /// with the supplied message. Centralising the logic keeps the error
    /// strings used across the transport crate in sync and avoids drift when
    /// additional call sites are introduced.
    pub fn ensure_decision(
        &self,
        expected: NegotiationPrologue,
        error_message: &'static str,
    ) -> io::Result<()> {
        match self.decision {
            decision if decision == expected => Ok(()),
            NegotiationPrologue::NeedMoreData => Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                NEGOTIATION_PROLOGUE_UNDETERMINED_MSG,
            )),
            _ => Err(io::Error::new(io::ErrorKind::InvalidData, error_message)),
        }
    }

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
        self.buffer.sniffed_prefix_len()
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

    /// Releases the wrapper and returns its components.
    ///
    /// The conversion can also be performed via [`From`] and [`Into`], enabling
    /// callers to decompose the replaying stream without invoking this method
    /// directly.
    ///
    /// ```
    /// use rsync_transport::{sniff_negotiation_stream, NegotiatedStreamParts};
    /// use std::io::Cursor;
    ///
    /// let stream = sniff_negotiation_stream(Cursor::new(b"@RSYNCD: 31.0\n".to_vec()))
    ///     .expect("sniff succeeds");
    /// let parts: NegotiatedStreamParts<_> = stream.into();
    /// assert!(parts.is_legacy());
    /// ```
    #[must_use]
    pub fn into_parts(self) -> NegotiatedStreamParts<R> {
        NegotiatedStreamParts::new(self.decision, self.buffer, self.inner)
    }

    /// Releases the wrapper and returns the raw negotiation components together with the inner reader.
    ///
    /// The tuple mirrors the state captured during sniffing: the detected [`NegotiationPrologue`], the
    /// length of the buffered prefix, the current replay cursor (how many buffered bytes were already
    /// consumed), the owned buffer containing the sniffed bytes, and the inner reader. This variant
    /// avoids cloning the buffer when higher layers need to persist or reuse the sniffed data. The
    /// replay cursor and prefix length are preserved so callers can reconstruct a [`NegotiatedStream`]
    /// using [`Self::from_raw_parts`] without losing track of partially consumed replay bytes.
    #[must_use]
    pub fn into_raw_parts(self) -> (NegotiationPrologue, usize, usize, Vec<u8>, R) {
        self.into_parts().into_raw_parts()
    }

    /// Returns a shared reference to the inner reader.
    #[must_use]
    pub const fn inner(&self) -> &R {
        &self.inner
    }

    /// Returns a mutable reference to the inner reader.
    #[must_use]
    pub fn inner_mut(&mut self) -> &mut R {
        &mut self.inner
    }

    /// Releases the wrapper and returns the inner reader.
    #[must_use]
    pub fn into_inner(self) -> R {
        self.inner
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

    /// Transforms the inner reader while preserving the buffered negotiation state.
    ///
    /// The helper allows callers to wrap the underlying transport (for example to
    /// install additional instrumentation or apply timeout adapters) without
    /// losing the bytes that were already sniffed during negotiation detection.
    /// The replay cursor remains unchanged so subsequent reads continue exactly
    /// where the caller left off. The mapping closure is responsible for
    /// carrying over any relevant state on the inner reader (such as read
    /// positions) before returning the replacement value.
    #[must_use]
    pub fn map_inner<F, T>(self, map: F) -> NegotiatedStream<T>
    where
        F: FnOnce(R) -> T,
    {
        self.into_parts().map_inner(map).into_stream()
    }

    /// Attempts to transform the inner reader while keeping the buffered negotiation state intact.
    ///
    /// The closure returns the replacement reader on success or a tuple containing the error and
    /// original reader on failure. The latter allows callers to recover the original
    /// [`NegotiatedStream`] without losing any replay bytes.
    #[must_use = "the result contains either the mapped stream or the preserved error and original stream"]
    pub fn try_map_inner<F, T, E>(
        self,
        map: F,
    ) -> Result<NegotiatedStream<T>, TryMapInnerError<NegotiatedStream<R>, E>>
    where
        F: FnOnce(R) -> Result<T, (E, R)>,
    {
        self.into_parts()
            .try_map_inner(map)
            .map(NegotiatedStreamParts::into_stream)
            .map_err(|err| err.map_original(NegotiatedStreamParts::into_stream))
    }

    /// Clones the replaying stream by duplicating the inner reader through the provided closure.
    ///
    /// This mirrors the behaviour of transports such as [`std::net::TcpStream`], which expose a
    /// [`try_clone`](std::net::TcpStream::try_clone) method instead of implementing [`Clone`]. The
    /// buffered negotiation state is copied so both the original and the clone replay the captured
    /// prefix and remainder independently. Any error returned by `clone_inner` is propagated
    /// unchanged, leaving the original stream untouched.
    ///
    /// # Examples
    ///
    /// ```
    /// use rsync_transport::sniff_negotiation_stream;
    /// use std::io::{Cursor, Read};
    ///
    /// let stream = sniff_negotiation_stream(Cursor::new(b"@RSYNCD: 31.0\nreply".to_vec()))
    ///     .expect("sniff succeeds");
    /// let mut cloned = stream
    ///     .try_clone_with(|cursor| -> std::io::Result<_> { Ok(cursor.clone()) })
    ///     .expect("cursor clone succeeds");
    ///
    /// let mut replay = Vec::new();
    /// cloned
    ///     .read_to_end(&mut replay)
    ///     .expect("cloned stream replays buffered bytes");
    /// assert_eq!(replay, b"@RSYNCD: 31.0\nreply");
    /// ```
    #[doc(alias = "try_clone")]
    #[must_use = "the result reports whether cloning the inner reader succeeded"]
    pub fn try_clone_with<F, T, E>(&self, clone_inner: F) -> Result<NegotiatedStream<T>, E>
    where
        F: FnOnce(&R) -> Result<T, E>,
    {
        let inner = clone_inner(&self.inner)?;
        Ok(NegotiatedStream {
            inner,
            decision: self.decision,
            buffer: self.buffer.clone(),
        })
    }

    /// Reconstructs a [`NegotiatedStream`] from previously extracted raw components.
    ///
    /// Callers typically pair this with [`Self::into_raw_parts`], allowing negotiation state to be
    /// stored temporarily (for example across async boundaries) and later resumed without replaying the
    /// sniffed prefix. The provided lengths are clamped to the buffer capacity to maintain the
    /// invariants enforced during initial construction.
    #[must_use]
    pub fn from_raw_parts(
        inner: R,
        decision: NegotiationPrologue,
        sniffed_prefix_len: usize,
        buffered_pos: usize,
        buffered: Vec<u8>,
    ) -> Self {
        Self::from_raw_components(inner, decision, sniffed_prefix_len, buffered_pos, buffered)
    }

    pub(crate) fn from_raw_components(
        inner: R,
        decision: NegotiationPrologue,
        sniffed_prefix_len: usize,
        buffered_pos: usize,
        buffered: Vec<u8>,
    ) -> Self {
        Self {
            inner,
            decision,
            buffer: NegotiationBuffer::new(sniffed_prefix_len, buffered_pos, buffered),
        }
    }

    pub(crate) fn from_buffer(
        inner: R,
        decision: NegotiationPrologue,
        buffer: NegotiationBuffer,
    ) -> Self {
        Self {
            inner,
            decision,
            buffer,
        }
    }

    /// Reconstructs a [`NegotiatedStream`] from its previously extracted parts.
    ///
    /// The helper restores the buffered read position so consumers that staged
    /// the stream's state for inspection or temporary ownership changes can
    /// resume reading without replaying bytes that were already delivered. The
    /// clamped invariants mirror the construction performed during negotiation
    /// sniffing to guarantee the prefix length never exceeds the buffered
    /// payload.
    #[must_use]
    pub fn from_parts(parts: NegotiatedStreamParts<R>) -> Self {
        let (decision, buffer, inner) = parts.into_components();
        Self::from_buffer(inner, decision, buffer)
    }
}

impl<R: Read> NegotiatedStream<R> {
    /// Reads the legacy daemon greeting line after the negotiation prefix has been sniffed.
    ///
    /// The method mirrors [`rsync_protocol::read_legacy_daemon_line`] but operates on the
    /// replaying stream wrapper instead of a [`rsync_protocol::NegotiationPrologueSniffer`]. It expects the
    /// negotiation to have been classified as legacy ASCII and the canonical `@RSYNCD:` prefix
    /// to remain fully buffered. Consuming any of the replay bytes before invoking the helper
    /// results in an [`io::ErrorKind::InvalidInput`] error so higher layers cannot accidentally
    /// replay a partial prefix. The captured line (including the terminating newline) is written
    /// into `line`, which is cleared before new data is appended.
    ///
    /// # Errors
    ///
    /// - [`io::ErrorKind::InvalidInput`] if the negotiation is not legacy ASCII, if the prefix is
    ///   incomplete, or if buffered bytes were consumed prior to calling the method.
    /// - [`io::ErrorKind::UnexpectedEof`] if the underlying stream closes before a newline is
    ///   observed.
    /// - [`io::ErrorKind::OutOfMemory`] when reserving space for the output buffer fails.
    #[doc(alias = "@RSYNCD")]
    pub fn read_legacy_daemon_line(&mut self, line: &mut Vec<u8>) -> io::Result<()> {
        self.read_legacy_line(line, true)
    }

    /// Reads and parses the legacy daemon greeting using the replaying stream wrapper.
    ///
    /// The helper forwards to [`Self::read_legacy_daemon_line`] before delegating to
    /// [`rsync_protocol::parse_legacy_daemon_greeting_bytes`]. On success the negotiated
    /// [`ProtocolVersion`] is returned while leaving any bytes after the newline buffered for
    /// subsequent reads.
    #[doc(alias = "@RSYNCD")]
    pub fn read_and_parse_legacy_daemon_greeting(
        &mut self,
        line: &mut Vec<u8>,
    ) -> io::Result<ProtocolVersion> {
        self.read_legacy_daemon_line(line)?;
        parse_legacy_daemon_greeting_bytes(line).map_err(io::Error::from)
    }

    /// Reads and parses the legacy daemon greeting, returning the detailed representation.
    ///
    /// This mirrors [`Self::read_and_parse_legacy_daemon_greeting`] but exposes the structured
    /// [`LegacyDaemonGreeting`] used by higher layers to inspect the advertised protocol number,
    /// subprotocol, and digest list.
    #[doc(alias = "@RSYNCD")]
    pub fn read_and_parse_legacy_daemon_greeting_details<'a>(
        &mut self,
        line: &'a mut Vec<u8>,
    ) -> io::Result<LegacyDaemonGreeting<'a>> {
        self.read_legacy_daemon_line(line)?;
        parse_legacy_daemon_greeting_bytes_details(line).map_err(io::Error::from)
    }

    /// Reads and parses a legacy daemon control message such as `@RSYNCD: OK` or `@RSYNCD: AUTHREQD`.
    ///
    /// The helper mirrors [`rsync_protocol::parse_legacy_daemon_message_bytes`] but operates on the
    /// replaying transport wrapper so callers can continue using [`Read`] after the buffered
    /// negotiation prefix has been replayed. The returned [`LegacyDaemonMessage`] borrows the
    /// supplied buffer, matching the lifetime semantics of the parser from the protocol crate.
    #[doc(alias = "@RSYNCD")]
    pub fn read_and_parse_legacy_daemon_message<'a>(
        &mut self,
        line: &'a mut Vec<u8>,
    ) -> io::Result<LegacyDaemonMessage<'a>> {
        self.read_legacy_line(line, false)?;
        parse_legacy_daemon_message_bytes(line).map_err(io::Error::from)
    }

    /// Reads and parses a legacy daemon error line of the form `@ERROR: ...`.
    ///
    /// Empty payloads are returned as `Some("")`, mirroring the behaviour of
    /// [`rsync_protocol::parse_legacy_error_message_bytes`]. Any parsing failure is converted into
    /// [`io::ErrorKind::InvalidData`], matching the conversion performed by the protocol crate.
    #[doc(alias = "@ERROR")]
    pub fn read_and_parse_legacy_daemon_error_message<'a>(
        &mut self,
        line: &'a mut Vec<u8>,
    ) -> io::Result<Option<&'a str>> {
        self.read_legacy_line(line, false)?;
        parse_legacy_error_message_bytes(line).map_err(io::Error::from)
    }

    /// Reads and parses a legacy daemon warning line of the form `@WARNING: ...`.
    #[doc(alias = "@WARNING")]
    pub fn read_and_parse_legacy_daemon_warning_message<'a>(
        &mut self,
        line: &'a mut Vec<u8>,
    ) -> io::Result<Option<&'a str>> {
        self.read_legacy_line(line, false)?;
        parse_legacy_warning_message_bytes(line).map_err(io::Error::from)
    }
}

impl<R: Read> NegotiatedStream<R> {
    fn read_legacy_line(
        &mut self,
        line: &mut Vec<u8>,
        require_full_prefix: bool,
    ) -> io::Result<()> {
        line.clear();

        match self.decision {
            NegotiationPrologue::LegacyAscii => {}
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "legacy negotiation has not been detected",
                ));
            }
        }

        let prefix_len = self.buffer.sniffed_prefix_len();
        let legacy_prefix_complete = self.buffer.legacy_prefix_complete();
        let remaining = self.buffer.sniffed_prefix_remaining();
        if require_full_prefix {
            if !legacy_prefix_complete {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "legacy negotiation prefix is incomplete",
                ));
            }

            if remaining != LEGACY_DAEMON_PREFIX_LEN {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "legacy negotiation prefix has already been consumed",
                ));
            }
        } else if legacy_prefix_complete && remaining != 0 && remaining != prefix_len {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "legacy negotiation prefix has been partially consumed",
            ));
        }

        self.populate_line_from_buffer(line)
    }

    fn populate_line_from_buffer(&mut self, line: &mut Vec<u8>) -> io::Result<()> {
        while self.buffer.has_remaining() {
            let remaining = self.buffer.remaining_slice();

            if let Some(newline_index) = remaining.iter().position(|&byte| byte == b'\n') {
                let to_copy = newline_index + 1;
                line.try_reserve(to_copy)
                    .map_err(map_line_reserve_error_for_io)?;
                line.extend_from_slice(&remaining[..to_copy]);
                self.buffer.consume(to_copy);
                return Ok(());
            }

            line.try_reserve(remaining.len())
                .map_err(map_line_reserve_error_for_io)?;
            line.extend_from_slice(remaining);
            let consumed = remaining.len();
            self.buffer.consume(consumed);
        }

        let mut byte = [0u8; 1];
        loop {
            match self.inner.read(&mut byte) {
                Ok(0) => {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "EOF while reading legacy rsync daemon line",
                    ));
                }
                Ok(read) => {
                    let observed = &byte[..read];
                    line.try_reserve(observed.len())
                        .map_err(map_line_reserve_error_for_io)?;
                    line.extend_from_slice(observed);
                    if observed.contains(&b'\n') {
                        return Ok(());
                    }
                }
                Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                Err(err) => return Err(err),
            }
        }
    }
}

impl<R: Read> Read for NegotiatedStream<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }

        let copied = self.buffer.copy_into(buf);
        if copied > 0 {
            return Ok(copied);
        }

        self.inner.read(buf)
    }

    fn read_vectored(&mut self, bufs: &mut [IoSliceMut<'_>]) -> io::Result<usize> {
        if bufs.is_empty() {
            return Ok(0);
        }

        let copied = self.buffer.copy_into_vectored(bufs);
        if copied > 0 {
            return Ok(copied);
        }

        self.inner.read_vectored(bufs)
    }
}

impl<R: BufRead> BufRead for NegotiatedStream<R> {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        if self.buffer.has_remaining() {
            return Ok(self.buffer.remaining_slice());
        }

        self.inner.fill_buf()
    }

    fn consume(&mut self, amt: usize) {
        let remainder = self.buffer.consume(amt);
        if remainder > 0 {
            BufRead::consume(&mut self.inner, remainder);
        }
    }
}

impl<R: Write> Write for NegotiatedStream<R> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.inner.write(buf)
    }

    fn write_vectored(&mut self, bufs: &[IoSlice<'_>]) -> io::Result<usize> {
        self.inner.write_vectored(bufs)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }

    fn write_fmt(&mut self, fmt: fmt::Arguments<'_>) -> io::Result<()> {
        self.inner.write_fmt(fmt)
    }
}
