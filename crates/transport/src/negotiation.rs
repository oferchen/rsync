use std::any::type_name;
use std::collections::TryReserveError;
use std::fmt;
use std::io::{self, BufRead, IoSlice, IoSliceMut, Read, Write};
use std::slice;

use rsync_protocol::{
    LEGACY_DAEMON_PREFIX_LEN, LegacyDaemonGreeting, LegacyDaemonMessage, NegotiationPrologue,
    NegotiationPrologueSniffer, ProtocolVersion, parse_legacy_daemon_greeting_bytes,
    parse_legacy_daemon_greeting_bytes_details, parse_legacy_daemon_message_bytes,
    parse_legacy_error_message_bytes, parse_legacy_warning_message_bytes,
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

/// Vectored view over buffered negotiation data.
///
/// The structure exposes up to two [`IoSlice`] segments: the remaining portion of the
/// canonical legacy prefix (`@RSYNCD:`) and any buffered payload that followed the prologue.
/// Consumers obtain instances via [`NegotiatedStream::buffered_vectored`],
/// [`NegotiatedStream::buffered_remaining_vectored`], or their counterparts on
/// [`NegotiatedStreamParts`]. The iterator interface allows the slices to be passed directly to
/// [`Write::write_vectored`] without allocating intermediate buffers, flattened into a
/// [`Vec<u8>`] via [`extend_vec`](NegotiationBufferedSlices::extend_vec), or iterated over
/// using the standard `IntoIterator` trait implementations.
///
/// # Examples
///
/// ```
/// use rsync_transport::sniff_negotiation_stream;
/// use std::io::Cursor;
///
/// let stream = sniff_negotiation_stream(Cursor::new(b"@RSYNCD: 31.0\nreply".to_vec()))
///     .expect("sniff succeeds");
/// let vectored = stream.buffered_vectored();
///
/// let mut flattened = Vec::with_capacity(vectored.len());
/// for slice in vectored.iter() {
///     flattened.extend_from_slice(slice);
/// }
///
/// assert_eq!(flattened, stream.buffered());
/// ```
#[derive(Clone, Debug)]
pub struct NegotiationBufferedSlices<'a> {
    segments: [IoSlice<'a>; 2],
    count: usize,
    total_len: usize,
}

impl<'a> NegotiationBufferedSlices<'a> {
    fn new(prefix: &'a [u8], remainder: &'a [u8]) -> Self {
        let mut segments = [IoSlice::new(&[]); 2];
        let mut count = 0usize;

        if !prefix.is_empty() {
            segments[count] = IoSlice::new(prefix);
            count += 1;
        }

        if !remainder.is_empty() {
            segments[count] = IoSlice::new(remainder);
            count += 1;
        }

        Self {
            segments,
            count,
            total_len: prefix.len() + remainder.len(),
        }
    }

    /// Returns the populated slice view over the underlying [`IoSlice`] array.
    #[must_use]
    pub fn as_slices(&self) -> &[IoSlice<'a>] {
        &self.segments[..self.count]
    }

    /// Returns the number of buffered bytes represented by the vectored view.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.total_len
    }

    /// Reports whether any data is present in the vectored representation.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Returns the number of slices that were populated.
    #[must_use]
    pub const fn segment_count(&self) -> usize {
        self.count
    }

    /// Returns an iterator over the populated slices.
    #[must_use = "callers must iterate to observe the buffered slices"]
    pub fn iter(&self) -> slice::Iter<'_, IoSlice<'a>> {
        self.as_slices().iter()
    }

    /// Extends the provided buffer with the buffered negotiation bytes.
    ///
    /// The helper reserves exactly enough additional capacity to append the
    /// replay prefix and payload while preserving their original ordering. It
    /// only grows the vector when the existing spare capacity is insufficient,
    /// avoiding allocator-dependent exponential growth. This mirrors
    /// [`write_to`](Self::write_to) but avoids going through the [`Write`] trait
    /// when callers simply need an owned [`Vec<u8>`] for later comparison.
    ///
    /// # Examples
    ///
    /// ```
    /// # use std::collections::TryReserveError;
    /// use rsync_transport::sniff_negotiation_stream;
    /// use std::io::Cursor;
    ///
    /// # fn demo() -> Result<(), TryReserveError> {
    /// let stream =
    ///     sniff_negotiation_stream(Cursor::new(b"@RSYNCD: 31.0\nreply".to_vec()))
    ///         .expect("sniff succeeds");
    /// let slices = stream.buffered_vectored();
    /// let mut replay = Vec::new();
    /// let appended = slices.extend_vec(&mut replay)?;
    ///
    /// assert_eq!(appended, replay.len());
    /// assert_eq!(replay, stream.buffered());
    /// # Ok(())
    /// # }
    /// # demo().unwrap();
    /// ```
    #[must_use = "the returned length reports how many bytes were appended"]
    pub fn extend_vec(&self, buffer: &mut Vec<u8>) -> Result<usize, TryReserveError> {
        if self.is_empty() {
            return Ok(0);
        }

        let additional = self.total_len;
        let spare = buffer.capacity().saturating_sub(buffer.len());
        if spare < additional {
            buffer.try_reserve_exact(additional - spare)?;
        }

        for slice in self.as_slices() {
            buffer.extend_from_slice(slice.as_ref());
        }
        Ok(additional)
    }

    /// Collects the buffered negotiation bytes into a freshly allocated [`Vec<u8>`].
    ///
    /// The helper mirrors [`Self::extend_vec`] but manages the allocation internally,
    /// returning the replay prefix and payload as an owned buffer. It pre-reserves
    /// the exact byte count required for the transcript, keeping allocations
    /// deterministic while avoiding exponential growth strategies. This keeps call
    /// sites concise when they only need the buffered transcript for comparison or
    /// logging. Allocation failures propagate as [`TryReserveError`], matching the
    /// behaviour of [`Self::extend_vec`].
    ///
    /// # Examples
    ///
    /// ```
    /// use rsync_transport::sniff_negotiation_stream;
    /// use std::io::Cursor;
    ///
    /// let stream =
    ///     sniff_negotiation_stream(Cursor::new(b"@RSYNCD: 31.0\nreply".to_vec()))
    ///         .expect("sniff succeeds");
    /// let slices = stream.buffered_vectored();
    /// let replay = slices.to_vec()?;
    ///
    /// assert_eq!(replay, stream.buffered());
    /// # Ok::<(), std::collections::TryReserveError>(())
    /// ```
    #[must_use = "the returned vector owns the buffered negotiation transcript"]
    pub fn to_vec(&self) -> Result<Vec<u8>, TryReserveError> {
        let mut buffer = Vec::new();

        if self.total_len != 0 {
            buffer.try_reserve_exact(self.total_len)?;
        }

        let _ = self.extend_vec(&mut buffer)?;
        Ok(buffer)
    }

    /// Copies the buffered bytes into the provided destination slice.
    ///
    /// The helper mirrors [`Self::extend_vec`] but writes directly into an existing
    /// buffer. When `dest` does not provide enough capacity for the replay
    /// prefix and payload, the method returns a [`CopyToSliceError`] describing
    /// the required length. Callers can resize their storage and retry. On
    /// success the number of bytes written equals the total buffered length and
    /// any remaining capacity in `dest` is left untouched.
    ///
    /// # Examples
    ///
    /// ```
    /// use rsync_transport::sniff_negotiation_stream;
    /// use std::io::Cursor;
    ///
    /// let stream =
    ///     sniff_negotiation_stream(Cursor::new(b"@RSYNCD: 31.0\nreply".to_vec()))
    ///         .expect("sniff succeeds");
    /// let slices = stream.buffered_vectored();
    /// let mut buffer = [0u8; 32];
    /// let copied = slices.copy_to_slice(&mut buffer)?;
    ///
    /// assert_eq!(copied, stream.buffered().len());
    /// assert_eq!(&buffer[..copied], &stream.buffered()[..]);
    /// # Ok::<(), rsync_transport::CopyToSliceError>(())
    /// ```
    #[must_use = "inspect the result to discover how many bytes were copied"]
    pub fn copy_to_slice(&self, dest: &mut [u8]) -> Result<usize, CopyToSliceError> {
        if self.is_empty() {
            return Ok(0);
        }

        let required = self.total_len;
        if dest.len() < required {
            return Err(CopyToSliceError::new(required, dest.len()));
        }

        let mut offset = 0usize;
        for slice in self.as_slices() {
            let bytes = slice.as_ref();
            let end = offset + bytes.len();
            dest[offset..end].copy_from_slice(bytes);
            offset = end;
        }

        Ok(required)
    }

    /// Streams the buffered negotiation data into the provided writer.
    ///
    /// The helper prefers vectored I/O when the writer advertises support,
    /// mirroring upstream rsync's habit of emitting the replay prefix in a
    /// single `writev` call. Interruptions are transparently retried. When
    /// vectored writes are unsupported or only partially flush the buffered
    /// data, the method falls back to sequential [`Write::write_all`]
    /// operations to ensure the entire transcript is forwarded without
    /// duplication.
    ///
    /// # Examples
    ///
    /// ```
    /// use rsync_transport::sniff_negotiation_stream;
    /// use std::io::Cursor;
    ///
    /// let mut stream =
    ///     sniff_negotiation_stream(Cursor::new(b"@RSYNCD: 31.0\nreply".to_vec()))
    ///         .expect("sniff succeeds");
    /// let slices = stream.buffered_vectored();
    ///
    /// let mut replay = Vec::new();
    /// slices.write_to(&mut replay).expect("write succeeds");
    ///
    /// assert_eq!(replay, stream.buffered());
    /// ```
    #[must_use = "ignoring the result would drop I/O errors emitted while replaying the transcript"]
    pub fn write_to<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        let slices = self.as_slices();

        if slices.is_empty() {
            return Ok(());
        }

        if self.count == 1 {
            writer.write_all(slices[0].as_ref())?;
            return Ok(());
        }

        let mut remaining = self.total_len;
        let mut vectored = self.segments;
        let mut segments: &mut [IoSlice<'_>] = &mut vectored[..self.count];

        while !segments.is_empty() && remaining > 0 {
            match writer.write_vectored(&*segments) {
                Ok(0) => return Err(io::Error::from(io::ErrorKind::WriteZero)),
                Ok(written) => {
                    debug_assert!(written <= remaining);
                    remaining -= written;

                    if remaining == 0 {
                        return Ok(());
                    }

                    IoSlice::advance_slices(&mut segments, written);
                }
                Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                Err(err) if err.kind() == io::ErrorKind::Unsupported => break,
                Err(err) => return Err(err),
            }
        }

        if remaining == 0 {
            return Ok(());
        }

        let mut consumed = self.total_len - remaining;

        for slice in slices {
            let bytes = slice.as_ref();

            if consumed >= bytes.len() {
                consumed -= bytes.len();
                continue;
            }

            if consumed > 0 {
                writer.write_all(&bytes[consumed..])?;
                consumed = 0;
            } else {
                writer.write_all(bytes)?;
            }
        }

        Ok(())
    }
}

