use std::collections::TryReserveError;
use std::io::{self, IoSlice, Write};
use std::slice;

use super::errors::CopyToSliceError;

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
