use std::collections::TryReserveError;
use std::fmt;
use std::io::{self, IoSlice, Write as IoWrite};
use std::iter::FusedIterator;
use std::slice;

use super::{MAX_MESSAGE_SEGMENTS, OVERREPORTED_BYTES_ERROR, errors::map_message_reserve_error};

/// Collection of slices that jointly render a [`Message`](crate::message::Message).
///
/// The segments reference the message payload together with optional exit codes, source
/// locations, and role trailers. Callers obtain the structure through
/// [`Message::as_segments`](crate::message::Message::as_segments)
/// and can then stream the slices into vectored writers, aggregate statistics, or reuse the
/// layout when constructing custom buffers. `MessageSegments` implements [`AsRef`] so the
/// collected [`IoSlice`] values can be passed directly to APIs such as
/// [`write_vectored`](IoWrite::write_vectored) without requiring an intermediate allocation.
///
/// # Examples
///
/// Convert the segments into a slice suitable for [`write_vectored`](IoWrite::write_vectored).
///
/// ```
/// use rsync_core::{
///     message::{Message, MessageScratch, Role},
///     message_source,
/// };
///
/// let mut scratch = MessageScratch::new();
/// let message = Message::error(11, "error in file IO")
///     .with_role(Role::Receiver)
///     .with_source(message_source!());
/// let segments = message.as_segments(&mut scratch, false);
///
/// let slices: &[std::io::IoSlice<'_>] = segments.as_ref();
/// assert_eq!(slices.len(), segments.segment_count());
/// ```
///
/// Consume the segments to collect the rendered message into a contiguous buffer.
///
/// ```
/// use rsync_core::{
///     message::{Message, MessageScratch, Role},
///     message_source,
/// };
///
/// let mut scratch = MessageScratch::new();
/// let message = Message::error(23, "delta-transfer failure")
///     .with_role(Role::Sender)
///     .with_source(message_source!());
///
/// let segments = message.as_segments(&mut scratch, false);
/// let mut flattened = Vec::new();
/// segments.extend_vec(&mut flattened)?;
///
/// assert_eq!(flattened, message.to_bytes().unwrap());
/// # Ok::<(), std::io::Error>(())
/// ```
#[derive(Clone, Debug)]
pub struct MessageSegments<'a> {
    pub(super) segments: [IoSlice<'a>; MAX_MESSAGE_SEGMENTS],
    pub(super) count: usize,
    pub(super) total_len: usize,
}

impl<'a> MessageSegments<'a> {
    /// Returns the populated slice view over the underlying [`IoSlice`] array.
    #[inline]
    #[must_use]
    pub fn as_slices(&self) -> &[IoSlice<'a>] {
        &self.segments[..self.count]
    }

    #[inline]
    fn as_slices_mut(&mut self) -> &mut [IoSlice<'a>] {
        &mut self.segments[..self.count]
    }

    /// Returns the total number of bytes covered by the message segments.
    #[inline]
    #[must_use]
    pub const fn len(&self) -> usize {
        self.total_len
    }

    /// Reports the number of populated segments.
    #[inline]
    #[must_use]
    pub const fn segment_count(&self) -> usize {
        self.count
    }

    /// Returns an iterator over the populated [`IoSlice`] values.
    ///
    /// The iterator traverses the same slices that [`Self::as_slices`] exposes, preserving their
    /// original ordering so call sites can stream the message into custom sinks without allocating
    /// intermediate buffers. This mirrors upstream rsync's behaviour where formatted messages are
    /// emitted sequentially. The iterator borrows the segments, meaning the caller must keep the
    /// [`MessageSegments`] instance alive for the duration of the iteration.
    ///
    /// # Examples
    ///
    /// Iterate over the segments to compute their cumulative length.
    ///
    /// ```
    /// use rsync_core::{
    ///     message::{Message, MessageScratch, Role},
    ///     message_source,
    /// };
    ///
    /// let message = Message::error(23, "delta-transfer failure")
    ///     .with_role(Role::Sender)
    ///     .with_source(message_source!());
    /// let mut scratch = MessageScratch::new();
    /// let segments = message.as_segments(&mut scratch, false);
    /// let total: usize = segments.iter().map(|slice| slice.len()).sum();
    ///
    /// assert_eq!(total, segments.len());
    /// ```
    #[inline]
    #[must_use = "iterate over the slices to observe the rendered message segments"]
    pub fn iter(&self) -> slice::Iter<'_, IoSlice<'a>> {
        self.as_slices().iter()
    }