impl<'a> AsRef<[IoSlice<'a>]> for NegotiationBufferedSlices<'a> {
    fn as_ref(&self) -> &[IoSlice<'a>] {
        self.as_slices()
    }
}

impl<'a> IntoIterator for &'a NegotiationBufferedSlices<'a> {
    type Item = &'a IoSlice<'a>;
    type IntoIter = slice::Iter<'a, IoSlice<'a>>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl<'a> IntoIterator for NegotiationBufferedSlices<'a> {
    type Item = IoSlice<'a>;
    type IntoIter = std::iter::Take<std::array::IntoIter<IoSlice<'a>, 2>>;

    fn into_iter(self) -> Self::IntoIter {
        self.segments.into_iter().take(self.count)
    }
}

/// Error returned when `NegotiationBufferedSlices::copy_to_slice` receives an undersized buffer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CopyToSliceError {
    required: usize,
    provided: usize,
}

impl CopyToSliceError {
    const fn new(required: usize, provided: usize) -> Self {
        Self { required, provided }
    }

    /// Number of bytes required to store the buffered negotiation transcript.
    #[must_use]
    pub const fn required(self) -> usize {
        self.required
    }

    /// Number of bytes supplied by the caller.
    #[must_use]
    pub const fn provided(self) -> usize {
        self.provided
    }

