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
/// use rsync_io::sniff_negotiation_stream;
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
    /// use rsync_io::sniff_negotiation_stream;
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
    /// use rsync_io::sniff_negotiation_stream;
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
    /// use rsync_io::sniff_negotiation_stream;
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
    /// # Ok::<(), rsync_io::CopyToSliceError>(())
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
    /// use rsync_io::sniff_negotiation_stream;
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

#[cfg(test)]
mod tests {
    use super::*;

    // ==== Construction ====

    #[test]
    fn new_with_both_slices() {
        let prefix = b"@RSYNCD:";
        let remainder = b" 31.0\n";
        let slices = NegotiationBufferedSlices::new(prefix, remainder);
        assert_eq!(slices.segment_count(), 2);
        assert_eq!(slices.len(), prefix.len() + remainder.len());
    }

    #[test]
    fn new_with_only_prefix() {
        let prefix = b"@RSYNCD:";
        let slices = NegotiationBufferedSlices::new(prefix, &[]);
        assert_eq!(slices.segment_count(), 1);
        assert_eq!(slices.len(), prefix.len());
    }

    #[test]
    fn new_with_only_remainder() {
        let remainder = b"some data";
        let slices = NegotiationBufferedSlices::new(&[], remainder);
        assert_eq!(slices.segment_count(), 1);
        assert_eq!(slices.len(), remainder.len());
    }

    #[test]
    fn new_with_empty_slices() {
        let slices = NegotiationBufferedSlices::new(&[], &[]);
        assert_eq!(slices.segment_count(), 0);
        assert_eq!(slices.len(), 0);
        assert!(slices.is_empty());
    }

    // ==== Accessors ====

    #[test]
    fn as_slices_returns_populated_segments() {
        let prefix = b"pre";
        let remainder = b"rest";
        let slices = NegotiationBufferedSlices::new(prefix, remainder);
        let io_slices = slices.as_slices();
        assert_eq!(io_slices.len(), 2);
        assert_eq!(io_slices[0].as_ref(), prefix.as_slice());
        assert_eq!(io_slices[1].as_ref(), remainder.as_slice());
    }

    #[test]
    fn len_returns_total_bytes() {
        let prefix = b"hello";
        let remainder = b"world";
        let slices = NegotiationBufferedSlices::new(prefix, remainder);
        assert_eq!(slices.len(), 10);
    }

    #[test]
    fn is_empty_false_when_has_data() {
        let slices = NegotiationBufferedSlices::new(b"x", &[]);
        assert!(!slices.is_empty());
    }

    #[test]
    fn is_empty_true_when_no_data() {
        let slices = NegotiationBufferedSlices::new(&[], &[]);
        assert!(slices.is_empty());
    }

    #[test]
    fn segment_count_reflects_populated_segments() {
        let slices1 = NegotiationBufferedSlices::new(b"a", b"b");
        assert_eq!(slices1.segment_count(), 2);

        let slices2 = NegotiationBufferedSlices::new(b"a", &[]);
        assert_eq!(slices2.segment_count(), 1);

        let slices3 = NegotiationBufferedSlices::new(&[], &[]);
        assert_eq!(slices3.segment_count(), 0);
    }

    // ==== Iterator ====

    #[test]
    fn iter_yields_all_segments() {
        let prefix = b"abc";
        let remainder = b"def";
        let slices = NegotiationBufferedSlices::new(prefix, remainder);
        let collected: Vec<&[u8]> = slices.iter().map(|s| s.as_ref()).collect();
        assert_eq!(collected, vec![prefix.as_slice(), remainder.as_slice()]);
    }

    #[test]
    fn into_iter_by_ref_yields_all_segments() {
        let prefix = b"123";
        let remainder = b"456";
        let slices = NegotiationBufferedSlices::new(prefix, remainder);
        let mut count = 0;
        for slice in &slices {
            count += slice.len();
        }
        assert_eq!(count, 6);
    }

    #[test]
    fn into_iter_by_value_yields_all_segments() {
        let prefix = b"ab";
        let remainder = b"cd";
        let slices = NegotiationBufferedSlices::new(prefix, remainder);
        let collected: Vec<Vec<u8>> = slices.into_iter().map(|s| s.to_vec()).collect();
        assert_eq!(collected, vec![b"ab".to_vec(), b"cd".to_vec()]);
    }

    // ==== extend_vec ====

    #[test]
    fn extend_vec_appends_all_data() {
        let prefix = b"hello";
        let remainder = b"world";
        let slices = NegotiationBufferedSlices::new(prefix, remainder);
        let mut buffer = Vec::new();
        let appended = slices.extend_vec(&mut buffer).unwrap();
        assert_eq!(appended, 10);
        assert_eq!(buffer, b"helloworld");
    }

