use core::fmt;
use std::io::{self, Read, Write};

/// Errors that can occur while updating the rolling checksum state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RollingError {
    /// The checksum window is empty, preventing the rolling update from making progress.
    EmptyWindow,
    /// The checksum window length exceeds what can be represented in 32 bits.
    WindowTooLarge {
        /// Number of bytes present in the rolling window when the error was raised.
        len: usize,
    },
    /// The number of outgoing bytes does not match the number of incoming bytes.
    MismatchedSliceLength {
        /// Number of bytes being removed from the rolling window.
        outgoing: usize,
        /// Number of bytes being appended to the rolling window.
        incoming: usize,
    },
}

impl fmt::Display for RollingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyWindow => write!(f, "rolling checksum requires a non-empty window"),
            Self::WindowTooLarge { len } => write!(
                f,
                "rolling checksum window of {len} bytes exceeds 32-bit limit"
            ),
            Self::MismatchedSliceLength { outgoing, incoming } => write!(
                f,
                "rolling checksum requires outgoing ({outgoing}) and incoming ({incoming}) slices to have the same length"
            ),
        }
    }
}

impl std::error::Error for RollingError {}

/// Error returned when reconstructing a rolling checksum digest from a byte slice of the wrong length.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RollingSliceError {
    len: usize,
}

impl RollingSliceError {
    /// Number of bytes the caller supplied when the error was raised.
    #[must_use]
    pub const fn len(self) -> usize {
        self.len
    }

    /// Reports whether the provided slice was empty when the error occurred.
    ///
    /// # Examples
    ///
    /// ```
    /// use rsync_checksums::{RollingDigest, RollingSliceError};
    ///
    /// let err = RollingDigest::from_le_slice(&[], 0).unwrap_err();
    /// assert!(err.is_empty());
    /// ```
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.len == 0
    }

    /// Number of bytes required to decode a rolling checksum digest.
    pub const EXPECTED_LEN: usize = 4;
}

impl fmt::Display for RollingSliceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "rolling checksum digest requires {} bytes, received {}",
            Self::EXPECTED_LEN,
            self.len
        )
    }
}

impl std::error::Error for RollingSliceError {}

/// Rolling checksum used by rsync for weak block matching (often called `rsum`).
///
/// The checksum mirrors upstream rsync's Adler-32 variant where the first component
/// (`s1`) accumulates the byte sum and the second component (`s2`) tracks the sum of
/// the running prefix sums. Both components are truncated to 16 bits after every
/// update to match the canonical algorithm used during delta transfer.
#[doc(alias = "rsum")]
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RollingChecksum {
    s1: u32,
    s2: u32,
    len: usize,
}

impl RollingChecksum {
    /// Default buffer length used by [`update_reader`](Self::update_reader).
    pub const DEFAULT_READER_BUFFER_LEN: usize = 32 * 1024;