    /// Returns how many additional bytes would have satisfied the copy request.
    ///
    /// The value saturates when the provided length exceeds the recorded requirement so the
    /// method remains robust even if the error is constructed with inconsistent inputs. Callers
    /// that surface diagnostics to users can therefore embed the `missing` count directly in their
    /// messages without worrying about underflow. When produced by
    /// `NegotiationBufferedSlices::copy_to_slice`, the return value matches `required - provided`,
    /// mirroring the conventions used by upstream rsync when reporting undersized scratch buffers.
    #[must_use]
    pub const fn missing(self) -> usize {
        self.required.saturating_sub(self.provided)
    }
}

impl fmt::Display for CopyToSliceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "buffer length {} is insufficient for negotiation transcript requiring {} bytes",
            self.provided, self.required
        )
    }
}

impl std::error::Error for CopyToSliceError {}

impl From<CopyToSliceError> for io::Error {
    fn from(err: CopyToSliceError) -> Self {
        io::Error::new(io::ErrorKind::InvalidInput, err)
    }
}

trait NegotiationBufferAccess {
    fn buffer_ref(&self) -> &NegotiationBuffer;

    #[inline]
    fn buffered(&self) -> &[u8] {
        self.buffer_ref().buffered()
    }

    #[inline]
    fn sniffed_prefix(&self) -> &[u8] {
        self.buffer_ref().sniffed_prefix()
    }