    #[test]
    fn extend_vec_appends_to_existing() {
        let slices = NegotiationBufferedSlices::new(b"two", &[]);
        let mut buffer = b"one".to_vec();
        let appended = slices.extend_vec(&mut buffer).unwrap();
        assert_eq!(appended, 3);
        assert_eq!(buffer, b"onetwo");
    }

    #[test]
    fn extend_vec_returns_zero_when_empty() {
        let slices = NegotiationBufferedSlices::new(&[], &[]);
        let mut buffer = vec![1, 2, 3];
        let appended = slices.extend_vec(&mut buffer).unwrap();
        assert_eq!(appended, 0);
        assert_eq!(buffer, vec![1, 2, 3]); // unchanged
    }

    // ==== to_vec ====

    #[test]
    fn to_vec_returns_all_data() {
        let prefix = b"@RSYNCD:";
        let remainder = b" 31.0\n";
        let slices = NegotiationBufferedSlices::new(prefix, remainder);
        let vec = slices.to_vec().unwrap();
        assert_eq!(vec, b"@RSYNCD: 31.0\n");
    }

    #[test]
    fn to_vec_returns_empty_when_no_data() {
        let slices = NegotiationBufferedSlices::new(&[], &[]);
        let vec = slices.to_vec().unwrap();
        assert!(vec.is_empty());
    }

    // ==== copy_to_slice ====

    #[test]
    fn copy_to_slice_succeeds_with_sufficient_buffer() {
        let prefix = b"abc";
        let remainder = b"def";
        let slices = NegotiationBufferedSlices::new(prefix, remainder);
        let mut buffer = [0u8; 10];
        let copied = slices.copy_to_slice(&mut buffer).unwrap();
        assert_eq!(copied, 6);
        assert_eq!(&buffer[..6], b"abcdef");
    }

    #[test]
    fn copy_to_slice_fails_with_insufficient_buffer() {
        let slices = NegotiationBufferedSlices::new(b"hello", b"world");
        let mut buffer = [0u8; 5];
        let err = slices.copy_to_slice(&mut buffer).unwrap_err();
        assert_eq!(err.required(), 10);
        assert_eq!(err.provided(), 5);
        assert_eq!(err.missing(), 5);
    }

    #[test]
    fn copy_to_slice_returns_zero_when_empty() {
        let slices = NegotiationBufferedSlices::new(&[], &[]);
        let mut buffer = [0u8; 5];
        let copied = slices.copy_to_slice(&mut buffer).unwrap();
        assert_eq!(copied, 0);
    }

    // ==== write_to ====

    #[test]
    fn write_to_writes_all_data() {
        let prefix = b"prefix";
        let remainder = b"suffix";
        let slices = NegotiationBufferedSlices::new(prefix, remainder);
        let mut output = Vec::new();
        slices.write_to(&mut output).unwrap();
        assert_eq!(output, b"prefixsuffix");
    }

    #[test]
    fn write_to_succeeds_with_single_segment() {
        let slices = NegotiationBufferedSlices::new(b"single", &[]);
        let mut output = Vec::new();
        slices.write_to(&mut output).unwrap();
        assert_eq!(output, b"single");
    }

    #[test]
    fn write_to_succeeds_when_empty() {
        let slices = NegotiationBufferedSlices::new(&[], &[]);
        let mut output = Vec::new();
        slices.write_to(&mut output).unwrap();
        assert!(output.is_empty());
    }

    // ==== AsRef ====

    #[test]
    fn as_ref_returns_io_slices() {
        let slices = NegotiationBufferedSlices::new(b"a", b"b");
        let io_slices: &[IoSlice<'_>] = slices.as_ref();
        assert_eq!(io_slices.len(), 2);
    }

    // ==== Clone and Debug ====

    #[test]
    fn clone_produces_equivalent_copy() {
        let prefix = b"test";
        let remainder = b"data";
        let slices = NegotiationBufferedSlices::new(prefix, remainder);
        let cloned = slices.clone();
        assert_eq!(slices.len(), cloned.len());
        assert_eq!(slices.segment_count(), cloned.segment_count());
    }

    #[test]
    fn debug_format_contains_type_name() {
        let slices = NegotiationBufferedSlices::new(b"x", &[]);
        let debug = format!("{slices:?}");
        assert!(debug.contains("NegotiationBufferedSlices"));
    }

    // ==== Edge cases ====

    #[test]
    fn handles_large_segments() {
        let large_prefix = vec![b'a'; 1000];
        let large_remainder = vec![b'b'; 2000];
        let slices = NegotiationBufferedSlices::new(&large_prefix, &large_remainder);
        assert_eq!(slices.len(), 3000);
        let vec = slices.to_vec().unwrap();
        assert_eq!(vec.len(), 3000);
    }

    #[test]
    fn iter_empty_yields_nothing() {
        let slices = NegotiationBufferedSlices::new(&[], &[]);
        let count = slices.iter().count();
        assert_eq!(count, 0);
    }
}