    /// Creates a new rolling checksum with zeroed state.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            s1: 0,
            s2: 0,
            len: 0,
        }
    }

    /// Reconstructs a rolling checksum from a previously captured digest.
    ///
    /// The helper mirrors the restoration logic used by upstream rsync when a receiver
    /// rehydrates the checksum state from the `sum1`/`sum2` pair transmitted over the
    /// wire. Providing a dedicated constructor avoids repeating the field mapping in
    /// higher layers and keeps the internal truncation rules encapsulated within the
    /// type. The returned checksum is immediately ready for further rolling updates.
    ///
    /// # Examples
    ///
    /// ```
    /// use rsync_checksums::{RollingChecksum, RollingDigest};
    ///
    /// let mut checksum = RollingChecksum::new();
    /// checksum.update(b"delta state");
    /// let digest = checksum.digest();
    ///
    /// let restored = RollingChecksum::from_digest(digest);
    /// assert_eq!(restored.digest(), digest);
    /// ```
    #[must_use]
    pub const fn from_digest(digest: RollingDigest) -> Self {
        Self {
            s1: digest.sum1() as u32,
            s2: digest.sum2() as u32,
            len: digest.len(),
        }
    }

    /// Resets the checksum back to its initial state.
    pub fn reset(&mut self) {
        self.s1 = 0;
        self.s2 = 0;
        self.len = 0;
    }

    /// Returns the number of bytes that contributed to the current state.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Returns `true` if no bytes have been observed yet.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Updates the checksum with an additional slice of bytes.
    #[inline]
    pub fn update(&mut self, chunk: &[u8]) {
        if chunk.is_empty() {
            return;
        }

        let mut s1 = self.s1;
        let mut s2 = self.s2;

        let mut iter = chunk.chunks_exact(4);
        for block in &mut iter {
            s1 = s1.wrapping_add(u32::from(block[0]));
            s2 = s2.wrapping_add(s1);

            s1 = s1.wrapping_add(u32::from(block[1]));
            s2 = s2.wrapping_add(s1);

            s1 = s1.wrapping_add(u32::from(block[2]));
            s2 = s2.wrapping_add(s1);

            s1 = s1.wrapping_add(u32::from(block[3]));
            s2 = s2.wrapping_add(s1);
        }

        for &byte in iter.remainder() {
            s1 = s1.wrapping_add(u32::from(byte));
            s2 = s2.wrapping_add(s1);
        }

        self.s1 = s1 & 0xffff;
        self.s2 = s2 & 0xffff;
        self.len = self.len.saturating_add(chunk.len());
    }

    /// Updates the checksum by consuming data from an [`io::Read`] implementation.
    ///
    /// The method repeatedly fills `buffer` and forwards the consumed bytes to
    /// [`update`](Self::update). It returns the total number of bytes read so
    /// callers can validate that the expected amount of data was processed.
    ///
    /// Providing an empty buffer is rejected to avoid an infinite read loop on
    /// streams that yield zero-byte reads. The buffer is reused for each read
    /// operation, making the helper allocation-free and suitable for tight
    /// transfer loops.
    ///
    /// # Errors
    ///
    /// Returns [`io::ErrorKind::InvalidInput`] when `buffer` is empty and
    /// otherwise propagates any error reported by the underlying reader.
    ///
    /// # Examples
    ///
    /// ```
    /// use rsync_checksums::RollingChecksum;
    /// use std::io::Cursor;
    ///
    /// let data = b"streamed input";
    /// let mut cursor = Cursor::new(&data[..]);
    /// let mut checksum = RollingChecksum::new();
    /// let mut buffer = [0u8; 4];
    /// let read = checksum
    ///     .update_reader_with_buffer(&mut cursor, &mut buffer)
    ///     .expect("reader succeeds");
    /// assert_eq!(read, data.len() as u64);
    /// assert_eq!(checksum.digest(), {
    ///     let mut manual = RollingChecksum::new();
    ///     manual.update(data);
    ///     manual.digest()
    /// });
    /// ```
    #[inline]
    pub fn update_reader_with_buffer<R: Read>(
        &mut self,
        reader: &mut R,
        buffer: &mut [u8],
    ) -> io::Result<u64> {
        if buffer.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "rolling checksum reader buffer must not be empty",
            ));
        }

        let mut total = 0u64;

        loop {
            match reader.read(buffer) {
                Ok(0) => break,
                Ok(read) => {
                    self.update(&buffer[..read]);
                    total += read as u64;
                }
                Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                Err(err) => return Err(err),
            }
        }

        Ok(total)
    }

    /// Updates the checksum by reading from `reader` using an internal buffer.
    ///
    /// This is a convenience wrapper around
    /// [`update_reader_with_buffer`](Self::update_reader_with_buffer) that
    /// allocates a stack buffer of
    /// [`DEFAULT_READER_BUFFER_LEN`](Self::DEFAULT_READER_BUFFER_LEN) bytes.
    /// The method is useful for tests and simple call sites that do not manage
    /// their own scratch space.
    ///
    /// # Errors
    ///
    /// Propagates any [`io::Error`] produced by the underlying reader.
    pub fn update_reader<R: Read>(&mut self, reader: &mut R) -> io::Result<u64> {
        let mut buffer = [0u8; Self::DEFAULT_READER_BUFFER_LEN];
        self.update_reader_with_buffer(reader, &mut buffer)
    }

    /// Updates the checksum by recomputing the state for a fresh block.
    ///
    /// This helper clears the internal state before delegating to [`update`](Self::update),
    /// making it convenient to compute the checksum of a block without manually calling
    /// [`reset`](Self::reset).
    pub fn update_from_block(&mut self, block: &[u8]) {
        self.reset();
        self.update(block);
    }

    /// Returns the current window length as a 32-bit value while validating invariants.
    #[inline]
    fn window_len_u32(&self) -> Result<u32, RollingError> {
        if self.len == 0 {
            return Err(RollingError::EmptyWindow);
        }

        u32::try_from(self.len).map_err(|_| RollingError::WindowTooLarge { len: self.len })
    }

    /// Performs the rolling checksum update by removing `outgoing` and appending `incoming`.
    ///
    /// # Errors
    ///
    /// Returns [`RollingError::EmptyWindow`] if the checksum has not been initialised with a
    /// block and [`RollingError::WindowTooLarge`] when the window length exceeds what the
    /// upstream algorithm supports (32 bits).
    #[inline]
    pub fn roll(&mut self, outgoing: u8, incoming: u8) -> Result<(), RollingError> {
        let window_len = self.window_len_u32()?;

        let out = u32::from(outgoing);
        let inn = u32::from(incoming);

        let new_s1 = self.s1.wrapping_sub(out).wrapping_add(inn) & 0xffff;
        let new_s2 = self
            .s2
            .wrapping_sub(window_len.wrapping_mul(out))
            .wrapping_add(new_s1)
            & 0xffff;

        self.s1 = new_s1;
        self.s2 = new_s2;
        Ok(())
    }

    /// Rolls the checksum forward by replacing multiple bytes at once.
    ///
    /// The method behaves as if [`roll`](Self::roll) were called repeatedly for each pair of
    /// outgoing and incoming bytes. Providing slices of different lengths is rejected to avoid
    /// ambiguous state. Passing empty slices is allowed and leaves the checksum unchanged after
    /// verifying that the checksum has been seeded with an initial window.
    ///
    /// # Errors
    ///
    /// Returns [`RollingError::MismatchedSliceLength`] when the outgoing and incoming slices
    /// differ in length, [`RollingError::EmptyWindow`] if the checksum has not been seeded with a
    /// block yet, and [`RollingError::WindowTooLarge`] if the internal window length exceeds the
    /// upstream limit.
    #[inline]
    pub fn roll_many(&mut self, outgoing: &[u8], incoming: &[u8]) -> Result<(), RollingError> {
        if outgoing.len() != incoming.len() {
            return Err(RollingError::MismatchedSliceLength {
                outgoing: outgoing.len(),
                incoming: incoming.len(),
            });
        }

        let window_len = self.window_len_u32()?;

        if outgoing.is_empty() {
            return Ok(());
        }

        let mut s1 = self.s1;
        let mut s2 = self.s2;

        for (&out, &inn) in outgoing.iter().zip(incoming.iter()) {
            let out = u32::from(out);
            let inn = u32::from(inn);

            s1 = s1.wrapping_sub(out).wrapping_add(inn) & 0xffff;
            s2 = s2
                .wrapping_sub(window_len.wrapping_mul(out))
                .wrapping_add(s1)
                & 0xffff;
        }

        self.s1 = s1;
        self.s2 = s2;

        Ok(())
    }

    /// Returns the rolling checksum value in rsync's packed 32-bit representation.
    #[must_use]
    pub const fn value(&self) -> u32 {
        (self.s2 << 16) | self.s1
    }

    /// Returns the current state as a structured digest.
    ///
    /// Callers that prefer trait-based conversions may also use
    /// [`RollingDigest::from`] with either an owned or borrowed
    /// [`RollingChecksum`] thanks to the blanket [`From`] implementations
    /// provided by this crate. The method remains the canonical way to extract
    /// the digest without moving the checksum out of its owner.
    #[must_use]
    pub fn digest(&self) -> RollingDigest {
        RollingDigest {
            s1: self.s1 as u16,
            s2: self.s2 as u16,
            len: self.len,
        }
    }
}