    /// Returns an iterator over the byte slices referenced by each segment.
    ///
    /// This is a convenience wrapper around [`Self::iter`] that exposes the underlying
    /// `&[u8]` views directly. It is especially useful when callers need to analyse or copy
    /// the rendered bytes without interacting with [`IoSlice`] explicitly, keeping their
    /// code agnostic of vectored I/O details. The iterator preserves the original ordering
    /// and implements [`DoubleEndedIterator`], [`ExactSizeIterator`], and [`FusedIterator`]
    /// so integrations can efficiently consume the slices in either direction.
    ///
    /// # Examples
    ///
    /// ```
    /// use rsync_core::{
    ///     message::{Message, MessageScratch, Role},
    ///     message_source,
    /// };
    ///
    /// let message = Message::error(23, "delta-transfer failure")
    ///     .with_role(Role::Sender)
    ///     .with_source(message_source!());
    /// let mut scratch = MessageScratch::new();
    /// let segments = message.as_segments(&mut scratch, false);
    /// let collected: Vec<&[u8]> = segments.iter_bytes().collect();
    ///
    /// assert_eq!(collected.len(), segments.segment_count());
    /// assert_eq!(collected.concat(), message.to_bytes().unwrap());
    /// ```
    #[inline]
    #[must_use]
    pub fn iter_bytes(
        &self,
    ) -> impl ExactSizeIterator<Item = &'_ [u8]> + DoubleEndedIterator + FusedIterator + '_ {
        self.iter().map(move |slice| slice.as_ref())
    }

