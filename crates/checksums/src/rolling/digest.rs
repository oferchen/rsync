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
    #[inline]
    #[must_use]
    pub const fn value(self) -> u32 {
        ((self.s2 as u32) << 16) | (self.s1 as u32)
    }

    /// Length of the data that contributed to the digest.
    #[inline]
    #[must_use]
    pub const fn len(self) -> usize {
        self.len
    }

    /// Returns whether the digest was computed from zero bytes.
    #[inline]
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.len == 0
    }

    /// First 16-bit component of the rolling checksum (`s1`).
    #[inline]
    #[must_use]
    pub const fn sum1(self) -> u16 {
        self.s1
    }

    /// Second 16-bit component of the rolling checksum (`s2`).
    #[inline]
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
    pub const fn write_le_bytes(&self, out: &mut [u8]) -> Result<(), RollingSliceError> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn rolling_digest_zero_constant() {
        let zero = RollingDigest::ZERO;
        assert_eq!(zero.sum1(), 0);
        assert_eq!(zero.sum2(), 0);
        assert_eq!(zero.len(), 0);
        assert!(zero.is_empty());
    }

    #[test]
    fn rolling_digest_new_stores_components() {
        let digest = RollingDigest::new(0x1234, 0x5678, 100);
        assert_eq!(digest.sum1(), 0x1234);
        assert_eq!(digest.sum2(), 0x5678);
        assert_eq!(digest.len(), 100);
    }

    #[test]
    fn rolling_digest_from_bytes_computes_checksum() {
        let data = b"hello world";
        let digest = RollingDigest::from_bytes(data);
        assert_eq!(digest.len(), data.len());
        assert!(!digest.is_empty());
        // Verify it matches manual computation
        let mut checksum = RollingChecksum::new();
        checksum.update(data);
        assert_eq!(digest, checksum.digest());
    }

    #[test]
    fn rolling_digest_from_bytes_empty() {
        let digest = RollingDigest::from_bytes(b"");
        assert_eq!(digest.len(), 0);
        assert!(digest.is_empty());
        assert_eq!(digest, RollingDigest::ZERO);
    }

    #[test]
    fn rolling_digest_from_reader_with_buffer() {
        let data = b"streamed input data";
        let mut reader = Cursor::new(data.to_vec());
        let mut buffer = [0u8; 8];
        let digest = RollingDigest::from_reader_with_buffer(&mut reader, &mut buffer).unwrap();
        assert_eq!(digest.len(), data.len());
        assert_eq!(digest, RollingDigest::from_bytes(data));
    }

    #[test]
    fn rolling_digest_from_reader_with_buffer_empty_buffer_fails() {
        let mut reader = Cursor::new(b"data".to_vec());
        let mut buffer: [u8; 0] = [];
        let result = RollingDigest::from_reader_with_buffer(&mut reader, &mut buffer);
        assert!(result.is_err());
    }

    #[test]
    fn rolling_digest_from_reader() {
        let data = b"test data for reader";
        let mut reader = Cursor::new(data.to_vec());
        let digest = RollingDigest::from_reader(&mut reader).unwrap();
        assert_eq!(digest.len(), data.len());
        assert_eq!(digest, RollingDigest::from_bytes(data));
    }

    #[test]
    fn rolling_digest_from_reader_empty() {
        let mut reader = Cursor::new(Vec::<u8>::new());
        let digest = RollingDigest::from_reader(&mut reader).unwrap();
        assert!(digest.is_empty());
    }

    #[test]
    fn rolling_digest_from_value_unpacks_correctly() {
        // Pack s2 in high 16 bits, s1 in low 16 bits
        let s1: u16 = 0x1234;
        let s2: u16 = 0x5678;
        let packed: u32 = ((s2 as u32) << 16) | (s1 as u32);
        let digest = RollingDigest::from_value(packed, 42);
        assert_eq!(digest.sum1(), s1);
        assert_eq!(digest.sum2(), s2);
        assert_eq!(digest.len(), 42);
    }

    #[test]
    fn rolling_digest_value_packs_correctly() {
        let digest = RollingDigest::new(0x1234, 0x5678, 100);
        let value = digest.value();
        // s2 should be in high 16 bits, s1 in low 16 bits
        assert_eq!(value & 0xFFFF, 0x1234);
        assert_eq!((value >> 16) & 0xFFFF, 0x5678);
    }

    #[test]
    fn rolling_digest_from_value_and_value_roundtrip() {
        let original = RollingDigest::new(0xABCD, 0xEF01, 256);
        let value = original.value();
        let reconstructed = RollingDigest::from_value(value, 256);
        assert_eq!(original, reconstructed);
    }

    #[test]
    fn rolling_digest_from_le_bytes() {
        let bytes: [u8; 4] = [0x34, 0x12, 0x78, 0x56]; // Little-endian for 0x56781234
        let digest = RollingDigest::from_le_bytes(bytes, 100);
        assert_eq!(digest.sum1(), 0x1234);
        assert_eq!(digest.sum2(), 0x5678);
        assert_eq!(digest.len(), 100);
    }

    #[test]
    fn rolling_digest_to_le_bytes() {
        let digest = RollingDigest::new(0x1234, 0x5678, 100);
        let bytes = digest.to_le_bytes();
        assert_eq!(bytes, [0x34, 0x12, 0x78, 0x56]);
    }

    #[test]
    fn rolling_digest_le_bytes_roundtrip() {
        let original = RollingDigest::new(0xABCD, 0xEF01, 500);
        let bytes = original.to_le_bytes();
        let reconstructed = RollingDigest::from_le_bytes(bytes, 500);
        assert_eq!(original, reconstructed);
    }

    #[test]
    fn rolling_digest_from_le_slice_valid() {
        let slice: &[u8] = &[0x34, 0x12, 0x78, 0x56];
        let digest = RollingDigest::from_le_slice(slice, 100).unwrap();
        assert_eq!(digest.sum1(), 0x1234);
        assert_eq!(digest.sum2(), 0x5678);
    }

    #[test]
    fn rolling_digest_from_le_slice_wrong_length() {
        let result = RollingDigest::from_le_slice(&[0x01, 0x02, 0x03], 0);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.len(), 3);
    }

    #[test]
    fn rolling_digest_from_le_slice_empty() {
        let result = RollingDigest::from_le_slice(&[], 0);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.is_empty());
    }

    #[test]
    fn rolling_digest_from_le_slice_too_long() {
        let result = RollingDigest::from_le_slice(&[1, 2, 3, 4, 5], 0);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.len(), 5);
    }

    #[test]
    fn rolling_digest_write_le_bytes_valid() {
        let digest = RollingDigest::new(0x1234, 0x5678, 100);
        let mut out = [0u8; 4];
        digest.write_le_bytes(&mut out).unwrap();
        assert_eq!(out, [0x34, 0x12, 0x78, 0x56]);
    }

    #[test]
    fn rolling_digest_write_le_bytes_wrong_length() {
        let digest = RollingDigest::new(0x1234, 0x5678, 100);
        let mut out = [0u8; 3];
        let result = digest.write_le_bytes(&mut out);
        assert!(result.is_err());
    }

    #[test]
    fn rolling_digest_write_le_bytes_too_long() {
        let digest = RollingDigest::new(0x1234, 0x5678, 100);
        let mut out = [0u8; 5];
        let result = digest.write_le_bytes(&mut out);
        assert!(result.is_err());
    }

    #[test]
    fn rolling_digest_read_le_from() {
        let data: [u8; 4] = [0x34, 0x12, 0x78, 0x56];
        let mut reader = Cursor::new(data);
        let digest = RollingDigest::read_le_from(&mut reader, 100).unwrap();
        assert_eq!(digest.sum1(), 0x1234);
        assert_eq!(digest.sum2(), 0x5678);
        assert_eq!(digest.len(), 100);
    }

    #[test]
    fn rolling_digest_read_le_from_short_read() {
        let data: [u8; 2] = [0x34, 0x12];
        let mut reader = Cursor::new(data);
        let result = RollingDigest::read_le_from(&mut reader, 100);
        assert!(result.is_err());
    }

    #[test]
    fn rolling_digest_write_le_to() {
        let digest = RollingDigest::new(0x1234, 0x5678, 100);
        let mut buffer = Vec::new();
        digest.write_le_to(&mut buffer).unwrap();
        assert_eq!(buffer, vec![0x34, 0x12, 0x78, 0x56]);
    }

    #[test]
    fn rolling_digest_read_write_roundtrip() {
        let original = RollingDigest::new(0xABCD, 0xEF01, 256);
        let mut buffer = Vec::new();
        original.write_le_to(&mut buffer).unwrap();
        let mut reader = Cursor::new(buffer);
        let reconstructed = RollingDigest::read_le_from(&mut reader, 256).unwrap();
        assert_eq!(original, reconstructed);
    }

    #[test]
    fn rolling_digest_len_and_is_empty() {
        let empty = RollingDigest::new(0, 0, 0);
        assert!(empty.is_empty());
        assert_eq!(empty.len(), 0);

        let non_empty = RollingDigest::new(1, 2, 100);
        assert!(!non_empty.is_empty());
        assert_eq!(non_empty.len(), 100);
    }

    #[test]
    fn rolling_digest_from_u32_owned() {
        let digest = RollingDigest::new(0x1234, 0x5678, 100);
        let value: u32 = digest.into();
        assert_eq!(value, digest.value());
    }

    #[test]
    fn rolling_digest_from_u32_ref() {
        let digest = RollingDigest::new(0x1234, 0x5678, 100);
        let value: u32 = (&digest).into();
        assert_eq!(value, digest.value());
    }

    #[test]
    fn rolling_digest_from_bytes_array_owned() {
        let digest = RollingDigest::new(0x1234, 0x5678, 100);
        let bytes: [u8; 4] = digest.into();
        assert_eq!(bytes, [0x34, 0x12, 0x78, 0x56]);
    }

    #[test]
    fn rolling_digest_from_bytes_array_ref() {
        let digest = RollingDigest::new(0x1234, 0x5678, 100);
        let bytes: [u8; 4] = (&digest).into();
        assert_eq!(bytes, [0x34, 0x12, 0x78, 0x56]);
    }

    #[test]
    fn rolling_digest_default_is_zero() {
        assert_eq!(RollingDigest::default(), RollingDigest::ZERO);
    }

    #[test]
    fn rolling_digest_clone_equals_original() {
        let digest = RollingDigest::new(100, 200, 300);
        assert_eq!(digest.clone(), digest);
    }

    #[test]
    fn rolling_digest_debug_format() {
        let digest = RollingDigest::new(1, 2, 3);
        let debug = format!("{digest:?}");
        assert!(debug.contains("RollingDigest"));
    }

    #[test]
    fn rolling_digest_copy_semantics() {
        let original = RollingDigest::new(1, 2, 3);
        let copy = original;
        assert_eq!(original, copy);
    }

    #[test]
    fn rolling_digest_equality() {
        let a = RollingDigest::new(1, 2, 3);
        let b = RollingDigest::new(1, 2, 3);
        let c = RollingDigest::new(1, 2, 4);
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn rolling_digest_from_bytes_deterministic() {
        let data = b"test data";
        let digest1 = RollingDigest::from_bytes(data);
        let digest2 = RollingDigest::from_bytes(data);
        assert_eq!(digest1, digest2);
    }

    #[test]
    fn rolling_digest_different_data_different_digest() {
        let digest1 = RollingDigest::from_bytes(b"hello");
        let digest2 = RollingDigest::from_bytes(b"world");
        assert_ne!(digest1, digest2);
    }
}