impl From<RollingDigest> for RollingChecksum {
    /// Converts a [`RollingDigest`] back into a [`RollingChecksum`] state.
    fn from(digest: RollingDigest) -> Self {
        Self::from_digest(digest)
    }
}

impl From<RollingChecksum> for RollingDigest {
    /// Converts an owned [`RollingChecksum`] into its corresponding digest.
    fn from(checksum: RollingChecksum) -> Self {
        checksum.digest()
    }
}

impl From<&RollingChecksum> for RollingDigest {
    /// Converts a borrowed [`RollingChecksum`] into its corresponding digest.
    fn from(checksum: &RollingChecksum) -> Self {
        checksum.digest()
    }
}

/// Digest produced by the rolling checksum.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RollingDigest {
    s1: u16,
    s2: u16,
    len: usize,
}

impl RollingDigest {
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
    /// use rsync_checksums::RollingDigest;
    ///
    /// let digest = RollingDigest::from_bytes(b"delta block");
    /// let manual = {
    ///     let mut checksum = rsync_checksums::RollingChecksum::new();
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
    /// use rsync_checksums::RollingDigest;
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
    /// # Examples
    ///
    /// ```
    /// use rsync_checksums::RollingDigest;
    ///
    /// let digest = RollingDigest::new(0x1357, 0x2468, 4096);
    /// let bytes = digest.to_le_bytes();
    /// let parsed = RollingDigest::from_le_bytes(bytes, digest.len());
    ///
    /// assert_eq!(parsed, digest);
    /// assert_eq!(parsed.sum1(), 0x1357);
    /// assert_eq!(parsed.sum2(), 0x2468);
    /// ```
    #[doc(alias = "SIVAL")]
    #[must_use]
    pub const fn from_le_bytes(bytes: [u8; 4], len: usize) -> Self {
        Self::from_value(u32::from_le_bytes(bytes), len)
    }