    /// Reports whether any slices were produced or contain bytes.
    #[inline]
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.count == 0 || self.total_len == 0
    }

    /// Returns a mutable iterator over the populated vectored slices.
    ///
    /// This mirrors [`Self::iter`] but yields mutable references so callers can
    /// adjust slice boundaries prior to issuing writes. The iterator maintains
    /// the original ordering so diagnostics remain byte-identical to upstream
    /// rsync.
    ///
    /// # Examples
    ///
    /// Iterate mutably over the slices and confirm they are all non-empty.
    ///
    /// ```
    /// use rsync_core::{
    ///     message::{Message, MessageScratch, Role},
    ///     message_source,
    /// };
    ///
    /// let mut scratch = MessageScratch::new();
    /// let message = Message::error(23, "delta-transfer failure")
    ///     .with_role(Role::Sender)
    ///     .with_source(message_source!());
    /// let mut segments = message.as_segments(&mut scratch, false);
    ///
    /// for slice in &mut segments {
    ///     let bytes: &[u8] = slice.as_ref();
    ///     assert!(!bytes.is_empty());
    /// }
    /// ```
    #[inline]
    #[must_use = "consume the iterator to mutate the in-place segment descriptors"]
    pub fn iter_mut(&mut self) -> slice::IterMut<'_, IoSlice<'a>> {
        self.as_slices_mut().iter_mut()
    }

    /// Streams the message segments into the provided writer.
    ///
    /// The helper prefers vectored writes when the message spans multiple
    /// segments so downstream sinks receive the payload in a single
    /// [`write_vectored`](IoWrite::write_vectored) call. Leading empty slices are
    /// trimmed before issuing the first vectored write to avoid writers
    /// reporting a spurious [`io::ErrorKind::WriteZero`] even though payload
    /// bytes remain. When the writer reports that vectored I/O is unsupported or
    /// performs a partial write, the remaining bytes are flushed sequentially to
    /// mirror upstream rsync's formatting logic.
    ///
    /// # Examples
    ///
    /// ```
    /// use rsync_core::{
    ///     message::{Message, MessageScratch, Role},
    ///     message_source,
    /// };
    ///
    /// let mut scratch = MessageScratch::new();
    /// let message = Message::error(12, "example")
    ///     .with_role(Role::Sender)
    ///     .with_source(message_source!());
    ///
    /// let segments = message.as_segments(&mut scratch, false);
    /// let mut buffer = Vec::new();
    /// segments.write_to(&mut buffer).unwrap();
    ///
    /// assert_eq!(buffer, message.to_bytes().unwrap());
    /// ```
    #[must_use = "rsync message streaming can fail when the underlying writer reports an I/O error"]
    pub fn write_to<W: IoWrite>(&self, writer: &mut W) -> io::Result<()> {
        if self.is_empty() {
            return Ok(());
        }

        if self.count == 1 {
            let bytes: &[u8] = self.segments[0].as_ref();

            if bytes.is_empty() {
                return Ok(());
            }

            writer.write_all(bytes)?;
            return Ok(());
        }

        let borrowed = Self::trim_leading_empty_slices(&self.segments[..self.count]);
        let mut remaining = self.total_len;

        if borrowed.is_empty() {
            return Ok(());
        }

        loop {
            match writer.write_vectored(borrowed) {
                Ok(0) => {
                    return Err(io::Error::from(io::ErrorKind::WriteZero));
                }
                Ok(written) => {
                    if written > remaining {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            OVERREPORTED_BYTES_ERROR,
                        ));
                    }
                    remaining -= written;

                    if remaining == 0 {
                        return Ok(());
                    }

                    let mut storage = self.segments;
                    let mut view = Self::trim_leading_empty_slices_mut(&mut storage[..self.count]);
                    IoSlice::advance_slices(&mut view, written);
                    view = Self::trim_leading_empty_slices_mut(view);

                    return Self::write_owned_view(writer, view, remaining);
                }
                Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                Err(err) if err.kind() == io::ErrorKind::Unsupported => {
                    return Self::write_borrowed_sequential(writer, borrowed, remaining);
                }
                Err(err) => return Err(err),
            }
        }
    }

    fn write_owned_view<W: IoWrite>(
        writer: &mut W,
        mut view: &mut [IoSlice<'a>],
        mut remaining: usize,
    ) -> io::Result<()> {
        while !view.is_empty() && remaining != 0 {
            match writer.write_vectored(view) {
                Ok(0) => {
                    return Err(io::Error::from(io::ErrorKind::WriteZero));
                }
                Ok(written) => {
                    if written > remaining {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            OVERREPORTED_BYTES_ERROR,
                        ));
                    }
                    remaining -= written;

                    if remaining == 0 {
                        return Ok(());
                    }

                    IoSlice::advance_slices(&mut view, written);
                    view = Self::trim_leading_empty_slices_mut(view);
                }
                Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                Err(err) if err.kind() == io::ErrorKind::Unsupported => break,
                Err(err) => return Err(err),
            }
        }

        Self::write_borrowed_sequential(writer, view, remaining)
    }

    fn write_borrowed_sequential<W: IoWrite>(
        writer: &mut W,
        slices: &[IoSlice<'a>],
        mut remaining: usize,
    ) -> io::Result<()> {
        let view = Self::trim_leading_empty_slices(slices);

        for slice in view.iter() {
            let bytes: &[u8] = slice.as_ref();

            if bytes.is_empty() {
                continue;
            }

            if bytes.len() > remaining {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    OVERREPORTED_BYTES_ERROR,
                ));
            }

            writer.write_all(bytes)?;
            debug_assert!(bytes.len() <= remaining);
            remaining -= bytes.len();
        }

        if remaining != 0 {
            return Err(io::Error::from(io::ErrorKind::WriteZero));
        }

        Ok(())
    }

    #[inline]
    fn trim_leading_empty_slices_mut<'b>(
        mut slices: &'b mut [IoSlice<'a>],
    ) -> &'b mut [IoSlice<'a>] {
        loop {
            let Some(is_empty) = slices.first().map(|slice| {
                let bytes: &[u8] = slice.as_ref();
                bytes.is_empty()
            }) else {
                return slices;
            };

            if !is_empty {
                return slices;
            }

            let (_, rest) = slices
                .split_first_mut()
                .expect("slice is non-empty after first() check");
            slices = rest;
        }
    }

    #[inline]
    fn trim_leading_empty_slices<'b>(mut slices: &'b [IoSlice<'a>]) -> &'b [IoSlice<'a>] {
        while let Some((first, rest)) = slices.split_first() {
            let first_bytes: &[u8] = first.as_ref();
            if !first_bytes.is_empty() {
                break;
            }

            slices = rest;
        }

        slices
    }

    /// Attempts to extend the provided buffer with the rendered message bytes without mapping
    /// allocation failures.
    ///
    /// The method ensures enough capacity for the rendered message by using
    /// [`Vec::try_reserve_exact`], avoiding the exponential growth strategy of
    /// [`Vec::try_reserve`]. Once space is reserved it appends each segment via
    /// [`Vec::extend_from_slice`], eliminating the intermediate zero-fill that
    /// [`Vec::resize`] would otherwise perform. This keeps allocations tight for
    /// call sites that accumulate multiple diagnostics into a single [`Vec<u8>`]
    /// without going through the [`Write`](IoWrite) trait while ensuring no
    /// redundant memory writes occur. When allocation fails the original
    /// [`TryReserveError`] is returned to the caller, allowing higher layers to
    /// surface precise diagnostics or retry with an alternative strategy.
    ///
    /// # Examples
    ///
    /// ```
    /// # use std::collections::TryReserveError;
    /// use rsync_core::{
    ///     message::{Message, MessageScratch, Role},
    ///     message_source,
    /// };
    ///
    /// # fn demo() -> Result<(), TryReserveError> {
    /// let mut scratch = MessageScratch::new();
    /// let message = Message::error(23, "delta-transfer failure")
    ///     .with_role(Role::Sender)
    ///     .with_source(message_source!());
    ///
    /// let segments = message.as_segments(&mut scratch, false);
    /// let mut buffer = b"prefix: ".to_vec();
    /// let prefix_len = buffer.len();
    /// let appended = segments.try_extend_vec(&mut buffer)?;
    ///
    /// assert_eq!(&buffer[..prefix_len], b"prefix: ");
    /// assert_eq!(&buffer[prefix_len..], message.to_bytes().unwrap().as_slice());
    /// assert_eq!(appended, message.to_bytes().unwrap().len());
    /// # Ok(())
    /// # }
    /// # demo().unwrap();
    /// ```
    #[must_use = "buffer extension reserves memory and may fail; handle allocation errors and inspect the appended length"]
    pub fn try_extend_vec(&self, buffer: &mut Vec<u8>) -> Result<usize, TryReserveError> {
        if self.is_empty() {
            return Ok(0);
        }

        let required = self.len();
        let spare = buffer.capacity().saturating_sub(buffer.len());
        if spare < required {
            let additional = required - spare;
            buffer.try_reserve_exact(additional)?;
            debug_assert!(
                buffer.capacity().saturating_sub(buffer.len()) >= required,
                "MessageSegments::try_extend_vec must reserve enough capacity for the entire message",
            );
        }

        let start = buffer.len();
        for slice in self.iter() {
            let bytes: &[u8] = slice.as_ref();
            if bytes.is_empty() {
                continue;
            }

            buffer.extend_from_slice(bytes);
        }

        debug_assert_eq!(buffer.len() - start, required);
        Ok(required)
    }

    /// Extends the provided buffer with the rendered message bytes.
    ///
    /// This convenience wrapper maps the [`TryReserveError`] returned by
    /// [`Self::try_extend_vec`] into an [`io::Error`] so callers that already
    /// operate in I/O contexts do not need to handle allocation failures
    /// explicitly.
    #[must_use = "buffer extension reserves memory and may fail; handle allocation errors and inspect the appended length"]
    pub fn extend_vec(&self, buffer: &mut Vec<u8>) -> io::Result<usize> {
        self.try_extend_vec(buffer)
            .map_err(map_message_reserve_error)
    }

    /// Copies the rendered message into the provided slice.
    ///
    /// The destination slice must be at least [`Self::len`] bytes long. When the capacity is
    /// insufficient the method returns [`CopyToSliceError`] describing the required length so callers
    /// can retry with a suitably sized buffer. This mirrors upstream rsync's approach of reusing
    /// stack-allocated buffers for message rendering while preserving deterministic allocation
    /// patterns.
    ///
    /// # Examples
    ///
    /// ```
    /// use rsync_core::{
    ///     message::{Message, MessageScratch, Role},
    ///     message_source,
    /// };
    ///
    /// let message = Message::error(23, "delta-transfer failure")
    ///     .with_role(Role::Sender)
    ///     .with_source(message_source!());
    /// let mut scratch = MessageScratch::new();
    /// let segments = message.as_segments(&mut scratch, false);
    ///
    /// let mut buffer = vec![0u8; segments.len()];
    /// let copied = segments.copy_to_slice(&mut buffer)?;
    ///
    /// assert_eq!(copied, segments.len());
    /// assert_eq!(buffer, message.to_bytes().unwrap());
    /// # Ok::<(), rsync_core::message::CopyToSliceError>(())
    /// ```
    #[must_use = "callers should handle the number of copied bytes or the returned error"]
    pub fn copy_to_slice(&self, dest: &mut [u8]) -> Result<usize, CopyToSliceError> {
        let required = self.len();
        if dest.len() < required {
            return Err(CopyToSliceError::new(required, dest.len()));
        }

        let mut offset = 0usize;
        for slice in self.iter() {
            let bytes: &[u8] = slice.as_ref();
            let end = offset + bytes.len();
            dest[offset..end].copy_from_slice(bytes);
            offset = end;
        }
        Ok(required)
    }

    /// Collects the message segments into a freshly allocated [`Vec<u8>`].
    ///
    /// The helper mirrors [`Self::extend_vec`] but manages the buffer lifecycle
    /// internally, returning the rendered bytes directly. This keeps call sites
    /// concise when they only need an owned copy of the message without
    /// providing scratch storage up front. Allocation failures propagate as
    /// [`io::ErrorKind::OutOfMemory`] so diagnostics remain actionable.
    ///
    /// # Examples
    ///
    /// ```
    /// use rsync_core::{
    ///     message::{Message, MessageScratch, Role},
    ///     message_source,
    /// };
    ///
    /// let mut scratch = MessageScratch::new();
    /// let message = Message::error(23, "delta-transfer failure")
    ///     .with_role(Role::Sender)
    ///     .with_source(message_source!());
    ///
    /// let segments = message.as_segments(&mut scratch, false);
    /// let collected = segments.to_vec().expect("allocation succeeds");
    ///
    /// assert_eq!(collected, message.to_bytes().unwrap());
    /// ```
    #[must_use = "collecting message segments allocates and can fail if memory reservations are unsuccessful"]
    pub fn to_vec(&self) -> io::Result<Vec<u8>> {
        let mut buffer = Vec::new();
        let _ = self.extend_vec(&mut buffer)?;
        Ok(buffer)
    }
}