    #[inline]
    fn buffered_remainder(&self) -> &[u8] {
        self.buffer_ref().buffered_remainder()
    }

    #[inline]
    fn buffered_vectored(&self) -> NegotiationBufferedSlices<'_> {
        self.buffer_ref().buffered_vectored()
    }

    #[inline]
    fn buffered_to_vec(&self) -> Result<Vec<u8>, TryReserveError> {
        self.buffer_ref().buffered_to_vec()
    }

    #[inline]
    fn copy_buffered_into_vec(&self, target: &mut Vec<u8>) -> Result<usize, TryReserveError> {
        self.buffer_ref().copy_into_vec(target)
    }

    #[inline]
    fn copy_buffered_remaining_into_vec(
        &self,
        target: &mut Vec<u8>,
    ) -> Result<usize, TryReserveError> {
        self.buffer_ref().copy_remaining_into_vec(target)
    }

    #[inline]
    fn extend_buffered_into_vec(&self, target: &mut Vec<u8>) -> Result<usize, TryReserveError> {
        self.buffer_ref().extend_into_vec(target)
    }

    #[inline]
    fn extend_buffered_remaining_into_vec(
        &self,
        target: &mut Vec<u8>,
    ) -> Result<usize, TryReserveError> {
        self.buffer_ref().extend_remaining_into_vec(target)
    }

    #[inline]
    fn buffered_split(&self) -> (&[u8], &[u8]) {
        self.buffer_ref().buffered_split()
    }

    #[inline]
    fn buffered_len(&self) -> usize {
        self.buffer_ref().buffered_len()
    }

    #[inline]
    fn buffered_consumed(&self) -> usize {
        self.buffer_ref().buffered_consumed()
    }

    #[inline]
    fn buffered_consumed_slice(&self) -> &[u8] {
        self.buffer_ref().buffered_consumed_slice()
    }

    #[inline]
    fn sniffed_prefix_remaining(&self) -> usize {
        self.buffer_ref().sniffed_prefix_remaining()
    }

    #[inline]
    fn legacy_prefix_complete(&self) -> bool {
        self.buffer_ref().legacy_prefix_complete()
    }

    #[inline]
    fn buffered_remaining(&self) -> usize {
        self.buffer_ref().buffered_remaining()
    }

    #[inline]
    fn buffered_remaining_slice(&self) -> &[u8] {
        self.buffer_ref().buffered_remaining_slice()
    }

    #[inline]
    fn buffered_remaining_vectored(&self) -> NegotiationBufferedSlices<'_> {
        self.buffer_ref().buffered_remaining_vectored()
    }

    #[inline]
    fn buffered_remaining_to_vec(&self) -> Result<Vec<u8>, TryReserveError> {
        self.buffer_ref().buffered_remaining_to_vec()
    }

    #[inline]
    fn copy_buffered_into(&self, target: &mut Vec<u8>) -> Result<usize, TryReserveError> {
        self.buffer_ref().copy_into_vec(target)
    }

    #[inline]
    fn copy_buffered_into_vectored(
        &self,
        bufs: &mut [IoSliceMut<'_>],
    ) -> Result<usize, BufferedCopyTooSmall> {
        self.buffer_ref().copy_all_into_vectored(bufs)
    }

    #[inline]
    fn copy_buffered_remaining_into_vectored(
        &self,
        bufs: &mut [IoSliceMut<'_>],
    ) -> Result<usize, BufferedCopyTooSmall> {
        self.buffer_ref().copy_remaining_into_vectored(bufs)
    }

    #[inline]
    fn copy_buffered_into_slice(&self, target: &mut [u8]) -> Result<usize, BufferedCopyTooSmall> {
        self.buffer_ref().copy_all_into_slice(target)
    }

    #[inline]
    fn copy_buffered_remaining_into_slice(
        &self,
        target: &mut [u8],
    ) -> Result<usize, BufferedCopyTooSmall> {
        self.buffer_ref().copy_remaining_into_slice(target)
    }

    #[inline]
    fn copy_buffered_into_array<const N: usize>(
        &self,
        target: &mut [u8; N],
    ) -> Result<usize, BufferedCopyTooSmall> {
        self.buffer_ref().copy_all_into_array(target)
    }

    #[inline]
    fn copy_buffered_remaining_into_array<const N: usize>(
        &self,
        target: &mut [u8; N],
    ) -> Result<usize, BufferedCopyTooSmall> {
        self.buffer_ref().copy_remaining_into_array(target)
    }

    #[inline]
    fn copy_buffered_into_writer<W: Write>(&self, target: &mut W) -> io::Result<usize> {
        self.buffer_ref().copy_all_into_writer(target)
    }

    #[inline]
    fn copy_buffered_remaining_into_writer<W: Write>(&self, target: &mut W) -> io::Result<usize> {
        self.buffer_ref().copy_remaining_into_writer(target)
    }
}

pub(crate) const NEGOTIATION_PROLOGUE_UNDETERMINED_MSG: &str =
    "connection closed before rsync negotiation prologue was determined";

#[derive(Clone, Debug)]
struct NegotiationBuffer {
    sniffed_prefix_len: usize,
    buffered_pos: usize,
    buffered: Vec<u8>,
}

/// Error returned when a caller-provided buffer is too small to hold the sniffed bytes.
///
/// The structure reports how many bytes were required to copy the replay data and how many were
/// provided by the caller. It mirrors upstream rsync's approach of signalling insufficient
/// capacity without mutating the destination, allowing higher layers to retry with a suitably
/// sized buffer while keeping the captured negotiation prefix intact.
///
/// The error implements [`From`] for [`io::Error`], making it straightforward to integrate with
/// APIs that expect transport errors. The conversion marks the error as
/// [`io::ErrorKind::InvalidInput`], matching upstream rsync's diagnostics when a caller supplies a
/// buffer that cannot hold the sniffed negotiation transcript.
///
/// # Examples
///
/// Convert the error into an [`io::Error`] when a scratch buffer is too small:
///
/// ```
/// use rsync_transport::sniff_negotiation_stream;
/// use std::io::{self, Cursor};
///
/// let stream = sniff_negotiation_stream(Cursor::new(b"@RSYNCD: 31.0\nrest".to_vec()))
///     .expect("sniff succeeds");
/// let mut scratch = [0u8; 4];
/// let err = stream
///     .copy_buffered_into_slice(&mut scratch)
///     .expect_err("insufficient capacity must error");
/// let io_err: io::Error = err.into();
/// assert_eq!(io_err.kind(), io::ErrorKind::InvalidInput);
/// assert!(io_err.to_string().contains("requires"));
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BufferedCopyTooSmall {
    required: usize,
    provided: usize,
}

impl BufferedCopyTooSmall {
    const fn new(required: usize, provided: usize) -> Self {
        Self { required, provided }
    }

    /// Returns the number of bytes necessary to copy the buffered negotiation data.
    #[must_use]
    pub const fn required(self) -> usize {
        self.required
    }

    /// Returns the number of bytes made available by the caller.
    #[must_use]
    pub const fn provided(self) -> usize {
        self.provided
    }

    /// Returns how many additional bytes would have been required for the copy to succeed.
    ///
    /// The difference is calculated with saturation to guard against inconsistent inputs. When the
    /// error originates from helpers such as [`NegotiatedStream::copy_buffered_into_slice`], the
    /// return value matches `required - provided`, mirroring the missing capacity reported by
    /// upstream rsync diagnostics.
    ///
    /// # Examples
    ///
    /// ```
    /// use rsync_transport::sniff_negotiation_stream;
    /// use std::io::Cursor;
    ///
    /// let stream = sniff_negotiation_stream(Cursor::new(b"@RSYNCD: 31.0\nrest".to_vec()))
    ///     .expect("sniff succeeds");
    /// let mut scratch = [0u8; 4];
    /// let err = stream
    ///     .copy_buffered_into_slice(&mut scratch)
    ///     .expect_err("insufficient capacity must error");
    /// assert_eq!(err.missing(), stream.buffered_len() - scratch.len());
    /// ```
    #[must_use]
    pub const fn missing(self) -> usize {
        self.required.saturating_sub(self.provided)
    }
}

impl fmt::Display for BufferedCopyTooSmall {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "buffered negotiation data requires {} bytes but destination provided {}",
            self.required, self.provided
        )
    }
}

