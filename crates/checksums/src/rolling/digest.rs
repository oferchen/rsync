use std::io::{self, Read, Write};

use super::checksum::RollingChecksum;
use super::error::RollingSliceError;

/// Digest produced by the rolling checksum.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RollingDigest {
    s1: u16,
    s2: u16,
    len: usize,
}

impl RollingDigest {
    /// Digest representing an empty window.
    ///
    /// Upstream rsync initialises rolling checksum state to zeroes before any
    /// data is observed. Exposing a constant avoids repetitive
    /// `RollingDigest::new(0, 0, 0)` expressions while ensuring callers reuse
    /// the canonical empty digest.
    pub const ZERO: Self = Self::new(0, 0, 0);

    /// Computes the digest for the provided byte slice.
    ///
    /// The helper instantiates a fresh [`RollingChecksum`], feeds the supplied
    /// bytes through it, and returns the resulting digest. This mirrors
    /// upstream rsync's habit of recalculating the weak checksum for individual
    /// blocks when constructing the sender's file list. Using this convenience
    /// avoids plumbing a mutable [`RollingChecksum`] through call sites that
    /// only need a one-off digest.
    ///
    /// # Examples
    ///
    /// ```
    /// use checksums::RollingDigest;
    ///
    /// let digest = RollingDigest::from_bytes(b"delta block");
    /// let manual = {
    ///     let mut checksum = checksums::RollingChecksum::new();
    ///     checksum.update(b"delta block");
    ///     checksum.digest()
    /// };
    ///
    /// assert_eq!(digest, manual);
    /// assert_eq!(digest.len(), b"delta block".len());
    /// ```
    #[must_use]
    pub fn from_bytes(bytes: &[u8]) -> Self {
        let mut checksum = RollingChecksum::new();
        checksum.update(bytes);
        checksum.digest()
    }

    /// Computes a digest by streaming bytes from the provided reader using the caller's buffer.
    ///
    /// This mirrors [`RollingChecksum::update_reader_with_buffer`] but returns the resulting
    /// [`RollingDigest`] directly. The buffer must be non-empty; otherwise an
    /// [`io::ErrorKind::InvalidInput`] error is returned. The `len` reported by
    /// the digest matches the total number of bytes consumed from `reader`
    /// (clamped to [`usize::MAX`] just like [`RollingChecksum`]).
    ///
    /// # Errors
    ///
    /// Propagates any [`io::Error`] emitted by the reader or reported for an empty buffer.
    ///
    /// # Examples
    ///
    /// ```
    /// use checksums::RollingDigest;
    /// use std::io::Cursor;
    ///
    /// let mut reader = Cursor::new(b"streamed input".to_vec());
    /// let mut scratch = [0u8; 8];
    /// let digest = RollingDigest::from_reader_with_buffer(&mut reader, &mut scratch)
    ///     .expect("reader succeeds");
    ///
    /// assert_eq!(digest.len(), b"streamed input".len());
    /// ```
    pub fn from_reader_with_buffer<R: Read>(reader: &mut R, buffer: &mut [u8]) -> io::Result<Self> {
        let mut checksum = RollingChecksum::new();
        let read = checksum.update_reader_with_buffer(reader, buffer)?;
        debug_assert_eq!(
            checksum.len(),
            usize::try_from(read).unwrap_or(usize::MAX),
            "rolling checksum length should mirror bytes read (modulo usize saturation)",
        );
        Ok(checksum.digest())
    }

    /// Computes a digest by streaming bytes from the reader using an internal stack buffer.
    ///
    /// This convenience wrapper allocates a
    /// [`RollingChecksum::DEFAULT_READER_BUFFER_LEN`] scratch buffer on the
    /// stack and delegates to [`from_reader_with_buffer`](Self::from_reader_with_buffer).
    /// It is ideal for tests and simple call sites that do not manage their own
    /// workspace for streaming checksums.
    ///
    /// # Errors
    ///
    /// Propagates any [`io::Error`] emitted by the reader.
    pub fn from_reader<R: Read>(reader: &mut R) -> io::Result<Self> {
        let mut checksum = RollingChecksum::new();
        checksum.update_reader(reader)?;
        Ok(checksum.digest())
    }

    /// Creates a digest from individual components.
    #[must_use]
    pub const fn new(sum1: u16, sum2: u16, len: usize) -> Self {
        Self {
            s1: sum1,
            s2: sum2,
            len,
        }
    }

    /// Constructs a digest from the packed 32-bit representation used on the wire.
    ///
    /// Upstream rsync transmits the rolling checksum as two 16-bit components (`s1`
    /// and `s2`) packed into a 32-bit integer. Higher layers often know the block
    /// length separately, so the caller provides it explicitly to avoid guessing.
    /// The helper mirrors [`Self::value`], making it cheap to round-trip digests
    /// through their network encoding without manually extracting bit fields.
    #[must_use]
    pub const fn from_value(value: u32, len: usize) -> Self {
        Self {
            s1: value as u16,
            s2: (value >> 16) as u16,
            len,
        }
    }

