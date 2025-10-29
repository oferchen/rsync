use std::collections::TryReserveError;
use std::io::{self, IoSlice, IoSliceMut, Write};
use std::slice;

/// Vectored view over buffered negotiation data.
///
/// The structure exposes up to two [`IoSlice`] segments: the remaining portion of the
/// canonical legacy prefix (`@RSYNCD:`) and any buffered payload that followed the prologue.
/// Consumers obtain instances via [`NegotiationBufferAccess::buffered_vectored`],
/// [`NegotiationBufferAccess::buffered_remaining_vectored`], or their counterparts on
/// [`NegotiatedStream`](super::NegotiatedStream) and [`NegotiatedStreamParts`](super::NegotiatedStreamParts).
/// The iterator interface allows the slices to be passed directly to
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
    pub(crate) fn new(prefix: &'a [u8], remainder: &'a [u8]) -> Self {
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
    pub(crate) const fn new(required: usize, provided: usize) -> Self {
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

impl std::fmt::Display for CopyToSliceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
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

/// Shared accessors for buffered negotiation data.
pub(crate) trait NegotiationBufferAccess {
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
    pub(crate) const fn new(required: usize, provided: usize) -> Self {
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
    /// error originates from helpers such as [`super::NegotiatedStream::copy_buffered_into_slice`], the
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

impl std::fmt::Display for BufferedCopyTooSmall {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
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

#[derive(Clone, Debug)]
pub(crate) struct NegotiationBuffer {
    sniffed_prefix_len: usize,
    buffered_pos: usize,
    buffered: Vec<u8>,
}

impl NegotiationBuffer {
    pub(crate) fn new(sniffed_prefix_len: usize, buffered_pos: usize, buffered: Vec<u8>) -> Self {
        let clamped_prefix_len = sniffed_prefix_len.min(buffered.len());
        let clamped_pos = buffered_pos.min(buffered.len());

        Self {
            sniffed_prefix_len: clamped_prefix_len,
            buffered_pos: clamped_pos,
            buffered,
        }
    }

    pub(crate) fn sniffed_prefix(&self) -> &[u8] {
        &self.buffered[..self.sniffed_prefix_len]
    }

    pub(crate) fn buffered_remainder(&self) -> &[u8] {
        let start = self
            .buffered_pos
            .max(self.sniffed_prefix_len())
            .min(self.buffered.len());
        &self.buffered[start..]
    }

    pub(crate) fn buffered(&self) -> &[u8] {
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

    pub(crate) const fn sniffed_prefix_len(&self) -> usize {
        self.sniffed_prefix_len
    }

    pub(crate) fn buffered_len(&self) -> usize {
        self.buffered.len()
    }

    pub(crate) fn buffered_consumed(&self) -> usize {
        self.buffered_pos
    }

    pub(crate) fn buffered_remaining(&self) -> usize {
        self.buffered.len().saturating_sub(self.buffered_pos)
    }

    pub(crate) fn sniffed_prefix_remaining(&self) -> usize {
        let consumed_prefix = self.buffered_pos.min(self.sniffed_prefix_len);
        self.sniffed_prefix_len.saturating_sub(consumed_prefix)
    }

    pub(crate) fn legacy_prefix_complete(&self) -> bool {
        self.sniffed_prefix_len >= rsync_protocol::LEGACY_DAEMON_PREFIX_LEN
    }

    pub(crate) fn has_remaining(&self) -> bool {
        self.buffered_pos < self.buffered.len()
    }

    pub(crate) fn remaining_slice(&self) -> &[u8] {
        &self.buffered[self.buffered_pos..]
    }

    pub(crate) fn buffered_remaining_slice(&self) -> &[u8] {
        self.remaining_slice()
    }

    pub(crate) fn copy_into(&mut self, buf: &mut [u8]) -> usize {
        if buf.is_empty() || !self.has_remaining() {
            return 0;
        }

        let available = &self.buffered[self.buffered_pos..];
        let to_copy = available.len().min(buf.len());
        buf[..to_copy].copy_from_slice(&available[..to_copy]);
        self.buffered_pos += to_copy;
        to_copy
    }

    pub(crate) fn copy_into_vec(&self, target: &mut Vec<u8>) -> Result<usize, TryReserveError> {
        Self::copy_bytes_into_vec(target, &self.buffered)
    }

    pub(crate) fn extend_into_vec(&self, target: &mut Vec<u8>) -> Result<usize, TryReserveError> {
        Self::extend_bytes_into_vec(target, &self.buffered)
    }

    pub(crate) fn extend_remaining_into_vec(
        &self,
        target: &mut Vec<u8>,
    ) -> Result<usize, TryReserveError> {
        Self::extend_bytes_into_vec(target, self.remaining_slice())
    }

    pub(crate) fn copy_remaining_into_vec(
        &self,
        target: &mut Vec<u8>,
    ) -> Result<usize, TryReserveError> {
        Self::copy_bytes_into_vec(target, self.remaining_slice())
    }

    pub(crate) fn copy_all_into_slice(
        &self,
        target: &mut [u8],
    ) -> Result<usize, BufferedCopyTooSmall> {
        let required = self.buffered.len();

        if target.len() < required {
            return Err(BufferedCopyTooSmall::new(required, target.len()));
        }

        target[..required].copy_from_slice(&self.buffered);
        Ok(required)
    }

    pub(crate) fn copy_remaining_into_slice(
        &self,
        target: &mut [u8],
    ) -> Result<usize, BufferedCopyTooSmall> {
        let remaining = self.remaining_slice();
        if target.len() < remaining.len() {
            return Err(BufferedCopyTooSmall::new(remaining.len(), target.len()));
        }

        target[..remaining.len()].copy_from_slice(remaining);
        Ok(remaining.len())
    }

    pub(crate) fn copy_all_into_array<const N: usize>(
        &self,
        target: &mut [u8; N],
    ) -> Result<usize, BufferedCopyTooSmall> {
        self.copy_all_into_slice(target.as_mut_slice())
    }

    pub(crate) fn copy_remaining_into_array<const N: usize>(
        &self,
        target: &mut [u8; N],
    ) -> Result<usize, BufferedCopyTooSmall> {
        self.copy_remaining_into_slice(target.as_mut_slice())
    }

    pub(crate) fn copy_all_into_writer<W: Write>(&self, target: &mut W) -> io::Result<usize> {
        target.write_all(&self.buffered)?;
        Ok(self.buffered.len())
    }

    pub(crate) fn copy_remaining_into_writer<W: Write>(&self, target: &mut W) -> io::Result<usize> {
        let remaining = self.remaining_slice();
        if remaining.is_empty() {
            return Ok(0);
        }

        target.write_all(remaining)?;
        Ok(remaining.len())
    }

    pub(crate) fn copy_remaining_into_vectored(
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

    pub(crate) fn copy_all_into_vectored(
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

    fn extend_bytes_into_vec(target: &mut Vec<u8>, bytes: &[u8]) -> Result<usize, TryReserveError> {
        if bytes.is_empty() {
            return Ok(0);
        }

        target.try_reserve(bytes.len())?;
        target.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    pub(crate) fn copy_into_vectored(&mut self, bufs: &mut [IoSliceMut<'_>]) -> usize {
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

    pub(crate) fn consume(&mut self, amt: usize) -> usize {
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

    pub(crate) fn into_raw_parts(self) -> (usize, usize, Vec<u8>) {
        let Self {
            sniffed_prefix_len,
            buffered_pos,
            buffered,
        } = self;
        (sniffed_prefix_len, buffered_pos, buffered)
    }
}