impl std::error::Error for BufferedCopyTooSmall {}

impl From<BufferedCopyTooSmall> for io::Error {
    fn from(err: BufferedCopyTooSmall) -> Self {
        io::Error::new(io::ErrorKind::InvalidInput, err)
    }
}

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
    /// The tuple mirrors the view exposed by [`NegotiationPrologueSniffer::buffered_split`],
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
        NegotiatedStreamParts {
            decision: self.decision,
            buffer: self.buffer,
            inner: self.inner,
        }
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

    fn from_raw_components(
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

    fn from_buffer(inner: R, decision: NegotiationPrologue, buffer: NegotiationBuffer) -> Self {
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
    /// replaying stream wrapper instead of a [`NegotiationPrologueSniffer`]. It expects the
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
    fn new(error: E, original: T) -> Self {
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

impl NegotiationBuffer {
    fn new(sniffed_prefix_len: usize, buffered_pos: usize, buffered: Vec<u8>) -> Self {
        let clamped_prefix_len = sniffed_prefix_len.min(buffered.len());
        let clamped_pos = buffered_pos.min(buffered.len());

        Self {
            sniffed_prefix_len: clamped_prefix_len,
            buffered_pos: clamped_pos,
            buffered,
        }
    }

    fn sniffed_prefix(&self) -> &[u8] {
        &self.buffered[..self.sniffed_prefix_len]
    }

    fn buffered_remainder(&self) -> &[u8] {
        let start = self
            .buffered_pos
            .max(self.sniffed_prefix_len())
            .min(self.buffered.len());
        &self.buffered[start..]
    }

    fn buffered(&self) -> &[u8] {
        &self.buffered
    }

    fn buffered_consumed_slice(&self) -> &[u8] {
        let consumed = self.buffered_pos.min(self.buffered.len());
        &self.buffered[..consumed]
    }

    fn buffered_vectored(&self) -> NegotiationBufferedSlices<'_> {
        let prefix = &self.buffered[..self.sniffed_prefix_len];
        let remainder = &self.buffered[self.sniffed_prefix_len..];
        NegotiationBufferedSlices::new(prefix, remainder)
    }

    fn buffered_to_vec(&self) -> Result<Vec<u8>, TryReserveError> {
        self.buffered_vectored().to_vec()
    }

    fn buffered_split(&self) -> (&[u8], &[u8]) {
        let prefix_len = self.sniffed_prefix_len();
        debug_assert!(prefix_len <= self.buffered.len());

        let consumed_prefix = self.buffered_pos.min(prefix_len);
        let prefix_start = consumed_prefix;
        let prefix_slice = &self.buffered[prefix_start..prefix_len];

        let remainder_start = self.buffered_pos.max(prefix_len).min(self.buffered.len());
        let remainder_slice = &self.buffered[remainder_start..];

        (prefix_slice, remainder_slice)
    }

    fn buffered_remaining_vectored(&self) -> NegotiationBufferedSlices<'_> {
        let (prefix, remainder) = self.buffered_split();
        NegotiationBufferedSlices::new(prefix, remainder)
    }

    fn buffered_remaining_to_vec(&self) -> Result<Vec<u8>, TryReserveError> {
        let remainder = self.buffered_remainder();
        if remainder.is_empty() {
            return Ok(Vec::new());
        }

        let mut buffer = Vec::new();
        buffer.try_reserve_exact(remainder.len())?;
        buffer.extend_from_slice(remainder);
        Ok(buffer)
    }

    const fn sniffed_prefix_len(&self) -> usize {
        self.sniffed_prefix_len
    }

    fn buffered_len(&self) -> usize {
        self.buffered.len()
    }

    fn buffered_consumed(&self) -> usize {
        self.buffered_pos
    }

    fn buffered_remaining(&self) -> usize {
        self.buffered.len().saturating_sub(self.buffered_pos)
    }

    fn sniffed_prefix_remaining(&self) -> usize {
        let consumed_prefix = self.buffered_pos.min(self.sniffed_prefix_len);
        self.sniffed_prefix_len.saturating_sub(consumed_prefix)
    }

    fn legacy_prefix_complete(&self) -> bool {
        self.sniffed_prefix_len >= LEGACY_DAEMON_PREFIX_LEN
    }

    fn has_remaining(&self) -> bool {
        self.buffered_pos < self.buffered.len()
    }

    fn remaining_slice(&self) -> &[u8] {
        &self.buffered[self.buffered_pos..]
    }

    fn buffered_remaining_slice(&self) -> &[u8] {
        self.remaining_slice()
    }

    fn copy_into(&mut self, buf: &mut [u8]) -> usize {
        if buf.is_empty() || !self.has_remaining() {
            return 0;
        }

        let available = &self.buffered[self.buffered_pos..];
        let to_copy = available.len().min(buf.len());
        buf[..to_copy].copy_from_slice(&available[..to_copy]);
        self.buffered_pos += to_copy;
        to_copy
    }

    #[inline]
    fn copy_bytes_into_vec(target: &mut Vec<u8>, bytes: &[u8]) -> Result<usize, TryReserveError> {
        let len = target.len();
        target.try_reserve(bytes.len().saturating_sub(len))?;
        target.clear();
        if bytes.is_empty() {
            return Ok(0);
        }

        target.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    #[inline]
    fn extend_bytes_into_vec(target: &mut Vec<u8>, bytes: &[u8]) -> Result<usize, TryReserveError> {
        if bytes.is_empty() {
            return Ok(0);
        }

        target.try_reserve(bytes.len())?;
        target.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn copy_into_vec(&self, target: &mut Vec<u8>) -> Result<usize, TryReserveError> {
        Self::copy_bytes_into_vec(target, &self.buffered)
    }

    fn extend_into_vec(&self, target: &mut Vec<u8>) -> Result<usize, TryReserveError> {
        Self::extend_bytes_into_vec(target, &self.buffered)
    }

    fn copy_remaining_into_vec(&self, target: &mut Vec<u8>) -> Result<usize, TryReserveError> {
        Self::copy_bytes_into_vec(target, self.remaining_slice())
    }

    fn extend_remaining_into_vec(&self, target: &mut Vec<u8>) -> Result<usize, TryReserveError> {
        Self::extend_bytes_into_vec(target, self.remaining_slice())
    }

    fn copy_all_into_slice(&self, target: &mut [u8]) -> Result<usize, BufferedCopyTooSmall> {
        let required = self.buffered.len();
        if target.len() < required {
            return Err(BufferedCopyTooSmall::new(required, target.len()));
        }

        target[..required].copy_from_slice(&self.buffered);
        Ok(required)
    }

    fn copy_remaining_into_slice(&self, target: &mut [u8]) -> Result<usize, BufferedCopyTooSmall> {
        let remaining = self.remaining_slice();
        if target.len() < remaining.len() {
            return Err(BufferedCopyTooSmall::new(remaining.len(), target.len()));
        }

        target[..remaining.len()].copy_from_slice(remaining);
        Ok(remaining.len())
    }

    fn copy_all_into_array<const N: usize>(
        &self,
        target: &mut [u8; N],
    ) -> Result<usize, BufferedCopyTooSmall> {
        self.copy_all_into_slice(target.as_mut_slice())
    }

    fn copy_remaining_into_array<const N: usize>(
        &self,
        target: &mut [u8; N],
    ) -> Result<usize, BufferedCopyTooSmall> {
        self.copy_remaining_into_slice(target.as_mut_slice())
    }

    fn copy_all_into_writer<W: Write>(&self, target: &mut W) -> io::Result<usize> {
        target.write_all(&self.buffered)?;
        Ok(self.buffered.len())
    }

    fn copy_remaining_into_writer<W: Write>(&self, target: &mut W) -> io::Result<usize> {
        let remaining = self.remaining_slice();
        target.write_all(remaining)?;
        Ok(remaining.len())
    }

    fn copy_all_into_vectored(
        &self,
        bufs: &mut [IoSliceMut<'_>],
    ) -> Result<usize, BufferedCopyTooSmall> {
        let required = self.buffered.len();
        if required == 0 {
            return Ok(0);
        }

        let mut provided = 0usize;
        for buf in bufs.iter() {
            provided = provided.saturating_add(buf.len());
            if provided >= required {
                break;
            }
        }

        if provided < required {
            return Err(BufferedCopyTooSmall::new(required, provided));
        }

        let mut written = 0usize;
        for buf in bufs.iter_mut() {
            if written == required {
                break;
            }

            let slice = buf.as_mut();
            if slice.is_empty() {
                continue;
            }

            let to_copy = (required - written).min(slice.len());
            slice[..to_copy].copy_from_slice(&self.buffered[written..written + to_copy]);
            written += to_copy;
        }

        debug_assert_eq!(written, required);
        Ok(required)
    }

    fn copy_remaining_into_vectored(
        &self,
        bufs: &mut [IoSliceMut<'_>],
    ) -> Result<usize, BufferedCopyTooSmall> {
        let remaining = self.remaining_slice();
        if remaining.is_empty() {
            return Ok(0);
        }

        let mut provided = 0usize;
        for buf in bufs.iter() {
            provided = provided.saturating_add(buf.len());
            if provided >= remaining.len() {
                break;
            }
        }

        if provided < remaining.len() {
            return Err(BufferedCopyTooSmall::new(remaining.len(), provided));
        }

        let mut written = 0usize;
        for buf in bufs.iter_mut() {
            if written == remaining.len() {
                break;
            }

            let slice = buf.as_mut();
            if slice.is_empty() {
                continue;
            }

            let to_copy = (remaining.len() - written).min(slice.len());
            slice[..to_copy].copy_from_slice(&remaining[written..written + to_copy]);
            written += to_copy;
        }

        debug_assert_eq!(written, remaining.len());
        Ok(remaining.len())
    }

    fn copy_into_vectored(&mut self, bufs: &mut [IoSliceMut<'_>]) -> usize {
        if bufs.is_empty() || !self.has_remaining() {
            return 0;
        }

        let available = &self.buffered[self.buffered_pos..];
        let mut copied = 0;

        for buf in bufs.iter_mut() {
            if copied == available.len() {
                break;
            }

            let target = buf.as_mut();
            if target.is_empty() {
                continue;
            }

            let remaining = available.len() - copied;
            let to_copy = remaining.min(target.len());
            target[..to_copy].copy_from_slice(&available[copied..copied + to_copy]);
            copied += to_copy;
        }

        self.buffered_pos += copied;
        copied
    }

    fn consume(&mut self, amt: usize) -> usize {
        if !self.has_remaining() {
            return amt;
        }

        let available = self.buffered_remaining();
        if amt < available {
            self.buffered_pos += amt;
            0
        } else {
            self.buffered_pos = self.buffered.len();
            amt - available
        }
    }

    fn into_raw_parts(self) -> (usize, usize, Vec<u8>) {
        let Self {
            sniffed_prefix_len,
            buffered_pos,
            buffered,
        } = self;
        (sniffed_prefix_len, buffered_pos, buffered)
    }
}

impl<R> NegotiatedStreamParts<R> {
    fn into_components(self) -> (NegotiationPrologue, NegotiationBuffer, R) {
        let Self {
            decision,
            buffer,
            inner,
        } = self;
        (decision, buffer, inner)
    }
}

/// Sniffs the negotiation prologue from the provided reader.
///
/// The function mirrors upstream rsync's handshake detection logic: it peeks at
/// the first byte to distinguish binary negotiations from legacy ASCII
/// `@RSYNCD:` exchanges, buffering the observed data so the caller can replay it
/// when parsing the daemon greeting or continuing the binary protocol.
///
/// # Errors
///
/// Returns [`io::ErrorKind::UnexpectedEof`] if the stream ends before a
/// negotiation style can be determined or propagates any underlying I/O error
/// reported by the reader.
#[must_use = "the returned stream holds the buffered negotiation state and must be consumed"]
pub fn sniff_negotiation_stream<R: Read>(reader: R) -> io::Result<NegotiatedStream<R>> {
    let mut sniffer = NegotiationPrologueSniffer::new();
    sniff_negotiation_stream_with_sniffer(reader, &mut sniffer)
}

/// Sniffs the negotiation prologue using a caller supplied sniffer instance.
///
/// The helper mirrors [`sniff_negotiation_stream`] but reuses the provided
/// [`NegotiationPrologueSniffer`], avoiding temporary allocations when a
/// higher layer already maintains a pool of reusable sniffers. The sniffer is
/// reset to guarantee stale state from previous sessions is discarded before
/// the new transport is observed.
#[must_use = "the returned stream holds the buffered negotiation state and must be consumed"]
pub fn sniff_negotiation_stream_with_sniffer<R: Read>(
    mut reader: R,
    sniffer: &mut NegotiationPrologueSniffer,
) -> io::Result<NegotiatedStream<R>> {
    sniffer.reset();

    let decision = sniffer.read_from(&mut reader)?;
    debug_assert_ne!(decision, NegotiationPrologue::NeedMoreData);

    let sniffed_prefix_len = sniffer.sniffed_prefix_len();
    let buffered = sniffer.take_buffered();

    debug_assert!(sniffed_prefix_len <= LEGACY_DAEMON_PREFIX_LEN);
    debug_assert!(sniffed_prefix_len <= buffered.len());

    Ok(NegotiatedStream::from_raw_components(
        reader,
        decision,
        sniffed_prefix_len,
        0,
        buffered,
    ))
}

#[derive(Debug)]
struct LegacyLineReserveError {
    inner: TryReserveError,
}

impl LegacyLineReserveError {
    fn new(inner: TryReserveError) -> Self {
        Self { inner }
    }
}

impl fmt::Display for LegacyLineReserveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "failed to reserve memory for legacy negotiation buffer: {}",
            self.inner
        )
    }
}

impl std::error::Error for LegacyLineReserveError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.inner)
    }
}

fn map_line_reserve_error_for_io(err: TryReserveError) -> io::Error {
    io::Error::new(io::ErrorKind::OutOfMemory, LegacyLineReserveError::new(err))
}

#[cfg(test)]
mod tests;