    /// Constructs a digest from the little-endian byte representation used by upstream rsync.
    ///
    /// Upstream serialises the rolling checksum with the `SIVAL` macro, which stores the packed
    /// value as little-endian bytes on the wire. Parsing the checksum therefore requires decoding
    /// the payload in the same order before recovering the logical components. This helper mirrors
    /// [`Self::from_value`] while avoiding an intermediate [`u32`].
    ///
    /// # Errors
    ///
    /// Returns [`RollingSliceError`] when the slice does not contain exactly four bytes.
    pub fn from_le_slice(bytes: &[u8], len: usize) -> Result<Self, RollingSliceError> {
        if bytes.len() != RollingSliceError::EXPECTED_LEN {
            return Err(RollingSliceError::new(bytes.len()));
        }

        let array = <[u8; RollingSliceError::EXPECTED_LEN]>::try_from(bytes)
            .expect("length verified above");
        Ok(Self::from_le_bytes(array, len))
    }

    /// Constructs a digest from the little-endian byte array used on the wire.
    #[must_use]
    pub const fn from_le_bytes(bytes: [u8; 4], len: usize) -> Self {
        let value = u32::from_le_bytes(bytes);
        Self::from_value(value, len)
    }

    /// Serialises the digest using the little-endian wire format.
    #[must_use]
    pub const fn to_le_bytes(self) -> [u8; 4] {
        self.value().to_le_bytes()
    }

    /// Returns the packed 32-bit representation of the digest.
    #[must_use]
    pub const fn value(self) -> u32 {
        ((self.s2 as u32) << 16) | (self.s1 as u32)
    }

    /// Length of the data that contributed to the digest.
    #[must_use]
    pub const fn len(self) -> usize {
        self.len
    }

    /// Returns whether the digest was computed from zero bytes.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.len == 0
    }

    /// First 16-bit component of the rolling checksum (`s1`).
    #[must_use]
    pub const fn sum1(self) -> u16 {
        self.s1
    }

    /// Second 16-bit component of the rolling checksum (`s2`).
    #[must_use]
    pub const fn sum2(self) -> u16 {
        self.s2
    }

    /// Writes the digest into the provided slice using the little-endian wire format.
    ///
    /// This is useful when callers need to store the digest directly into a
    /// fixed-size buffer (for example when writing directly into a network packet or an
    /// mmap-backed region). The output matches [`Self::to_le_bytes`], ensuring the result can be
    /// transmitted verbatim to peers expecting the packed `sum1`/`sum2` representation.
    ///
    /// # Errors
    ///
    /// Returns [`RollingSliceError`] when `out` does not contain exactly four bytes. The buffer is
    /// left untouched on error.
    pub fn write_le_bytes(&self, out: &mut [u8]) -> Result<(), RollingSliceError> {
        if out.len() != RollingSliceError::EXPECTED_LEN {
            return Err(RollingSliceError::new(out.len()));
        }

        out.copy_from_slice(&self.to_le_bytes());
        Ok(())
    }

    /// Reads a rolling checksum digest from the provided reader using the little-endian wire format.
    ///
    /// The helper mirrors upstream rsync's expectation that digests appear as four little-endian
    /// bytes. It blocks until the bytes have been read in full or the reader returns an error. The
    /// caller supplies the number of bytes that contributed to the checksum so the resulting
    /// [`RollingDigest`] matches the original metadata transmitted alongside the digest.
    ///
    /// # Errors
    ///
    /// Propagates any [`io::Error`] raised by the reader. Short reads surface as
    /// [`io::ErrorKind::UnexpectedEof`], matching upstream diagnostics for truncated payloads.
    pub fn read_le_from<R: Read>(reader: &mut R, len: usize) -> io::Result<Self> {
        let mut bytes = [0u8; RollingSliceError::EXPECTED_LEN];
        reader.read_exact(&mut bytes)?;
        Ok(Self::from_le_bytes(bytes, len))
    }

    /// Writes the rolling checksum digest to the provided writer using the little-endian wire format.
    ///
    /// The output matches [`Self::to_le_bytes`], making the helper convenient for serialising
    /// digests directly into network buffers without allocating intermediate arrays.
    ///
    /// # Errors
    ///
    /// Propagates any [`io::Error`] reported by the writer.
    pub fn write_le_to<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        writer.write_all(&self.to_le_bytes())
    }
}

impl From<RollingDigest> for u32 {
    #[inline]
    fn from(digest: RollingDigest) -> Self {
        digest.value()
    }
}

impl From<&RollingDigest> for u32 {
    #[inline]
    fn from(digest: &RollingDigest) -> Self {
        digest.value()
    }
}

impl From<RollingDigest> for [u8; 4] {
    #[inline]
    fn from(digest: RollingDigest) -> Self {
        digest.to_le_bytes()
    }
}

impl From<&RollingDigest> for [u8; 4] {
    #[inline]
    fn from(digest: &RollingDigest) -> Self {
        digest.to_le_bytes()
    }
}

impl Default for RollingDigest {
    fn default() -> Self {
        Self::ZERO
    }
}