impl<'a> AsRef<[IoSlice<'a>]> for MessageSegments<'a> {
    #[inline]
    fn as_ref(&self) -> &[IoSlice<'a>] {
        self.as_slices()
    }
}

impl<'a> IntoIterator for &'a MessageSegments<'a> {
    type Item = &'a IoSlice<'a>;
    type IntoIter = slice::Iter<'a, IoSlice<'a>>;

    #[inline]
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl<'a> IntoIterator for &'a mut MessageSegments<'a> {
    type Item = &'a mut IoSlice<'a>;
    type IntoIter = slice::IterMut<'a, IoSlice<'a>>;

    #[inline]
    fn into_iter(self) -> Self::IntoIter {
        self.iter_mut()
    }
}

impl<'a> IntoIterator for MessageSegments<'a> {
    type Item = IoSlice<'a>;
    type IntoIter = std::iter::Take<std::array::IntoIter<IoSlice<'a>, MAX_MESSAGE_SEGMENTS>>;

    #[inline]
    fn into_iter(self) -> Self::IntoIter {
        self.segments.into_iter().take(self.count)
    }
}

/// Error returned when [`MessageSegments::copy_to_slice`] receives an undersized buffer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CopyToSliceError {
    required: usize,
    provided: usize,
}

impl CopyToSliceError {
    pub(super) const fn new(required: usize, provided: usize) -> Self {
        Self { required, provided }
    }

    /// Number of bytes required to hold the rendered message.
    #[must_use]
    pub const fn required(self) -> usize {
        self.required
    }

    /// Number of bytes supplied by the caller.
    #[must_use]
    pub const fn provided(self) -> usize {
        self.provided
    }

    /// Returns how many additional bytes were necessary to satisfy the copy operation.
    ///
    /// The calculation uses saturating subtraction so that callers can report the missing capacity
    /// directly in diagnostics even if the error was created with inconsistent inputs. When the
    /// error originates from [`MessageSegments::copy_to_slice`], the result matches
    /// `required - provided`, mirroring upstream rsync's messaging when a scratch buffer is too
    /// small to hold the fully rendered diagnostic.
    #[must_use]
    pub const fn missing(self) -> usize {
        self.required.saturating_sub(self.provided)
    }
}

impl fmt::Display for CopyToSliceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "buffer length {} is insufficient for message requiring {} bytes",
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