    /// Constructs a digest from a little-endian byte slice, validating the input length.
    ///
    /// This helper complements [`Self::from_le_bytes`] by accepting arbitrary byte slices, making
    /// it convenient to decode digests from network buffers without first converting them into an
    /// array. When the slice does not contain exactly four bytes, the function returns
    /// [`RollingSliceError`], mirroring upstream rsync which treats truncated digests as fatal
    /// protocol violations.
    ///
    /// # Errors
    ///
    /// Returns [`RollingSliceError`] if `bytes` does not contain exactly four elements.
    ///
    /// # Examples
    ///
    /// ```
    /// use rsync_checksums::{RollingDigest, RollingSliceError};
    ///
    /// let digest = RollingDigest::new(0x1357, 0x2468, 4096);
    /// let parsed = RollingDigest::from_le_slice(&digest.to_le_bytes(), digest.len())?;
    /// assert_eq!(parsed.sum1(), 0x1357);
    /// assert_eq!(parsed.sum2(), 0x2468);
    /// # Ok::<(), RollingSliceError>(())
    /// ```
    pub fn from_le_slice(bytes: &[u8], len: usize) -> Result<Self, RollingSliceError> {
        if bytes.len() != RollingSliceError::EXPECTED_LEN {
            return Err(RollingSliceError { len: bytes.len() });
        }

        let mut array = [0u8; RollingSliceError::EXPECTED_LEN];
        array.copy_from_slice(bytes);
        Ok(Self::from_le_bytes(array, len))
    }

    /// Returns the first checksum component (sum of bytes).
    #[doc(alias = "s1")]
    #[must_use]
    pub const fn sum1(&self) -> u16 {
        self.s1
    }

    /// Returns the second checksum component (sum of prefix sums).
    #[doc(alias = "s2")]
    #[must_use]
    pub const fn sum2(&self) -> u16 {
        self.s2
    }

    /// Returns the number of bytes that contributed to the digest.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Returns whether the digest was computed from zero bytes.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Returns the checksum in rsync's packed 32-bit representation.
    #[must_use]
    pub const fn value(&self) -> u32 {
        ((self.s2 as u32) << 16) | (self.s1 as u32)
    }

    /// Returns the checksum encoded as the little-endian byte sequence used on the wire.
    #[doc(alias = "SIVAL")]
    #[must_use]
    pub const fn to_le_bytes(&self) -> [u8; 4] {
        self.value().to_le_bytes()
    }

