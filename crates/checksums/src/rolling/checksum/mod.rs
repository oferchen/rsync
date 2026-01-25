use std::io::{self, IoSlice, Read};

#[cfg(target_arch = "aarch64")]
pub(crate) mod neon;
#[cfg(test)]
mod tests;
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
pub(crate) mod x86;

use super::digest::RollingDigest;
use super::error::RollingError;

const VECTORED_STACK_CAPACITY: usize = 128;

/// Reports whether SIMD acceleration is currently available for the rolling
/// checksum implementation.
///
/// The function inspects the active architecture at runtime (or compile time
/// for platforms where the presence of SIMD is guaranteed) and mirrors the
/// dispatch logic used by [`RollingChecksum::update`]. Callers such as the
/// version banner renderer surface the result to users so the advertised
/// capabilities match the code paths selected during checksum updates.
#[must_use]
pub fn simd_acceleration_available() -> bool {
    simd_available_arch()
}

#[cfg(target_arch = "aarch64")]
#[inline]
fn simd_available_arch() -> bool {
    neon::simd_available()
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
#[inline]
fn simd_available_arch() -> bool {
    x86::simd_available()
}

#[cfg(not(any(target_arch = "aarch64", target_arch = "x86", target_arch = "x86_64")))]
#[inline]
fn simd_available_arch() -> bool {
    false
}

/// Rolling checksum used by rsync for weak block matching (often called `rsum`).
///
/// Mirrors upstream rsync's Adler-32 style weak checksum: `s1` accumulates the byte sum,
/// `s2` accumulates prefix sums, both truncated to 16 bits.
///
/// # Upstream Reference
///
/// - `checksum.c:82` - `get_checksum1()` - Rolling checksum computation
/// - `match.c:39` - `build_hash_table()` - Block hash table construction
/// - `match.c:193` - `hash_search()` - Fast block lookup using rolling checksum
///
/// This implementation provides CPU-accelerated SIMD variants (AVX2, SSE2, NEON)
/// with scalar fallback, maintaining byte-for-byte compatibility with upstream.
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
    ///
    /// # Examples
    ///
    /// ```
    /// use checksums::RollingChecksum;
    ///
    /// let mut checksum = RollingChecksum::new();
    /// assert!(checksum.is_empty());
    /// assert_eq!(checksum.len(), 0);
    /// ```
    #[must_use]
    pub const fn new() -> Self {
        Self {
            s1: 0,
            s2: 0,
            len: 0,
        }
    }

    /// Reconstructs a rolling checksum from a previously captured digest.
    #[must_use]
    pub const fn from_digest(digest: RollingDigest) -> Self {
        Self {
            s1: digest.sum1() as u32,
            s2: digest.sum2() as u32,
            len: digest.len(),
        }
    }

    /// Resets the checksum back to its initial state.
    pub const fn reset(&mut self) {
        self.s1 = 0;
        self.s2 = 0;
        self.len = 0;
    }

    /// Returns the number of bytes that contributed to the current state.
    #[inline]
    #[must_use]
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Returns `true` if no bytes have been observed yet.
    #[inline]
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Updates the checksum with an additional slice of bytes.
    ///
    /// This is the primary method for computing checksums over data blocks.
    /// SIMD acceleration (AVX2/SSE2/NEON) is used when available.
    ///
    /// # Examples
    ///
    /// ```
    /// use checksums::RollingChecksum;
    ///
    /// let mut checksum = RollingChecksum::new();
    /// checksum.update(b"Hello, ");
    /// checksum.update(b"rsync!");
    ///
    /// // Equivalent to computing over the full block
    /// let mut full = RollingChecksum::new();
    /// full.update(b"Hello, rsync!");
    /// assert_eq!(checksum.value(), full.value());
    /// ```
    #[inline]
    pub fn update(&mut self, chunk: &[u8]) {
        let (s1, s2, len) = accumulate_chunk_dispatch(self.s1, self.s2, self.len, chunk);
        self.s1 = s1;
        self.s2 = s2;
        self.len = len;
    }

    /// Updates the checksum using a vectored slice of byte buffers.
    #[doc(alias = "writev")]
    #[inline]
    pub fn update_vectored(&mut self, buffers: &[IoSlice<'_>]) {
        let mut s1 = self.s1;
        let mut s2 = self.s2;
        let mut len = self.len;
        let mut scratch = [0u8; VECTORED_STACK_CAPACITY];
        let mut scratch_len = 0usize;

        for slice in buffers {
            let chunk = slice.as_ref();

            if chunk.is_empty() {
                continue;
            }

            if chunk.len() >= VECTORED_STACK_CAPACITY {
                flush_vectored_scratch(&mut s1, &mut s2, &mut len, &mut scratch, &mut scratch_len);
                (s1, s2, len) = accumulate_chunk_dispatch(s1, s2, len, chunk);
                continue;
            }

            if scratch_len + chunk.len() > VECTORED_STACK_CAPACITY {
                flush_vectored_scratch(&mut s1, &mut s2, &mut len, &mut scratch, &mut scratch_len);
            }

            scratch[scratch_len..scratch_len + chunk.len()].copy_from_slice(chunk);
            scratch_len += chunk.len();

            if scratch_len == VECTORED_STACK_CAPACITY {
                flush_vectored_scratch(&mut s1, &mut s2, &mut len, &mut scratch, &mut scratch_len);
            }
        }

        flush_vectored_scratch(&mut s1, &mut s2, &mut len, &mut scratch, &mut scratch_len);

        self.s1 = s1;
        self.s2 = s2;
        self.len = len;
    }

    /// Updates the checksum by consuming data from an [`io::Read`] implementation.
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
                Ok(n) => {
                    self.update(&buffer[..n]);
                    Self::saturating_increment_total(&mut total, n);
                }
                Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                Err(err) => return Err(err),
            }
        }
        Ok(total)
    }

    /// Convenience wrapper that allocates a heap buffer.
    pub fn update_reader<R: Read>(&mut self, reader: &mut R) -> io::Result<u64> {
        let mut buffer = vec![0u8; Self::DEFAULT_READER_BUFFER_LEN];
        self.update_reader_with_buffer(reader, &mut buffer)
    }

    /// Clears the state and updates with `block`.
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

    #[inline]
    fn saturating_increment_total(total: &mut u64, amount: usize) {
        let inc = u64::try_from(amount).unwrap_or(u64::MAX);
        *total = total.saturating_add(inc);
    }

    #[cfg(test)]
    pub(crate) fn saturating_increment_total_for_tests(total: &mut u64, amount: usize) {
        Self::saturating_increment_total(total, amount);
    }

    /// Rolls the checksum by removing one byte and adding another.
    ///
    /// This enables O(1) sliding window updates for delta detection.
    /// The window size remains constant after rolling.
    ///
    /// # Examples
    ///
    /// ```
    /// use checksums::RollingChecksum;
    ///
    /// let data = b"ABCDE";
    /// let block_size = 3;
    ///
    /// // Compute checksum for "ABC"
    /// let mut rolling = RollingChecksum::new();
    /// rolling.update(&data[0..3]); // "ABC"
    ///
    /// // Roll window: remove 'A', add 'D' -> now covers "BCD"
    /// rolling.roll(data[0], data[3]).unwrap();
    ///
    /// // Verify it matches fresh computation of "BCD"
    /// let mut fresh = RollingChecksum::new();
    /// fresh.update(&data[1..4]); // "BCD"
    /// assert_eq!(rolling.value(), fresh.value());
    /// ```
    ///
    /// # Errors
    ///
    /// Returns [`RollingError::EmptyWindow`] if no bytes have been processed.
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

    /// Rolls multiple bytes at once.
    ///
    /// More efficient than calling [`roll`](Self::roll) repeatedly when
    /// sliding the window by multiple positions. Uses weighted-delta
    /// aggregation to reduce per-byte overhead.
    ///
    /// # Examples
    ///
    /// ```
    /// use checksums::RollingChecksum;
    ///
    /// let data = b"ABCDEFGH";
    /// let block_size = 4;
    ///
    /// // Compute checksum for "ABCD"
    /// let mut rolling = RollingChecksum::new();
    /// rolling.update(&data[0..4]);
    ///
    /// // Roll by 3 positions: "ABCD" -> "EFGH"
    /// rolling.roll_many(&data[0..3], &data[4..7]).unwrap();
    /// // One more roll to complete the shift
    /// rolling.roll(data[3], data[7]).unwrap();
    ///
    /// // Verify
    /// let mut fresh = RollingChecksum::new();
    /// fresh.update(&data[4..8]); // "EFGH"
    /// assert_eq!(rolling.value(), fresh.value());
    /// ```
    ///
    /// # Errors
    ///
    /// - [`RollingError::MismatchedSliceLength`] if slices differ in length.
    /// - [`RollingError::EmptyWindow`] if no bytes have been processed.
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

        let count = outgoing.len();
        let (Ok(count_i128), Ok(count_u32)) = (i128::try_from(count), u32::try_from(count)) else {
            return self.roll_many_scalar(outgoing, incoming);
        };

        let mut sum_outgoing = 0i128;
        let mut sum_delta = 0i128;
        let mut weighted_delta = 0i128;

        let mut weight = count_i128;
        for (&out_b, &in_b) in outgoing.iter().zip(incoming.iter()) {
            let outgoing_val = i128::from(out_b);
            let incoming_val = i128::from(in_b);
            let delta = incoming_val - outgoing_val;

            sum_outgoing += outgoing_val;
            sum_delta += delta;

            weighted_delta += delta * weight;
            weight -= 1;
        }

        debug_assert!(weight >= 0);

        let original_s1 = self.s1;

        let new_s1 = original_s1.wrapping_add(sum_delta as u32) & 0xffff;

        let new_s2 = self
            .s2
            .wrapping_sub(window_len.wrapping_mul(sum_outgoing as u32))
            .wrapping_add(original_s1.wrapping_mul(count_u32))
            .wrapping_add(weighted_delta as u32)
            & 0xffff;

        self.s1 = new_s1;
        self.s2 = new_s2;
        Ok(())
    }

    #[inline]
    fn roll_many_scalar(&mut self, outgoing: &[u8], incoming: &[u8]) -> Result<(), RollingError> {
        for (&out, &inn) in outgoing.iter().zip(incoming.iter()) {
            self.roll(out, inn)?;
        }
        Ok(())
    }

    /// Returns the rolling checksum value in rsync's packed 32-bit representation.
    ///
    /// The format is `(s2 << 16) | s1`, matching upstream rsync's wire format.
    /// Use this value for hash table lookups during delta detection.
    ///
    /// # Examples
    ///
    /// ```
    /// use checksums::RollingChecksum;
    ///
    /// let mut checksum = RollingChecksum::new();
    /// checksum.update(b"test data");
    ///
    /// let packed = checksum.value();
    /// // Upper 16 bits: s2 (weighted sum)
    /// // Lower 16 bits: s1 (byte sum)
    /// let s1 = packed & 0xFFFF;
    /// let s2 = packed >> 16;
    /// ```
    #[inline]
    #[must_use]
    pub const fn value(&self) -> u32 {
        (self.s2 << 16) | self.s1
    }

    /// Returns the current state as a structured digest.
    ///
    /// The digest can be used to save/restore checksum state, useful for
    /// checkpointing during large file processing.
    ///
    /// # Examples
    ///
    /// ```
    /// use checksums::RollingChecksum;
    ///
    /// let mut checksum = RollingChecksum::new();
    /// checksum.update(b"some data");
    ///
    /// // Save state
    /// let digest = checksum.digest();
    ///
    /// // Restore later
    /// let restored = RollingChecksum::from_digest(digest);
    /// assert_eq!(checksum.value(), restored.value());
    /// ```
    #[must_use]
    pub const fn digest(&self) -> RollingDigest {
        RollingDigest::new(self.s1 as u16, self.s2 as u16, self.len)
    }

    #[cfg(test)]
    pub(crate) const fn force_state(&mut self, s1: u32, s2: u32, len: usize) {
        self.s1 = s1;
        self.s2 = s2;
        self.len = len;
    }
}