#[cfg(test)]
mod proptest_tests {
    use super::*;
    use proptest::prelude::*;
    use std::io::Cursor;

    proptest! {
        /// Property: from_bytes produces deterministic digests.
        #[test]
        fn from_bytes_deterministic(data: Vec<u8>) {
            let digest1 = RollingDigest::from_bytes(&data);
            let digest2 = RollingDigest::from_bytes(&data);
            prop_assert_eq!(digest1, digest2);
        }

        /// Property: to_le_bytes/from_le_bytes roundtrip preserves value.
        #[test]
        fn le_bytes_roundtrip(s1: u16, s2: u16, len in 0usize..1_000_000) {
            let original = RollingDigest::new(s1, s2, len);
            let bytes = original.to_le_bytes();
            let reconstructed = RollingDigest::from_le_bytes(bytes, len);
            prop_assert_eq!(original, reconstructed);
        }

        /// Property: value() produces consistent packed u32.
        #[test]
        fn value_consistent(s1: u16, s2: u16, len in 0usize..1_000_000) {
            let digest = RollingDigest::new(s1, s2, len);
            let value = digest.value();
            let reconstructed = RollingDigest::from_value(value, len);
            prop_assert_eq!(digest, reconstructed);
        }

        /// Property: write_le_to/read_le_from roundtrip preserves digest.
        #[test]
        fn write_read_roundtrip(s1: u16, s2: u16, len in 0usize..1_000_000) {
            let original = RollingDigest::new(s1, s2, len);
            let mut buffer = Vec::new();
            original.write_le_to(&mut buffer).expect("write succeeds");
            let mut reader = Cursor::new(buffer);
            let reconstructed = RollingDigest::read_le_from(&mut reader, len).expect("read succeeds");
            prop_assert_eq!(original, reconstructed);
        }

        /// Property: digest length equals input length.
        #[test]
        fn len_matches_input(data: Vec<u8>) {
            let digest = RollingDigest::from_bytes(&data);
            prop_assert_eq!(digest.len(), data.len());
        }

        /// Property: Into<u32> and Into<[u8; 4]> are consistent.
        #[test]
        fn into_conversions_consistent(s1: u16, s2: u16, len in 0usize..100) {
            let digest = RollingDigest::new(s1, s2, len);
            let value: u32 = digest.into();
            let bytes: [u8; 4] = digest.into();
            let value_from_bytes = u32::from_le_bytes(bytes);
            prop_assert_eq!(value, value_from_bytes);
        }

        /// Property: empty digest is correctly identified.
        #[test]
        fn is_empty_correct(s1: u16, s2: u16, len: usize) {
            let digest = RollingDigest::new(s1, s2, len);
            prop_assert_eq!(digest.is_empty(), len == 0);
        }
    }
}