    /// Writes the checksum to the caller-provided buffer using the little-endian wire format.
    ///
    /// This helper avoids allocating a temporary array when the caller already owns a
    /// fixed-size buffer (for example when writing directly into a network packet or an
    /// mmap-backed region). The output matches [`Self::to_le_bytes`], ensuring the result can be
    /// transmitted verbatim to peers expecting the packed `sum1`/`sum2` representation.
    ///
    /// # Errors
    ///
    /// Returns [`RollingSliceError`] when `out` does not contain exactly four bytes. The buffer is
    /// left untouched on error.
    ///
    /// # Examples
    ///
    /// ```
    /// use rsync_checksums::RollingDigest;
    ///
    /// let digest = RollingDigest::new(0x1357, 0x2468, 1024);
    /// let mut buf = [0u8; 4];
    /// digest.write_le_bytes(&mut buf).expect("buffer has the right size");
    /// assert_eq!(buf, digest.to_le_bytes());
    /// ```
    pub fn write_le_bytes(&self, out: &mut [u8]) -> Result<(), RollingSliceError> {
        if out.len() != RollingSliceError::EXPECTED_LEN {
            return Err(RollingSliceError { len: out.len() });
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
    ///
    /// # Examples
    ///
    /// ```
    /// use rsync_checksums::RollingDigest;
    /// use std::io::Cursor;
    ///
    /// let digest = RollingDigest::new(0xaaaa, 0xbbbb, 4096);
    /// let mut cursor = Cursor::new(digest.to_le_bytes());
    /// let parsed = RollingDigest::read_le_from(&mut cursor, digest.len()).unwrap();
    /// assert_eq!(parsed, digest);
    /// ```
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
    ///
    /// # Examples
    ///
    /// ```
    /// use rsync_checksums::RollingDigest;
    /// use std::io::Cursor;
    ///
    /// let digest = RollingDigest::new(0x1234, 0x5678, 1024);
    /// let mut cursor = Cursor::new(Vec::new());
    /// digest.write_le_to(&mut cursor).unwrap();
    /// assert_eq!(cursor.into_inner(), digest.to_le_bytes());
    /// ```
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

#[cfg(test)]
mod tests {
    use super::*;

    use proptest::prelude::*;
    use std::io::{self, Cursor, Read};

    fn reference_digest(data: &[u8]) -> RollingDigest {
        let mut s1: u64 = 0;
        let mut s2: u64 = 0;

        for &byte in data {
            s1 += u64::from(byte);
            s2 += s1;
        }

        RollingDigest::new((s1 & 0xffff) as u16, (s2 & 0xffff) as u16, data.len())
    }

    fn random_data_and_window() -> impl Strategy<Value = (Vec<u8>, usize)> {
        prop::collection::vec(any::<u8>(), 1..=256).prop_flat_map(|data| {
            let len = data.len();
            let window_range = 1..=len;
            (Just(data), window_range).prop_map(|(data, window)| (data, window))
        })
    }

    fn chunked_sequences() -> impl Strategy<Value = Vec<Vec<u8>>> {
        prop::collection::vec(prop::collection::vec(any::<u8>(), 0..=64), 1..=8)
    }

    fn roll_many_sequences() -> impl Strategy<Value = (Vec<u8>, Vec<(u8, u8)>)> {
        prop::collection::vec(any::<u8>(), 1..=64).prop_flat_map(|seed| {
            let seed_clone = seed.clone();
            prop::collection::vec((any::<u8>(), any::<u8>()), 0..=32)
                .prop_map(move |pairs| (seed_clone.clone(), pairs))
        })
    }

    #[test]
    fn digest_matches_reference_for_known_input() {
        let data = b"rsync rolling checksum";
        let digest = reference_digest(data);

        let mut checksum = RollingChecksum::new();
        checksum.update(data);
        assert_eq!(checksum.digest(), digest);
        assert_eq!(checksum.value(), digest.value());
    }

    #[test]
    fn digest_from_bytes_matches_manual_update() {
        let data = b"from bytes helper";
        let digest = RollingDigest::from_bytes(data);

        let manual = {
            let mut checksum = RollingChecksum::new();
            checksum.update(data);
            checksum.digest()
        };

        assert_eq!(digest, manual);
        assert_eq!(digest.len(), data.len());
    }

    #[test]
    fn digest_round_trips_through_packed_value() {
        let sample = RollingDigest::new(0x1357, 0x2468, 4096);
        let packed = sample.value();
        let unpacked = RollingDigest::from_value(packed, sample.len());

        assert_eq!(unpacked, sample);
        assert_eq!(unpacked.value(), packed);
        assert_eq!(unpacked.sum1(), sample.sum1());
        assert_eq!(unpacked.sum2(), sample.sum2());
        assert_eq!(unpacked.len(), sample.len());
    }

    #[test]
    fn digest_round_trips_through_le_bytes() {
        let sample = RollingDigest::new(0xabcd, 0x1234, 512);
        let bytes = sample.to_le_bytes();
        let parsed = RollingDigest::from_le_bytes(bytes, sample.len());

        assert_eq!(parsed, sample);
        assert_eq!(parsed.to_le_bytes(), bytes);
        assert_eq!(parsed.sum1(), sample.sum1());
        assert_eq!(parsed.sum2(), sample.sum2());
        assert_eq!(parsed.len(), sample.len());
    }

    #[test]
    fn digest_round_trips_through_le_slice() {
        let sample = RollingDigest::new(0x1357, 0x2468, 1024);
        let parsed = RollingDigest::from_le_slice(&sample.to_le_bytes(), sample.len())
            .expect("slice length matches the digest encoding");

        assert_eq!(parsed, sample);
        assert_eq!(parsed.to_le_bytes(), sample.to_le_bytes());
    }

    #[test]
    fn digest_into_u32_matches_value() {
        let sample = RollingDigest::new(0x4321, 0x8765, 2048);
        let expected = sample.value();
        let packed = u32::from(sample);

        assert_eq!(packed, expected);
    }

    #[test]
    fn digest_ref_into_u32_matches_value() {
        let sample = RollingDigest::new(0x1357, 0x2468, 1024);
        let expected = sample.value();
        let packed = u32::from(&sample);

        assert_eq!(packed, expected);
    }

    #[test]
    fn digest_into_array_matches_le_bytes() {
        let sample = RollingDigest::new(0x0ace, 0x1bdf, 512);
        let expected = sample.to_le_bytes();
        let bytes: [u8; 4] = sample.into();

        assert_eq!(bytes, expected);
    }

    #[test]
    fn digest_ref_into_array_matches_le_bytes() {
        let sample = RollingDigest::new(0xface, 0xbeef, 128);
        let expected = sample.to_le_bytes();
        let bytes: [u8; 4] = (&sample).into();

        assert_eq!(bytes, expected);
    }

    #[test]
    fn digest_write_le_bytes_populates_target_slice() {
        let sample = RollingDigest::new(0x0fed, 0xcba9, 2048);
        let mut buffer = [0u8; RollingSliceError::EXPECTED_LEN];
        sample
            .write_le_bytes(&mut buffer)
            .expect("buffer length matches the digest encoding");

        assert_eq!(buffer, sample.to_le_bytes());
    }

    #[test]
    fn digest_write_le_bytes_rejects_wrong_length() {
        let sample = RollingDigest::new(0x1234, 0x5678, 128);
        let mut buffer = [0u8; RollingSliceError::EXPECTED_LEN + 1];
        let err = sample
            .write_le_bytes(&mut buffer)
            .expect_err("incorrect buffer length must be rejected");

        assert_eq!(err.len(), buffer.len());
        assert!(!err.is_empty());
    }

    #[test]
    fn digest_round_trips_through_reader_and_writer() {
        let sample = RollingDigest::new(0x0102, 0x0304, 2048);

        let mut writer = Cursor::new(Vec::new());
        sample
            .write_le_to(&mut writer)
            .expect("writing into an in-memory buffer cannot fail");
        assert_eq!(writer.get_ref().as_slice(), &sample.to_le_bytes());

        let mut reader = Cursor::new(writer.into_inner());
        let parsed = RollingDigest::read_le_from(&mut reader, sample.len())
            .expect("cursor provides the expected number of bytes");

        assert_eq!(parsed, sample);
    }

    #[test]
    fn digest_from_reader_with_buffer_matches_streaming_update() {
        let data = b"streamed rolling input";
        let mut reader = Cursor::new(&data[..]);
        let mut scratch = [0u8; 5];

        let digest = RollingDigest::from_reader_with_buffer(&mut reader, &mut scratch)
            .expect("reader succeeds");
        assert_eq!(digest, RollingDigest::from_bytes(data));
        assert_eq!(digest.len(), data.len());
    }

    #[test]
    fn digest_from_reader_with_buffer_rejects_empty_scratch() {
        let mut reader = Cursor::new(b"anything".to_vec());
        let mut scratch = [];

        let err = RollingDigest::from_reader_with_buffer(&mut reader, &mut scratch)
            .expect_err("empty scratch must be rejected");
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn digest_from_reader_matches_manual_update() {
        let data = b"buffered rolling digest";
        let mut reader = Cursor::new(&data[..]);

        let digest = RollingDigest::from_reader(&mut reader).expect("reader succeeds");
        assert_eq!(digest, RollingDigest::from_bytes(data));
        assert_eq!(digest.len(), data.len());
    }

    #[test]
    fn digest_read_le_from_reports_truncated_input() {
        let mut reader = Cursor::new(vec![0xde, 0xad, 0xbe]);
        let err = RollingDigest::read_le_from(&mut reader, 0).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn digest_from_le_slice_rejects_incorrect_length() {
        let error = RollingDigest::from_le_slice(&[0u8; 3], 0)
            .expect_err("three bytes cannot encode a rolling digest");

        assert_eq!(error.len(), 3);
    }

    #[test]
    fn recomputing_block_yields_same_state() {
        let data = b"0123456789abcdef";

        let mut checksum = RollingChecksum::new();
        checksum.update(&data[..8]);

        let mut recomputed = RollingChecksum::new();
        recomputed.update_from_block(&data[..8]);

        assert_eq!(checksum.digest(), recomputed.digest());
    }

    #[test]
    fn checksum_restores_from_digest() {
        let mut checksum = RollingChecksum::new();
        checksum.update(b"rolling checksum state");

        let digest = checksum.digest();
        let restored = RollingChecksum::from_digest(digest);

        assert_eq!(restored.digest(), digest);
        assert_eq!(restored.value(), checksum.value());
        assert_eq!(restored.len(), checksum.len());
    }

    #[test]
    fn digest_from_trait_impls_matches_inherent_method() {
        let mut checksum = RollingChecksum::new();
        checksum.update(b"trait conversions");

        let expected = checksum.digest();
        let via_ref: RollingDigest = (&checksum).into();
        let via_owned: RollingDigest = checksum.clone().into();

        assert_eq!(via_ref, expected);
        assert_eq!(via_owned, expected);
    }

    #[test]
    fn rolling_matches_recomputed_checksum() {
        let data = b"The quick brown fox jumps over the lazy dog";
        let window = 12;

        let mut rolling = RollingChecksum::new();
        rolling.update(&data[..window]);

        for start in 1..=data.len() - window {
            let outgoing = data[start - 1];
            let incoming = data[start + window - 1];
            rolling.roll(outgoing, incoming).expect("rolling succeeds");

            let mut expected = RollingChecksum::new();
            expected.update(&data[start..start + window]);
            assert_eq!(rolling.digest(), expected.digest());
        }
    }

    #[test]
    fn roll_errors_for_empty_window() {
        let mut checksum = RollingChecksum::new();
        let err = checksum
            .roll(0, 0)
            .expect_err("rolling on empty window must fail");
        assert_eq!(err, RollingError::EmptyWindow);
    }

    #[test]
    fn roll_many_errors_for_empty_window() {
        let mut checksum = RollingChecksum::new();
        let err = checksum
            .roll_many(b"a", b"b")
            .expect_err("rolling on empty window must fail");
        assert_eq!(err, RollingError::EmptyWindow);
        assert_eq!(checksum.digest(), RollingDigest::new(0, 0, 0));
    }

    #[test]
    fn roll_many_empty_slices_still_require_initial_window() {
        let mut checksum = RollingChecksum::new();
        let err = checksum
            .roll_many(&[], &[])
            .expect_err("empty slices should still require a seeded window");
        assert_eq!(err, RollingError::EmptyWindow);
        assert_eq!(checksum.digest(), RollingDigest::new(0, 0, 0));
    }

    #[test]
    fn roll_errors_for_window_exceeding_u32() {
        let mut checksum = RollingChecksum::new();
        checksum.s1 = 1;
        checksum.s2 = 1;
        checksum.len = (u32::MAX as usize) + 1;

        let err = checksum.roll(0, 0).expect_err("oversized window must fail");
        assert!(matches!(err, RollingError::WindowTooLarge { .. }));
    }

    #[test]
    fn roll_many_errors_for_window_exceeding_u32() {
        let mut checksum = RollingChecksum::new();
        checksum.s1 = 1;
        checksum.s2 = 1;
        checksum.len = (u32::MAX as usize) + 1;

        let original = checksum.clone();
        let err = checksum
            .roll_many(b"a", b"b")
            .expect_err("oversized window must fail");
        assert!(matches!(err, RollingError::WindowTooLarge { .. }));
        assert_eq!(checksum, original);
    }

    #[test]
    fn roll_many_matches_multiple_single_rolls() {
        let data = b"Lorem ipsum dolor sit amet, consectetur adipiscing elit.";
        let window = 12;
        let mut rolling = RollingChecksum::new();
        rolling.update(&data[..window]);

        let mut reference = rolling.clone();
        let mut position = window;

        while position < data.len() {
            let advance = (data.len() - position).min(3);
            let outgoing_start = position - window;
            let outgoing_end = outgoing_start + advance;
            let incoming_end = position + advance;

            rolling
                .roll_many(
                    &data[outgoing_start..outgoing_end],
                    &data[position..incoming_end],
                )
                .expect("multi-byte roll succeeds");

            for (&out, &inn) in data[outgoing_start..outgoing_end]
                .iter()
                .zip(data[position..incoming_end].iter())
            {
                reference.roll(out, inn).expect("single roll succeeds");
            }

            assert_eq!(rolling.digest(), reference.digest());
            assert_eq!(rolling.value(), reference.value());

            position += advance;
        }
    }

    #[test]
    fn roll_many_rejects_mismatched_lengths() {
        let mut checksum = RollingChecksum::new();
        checksum.update(b"abcd");

        let err = checksum
            .roll_many(b"ab", b"c")
            .expect_err("length mismatch must fail");
        assert!(matches!(
            err,
            RollingError::MismatchedSliceLength {
                outgoing: 2,
                incoming: 1,
            }
        ));
    }

    #[test]
    fn roll_many_allows_empty_slices() {
        let mut checksum = RollingChecksum::new();
        checksum.update(b"rsync");

        checksum
            .roll_many(&[], &[])
            .expect("empty slices should be ignored");
        assert_eq!(
            checksum.digest(),
            RollingDigest::new(checksum.s1 as u16, checksum.s2 as u16, checksum.len)
        );
    }

    #[test]
    fn update_reader_matches_manual_update() {
        let data = b"rolling checksum stream input";
        let mut cursor = Cursor::new(&data[..]);

        let mut streamed = RollingChecksum::new();
        let read = streamed
            .update_reader(&mut cursor)
            .expect("reading from cursor succeeds");
        assert_eq!(read, data.len() as u64);

        let mut manual = RollingChecksum::new();
        manual.update(data);

        assert_eq!(streamed.digest(), manual.digest());
        assert_eq!(streamed.value(), manual.value());
    }

    #[test]
    fn update_reader_with_buffer_accepts_small_buffers() {
        let data = b"chunked rolling checksum input";
        let mut cursor = Cursor::new(&data[..]);
        let mut checksum = RollingChecksum::new();
        let mut buffer = [0u8; 3];

        let read = checksum
            .update_reader_with_buffer(&mut cursor, &mut buffer)
            .expect("buffered read succeeds");

        assert_eq!(read, data.len() as u64);

        let mut manual = RollingChecksum::new();
        manual.update(data);

        assert_eq!(checksum.digest(), manual.digest());
        assert_eq!(checksum.value(), manual.value());
    }

    struct InterruptingReader<'a> {
        inner: Cursor<&'a [u8]>,
        interrupted: bool,
    }

    impl<'a> InterruptingReader<'a> {
        fn new(data: &'a [u8]) -> Self {
            Self {
                inner: Cursor::new(data),
                interrupted: false,
            }
        }
    }

    impl<'a> Read for InterruptingReader<'a> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if !self.interrupted {
                self.interrupted = true;
                Err(io::Error::from(io::ErrorKind::Interrupted))
            } else {
                self.inner.read(buf)
            }
        }
    }

    #[test]
    fn update_reader_with_buffer_retries_after_interruption() {
        let data = b"retry after interrupt";
        let mut reader = InterruptingReader::new(data);
        let mut checksum = RollingChecksum::new();
        let mut buffer = [0u8; 4];

        let read = checksum
            .update_reader_with_buffer(&mut reader, &mut buffer)
            .expect("interrupted read should be retried");

        assert_eq!(read, data.len() as u64);

        let mut manual = RollingChecksum::new();
        manual.update(data);

        assert_eq!(checksum.digest(), manual.digest());
        assert_eq!(checksum.value(), manual.value());
    }

    #[test]
    fn update_reader_with_buffer_rejects_empty_scratch() {
        let mut checksum = RollingChecksum::new();
        let mut cursor = Cursor::new(&b""[..]);
        let mut empty: [u8; 0] = [];

        let err = checksum
            .update_reader_with_buffer(&mut cursor, &mut empty)
            .expect_err("empty buffer must fail");

        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    proptest! {
        #[test]
        fn rolling_update_matches_single_pass(chunks in chunked_sequences()) {
            let mut incremental = RollingChecksum::new();
            let mut concatenated = Vec::new();

            for chunk in &chunks {
                incremental.update(chunk);
                concatenated.extend_from_slice(chunk);
            }

            let mut single_pass = RollingChecksum::new();
            single_pass.update(&concatenated);

            prop_assert_eq!(incremental.digest(), single_pass.digest());
            prop_assert_eq!(incremental.value(), single_pass.value());
        }

        #[test]
        fn rolling_matches_reference_for_random_windows((data, window) in random_data_and_window()) {
            let mut rolling = RollingChecksum::new();
            rolling.update(&data[..window]);

            let mut reference = RollingChecksum::new();
            reference.update(&data[..window]);

            prop_assert_eq!(rolling.digest(), reference.digest());
            prop_assert_eq!(rolling.value(), reference.value());

            if data.len() > window {
                for start in 1..=data.len() - window {
                    let outgoing = data[start - 1];
                    let incoming = data[start + window - 1];
                    rolling
                        .roll(outgoing, incoming)
                        .expect("rolling update must succeed");

                    let mut recomputed = RollingChecksum::new();
                    recomputed.update(&data[start..start + window]);

                    prop_assert_eq!(rolling.digest(), recomputed.digest());
                    prop_assert_eq!(rolling.value(), recomputed.value());
                }
            }
        }

        #[test]
        fn roll_many_matches_single_rolls_for_random_sequences(
            (seed, pairs) in roll_many_sequences(),
        ) {
            let mut optimized = RollingChecksum::new();
            optimized.update(&seed);

            let mut reference = optimized.clone();

            let (outgoing, incoming): (Vec<u8>, Vec<u8>) = pairs.into_iter().unzip();
            optimized
                .roll_many(&outgoing, &incoming)
                .expect("multi-byte roll succeeds");

            for (&out, &inn) in outgoing.iter().zip(incoming.iter()) {
                reference
                    .roll(out, inn)
                    .expect("single-byte roll succeeds");
            }

            prop_assert_eq!(optimized.digest(), reference.digest());
            prop_assert_eq!(optimized.value(), reference.value());
        }

        #[test]
        fn from_digest_round_trips(data in prop::collection::vec(any::<u8>(), 0..=256)) {
            let mut checksum = RollingChecksum::new();
            checksum.update(&data);

            let digest = checksum.digest();
            let restored = RollingChecksum::from_digest(digest);

            prop_assert_eq!(restored.digest(), digest);
            prop_assert_eq!(restored.value(), checksum.value());
            prop_assert_eq!(restored.len(), checksum.len());
        }
    }
}