impl From<RollingDigest> for RollingChecksum {
    fn from(digest: RollingDigest) -> Self {
        Self::from_digest(digest)
    }
}

impl_from_owned_and_ref!(RollingChecksum => RollingDigest, digest);

/// Architecture-neutral dispatcher:
/// 1. try arch-accelerated implementation,
/// 2. fall back to scalar.
#[inline]
fn accumulate_chunk_dispatch(s1: u32, s2: u32, len: usize, chunk: &[u8]) -> (u32, u32, usize) {
    if chunk.is_empty() {
        return (s1, s2, len);
    }

    if let Some(accel) = accumulate_chunk_arch(s1, s2, len, chunk) {
        return mask_result(accel);
    }

    mask_result(accumulate_chunk_scalar_raw(s1, s2, len, chunk))
}

/// Arch-specific strategy: returns `Some(...)` if this arch has a fast path,
/// otherwise `None`. This keeps the top-level dispatcher linear and avoids
/// unreachable-code patterns.
#[cfg(target_arch = "aarch64")]
#[inline]
fn accumulate_chunk_arch(s1: u32, s2: u32, len: usize, chunk: &[u8]) -> Option<(u32, u32, usize)> {
    Some(neon::accumulate_chunk(s1, s2, len, chunk))
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
#[inline]
fn accumulate_chunk_arch(s1: u32, s2: u32, len: usize, chunk: &[u8]) -> Option<(u32, u32, usize)> {
    x86::try_accumulate_chunk(s1, s2, len, chunk)
}

#[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64", target_arch = "x86")))]
#[inline]
fn accumulate_chunk_arch(
    _s1: u32,
    _s2: u32,
    _len: usize,
    _chunk: &[u8],
) -> Option<(u32, u32, usize)> {
    None
}

#[inline]
const fn mask_result((s1, s2, len): (u32, u32, usize)) -> (u32, u32, usize) {
    (s1 & 0xffff, s2 & 0xffff, len)
}

#[inline]
fn accumulate_chunk_scalar_raw(
    mut s1: u32,
    mut s2: u32,
    len: usize,
    chunk: &[u8],
) -> (u32, u32, usize) {
    if chunk.is_empty() {
        return (s1, s2, len);
    }

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

    (s1, s2, len.saturating_add(chunk.len()))
}

/// Flushes accumulated bytes from the scratch buffer into the checksum state.
///
/// During vectored I/O processing, small chunks are collected into a stack-allocated
/// scratch buffer to improve cache locality. This function processes the buffered
/// bytes when the buffer is full or at the end of processing.
///
/// # Arguments
///
/// * `s1`, `s2` - Rolling checksum accumulators (modified in place)
/// * `len` - Total bytes processed counter (modified in place)
/// * `scratch` - Stack buffer holding pending bytes
/// * `scratch_len` - Number of valid bytes in scratch (reset to 0 after flush)
#[inline]
fn flush_vectored_scratch(
    s1: &mut u32,
    s2: &mut u32,
    len: &mut usize,
    scratch: &mut [u8; VECTORED_STACK_CAPACITY],
    scratch_len: &mut usize,
) {
    if *scratch_len == 0 {
        return;
    }

    let (ns1, ns2, nlen) = accumulate_chunk_dispatch(*s1, *s2, *len, &scratch[..*scratch_len]);
    *s1 = ns1;
    *s2 = ns2;
    *len = nlen;
    *scratch_len = 0;
}

#[cfg(test)]
pub(crate) fn accumulate_chunk_scalar_for_tests(
    s1: u32,
    s2: u32,
    len: usize,
    chunk: &[u8],
) -> (u32, u32, usize) {
    accumulate_chunk_scalar_raw(s1, s2, len, chunk)
}
