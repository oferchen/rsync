use std::io::{self, IoSlice, Read};

use super::digest::RollingDigest;
use super::error::RollingError;

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
        let (s1, s2, len) = Self::accumulate_chunk(self.s1, self.s2, self.len, chunk);
        self.s1 = s1;
        self.s2 = s2;
        self.len = len;
    }

    /// Updates the checksum using a vectored slice of byte buffers.
    ///
    /// The method mirrors calling [`update`](Self::update) for each buffer in
    /// order while avoiding repeated truncation of the internal state. Empty
    /// buffers are skipped, allowing callers to forward `readv`/`writev` style
    /// slices directly without preprocessing.
    ///
    /// # Examples
    ///
    /// ```
    /// use rsync_checksums::RollingChecksum;
    /// use std::io::IoSlice;
    ///
    /// let mut checksum = RollingChecksum::new();
    /// let chunks = [IoSlice::new(b"hel"), IoSlice::new(b"lo")];
    /// checksum.update_vectored(&chunks);
    ///
    /// let mut reference = RollingChecksum::new();
    /// reference.update(b"hello");
    ///
    /// assert_eq!(checksum.digest(), reference.digest());
    /// assert_eq!(checksum.value(), reference.value());
    /// ```
    #[doc(alias = "writev")]
    #[inline]
    pub fn update_vectored(&mut self, buffers: &[IoSlice<'_>]) {
        let mut s1 = self.s1;
        let mut s2 = self.s2;
        let mut len = self.len;

        for slice in buffers {
            (s1, s2, len) = Self::accumulate_chunk(s1, s2, len, slice.as_ref());
        }

        self.s1 = s1;
        self.s2 = s2;
        self.len = len;
    }

    /// Updates the checksum by consuming data from an [`io::Read`] implementation.
    ///
    /// The method repeatedly fills `buffer` and forwards the consumed bytes to
    /// [`update`](Self::update). It returns the total number of bytes read—
    /// saturating at [`u64::MAX`]—so callers can validate that the expected amount
    /// of data was processed without observing integer wraparound.
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
                    Self::saturating_increment_total(&mut total, read);
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

    #[inline]
    fn saturating_increment_total(total: &mut u64, amount: usize) {
        let increment = u64::try_from(amount).unwrap_or(u64::MAX);
        *total = total.saturating_add(increment);
    }

    #[cfg(test)]
    pub(crate) fn saturating_increment_total_for_tests(total: &mut u64, amount: usize) {
        Self::saturating_increment_total(total, amount);
    }

    #[inline]
    fn accumulate_chunk(s1: u32, s2: u32, len: usize, chunk: &[u8]) -> (u32, u32, usize) {
        if chunk.is_empty() {
            return (s1, s2, len);
        }

        #[cfg(target_arch = "x86_64")]
        {
            if let Some(result) = x86::try_accumulate_chunk(s1, s2, len, chunk) {
                return mask_result(result);
            }
        }

        #[cfg(target_arch = "aarch64")]
        {
            return mask_result(neon::accumulate_chunk(s1, s2, len, chunk));
        }

        mask_result(accumulate_chunk_scalar_raw(s1, s2, len, chunk))
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
        RollingDigest::new(self.s1 as u16, self.s2 as u16, self.len)
    }

    #[cfg(test)]
    pub(crate) fn force_state(&mut self, s1: u32, s2: u32, len: usize) {
        self.s1 = s1;
        self.s2 = s2;
        self.len = len;
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

#[inline]
fn mask_result((s1, s2, len): (u32, u32, usize)) -> (u32, u32, usize) {
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

#[cfg(test)]
pub(crate) fn accumulate_chunk_scalar_for_tests(
    s1: u32,
    s2: u32,
    len: usize,
    chunk: &[u8],
) -> (u32, u32, usize) {
    accumulate_chunk_scalar_raw(s1, s2, len, chunk)
}

#[cfg(target_arch = "x86_64")]
#[allow(unsafe_code)]
pub(crate) mod x86 {
    #![allow(unsafe_op_in_unsafe_fn)]
    use super::accumulate_chunk_scalar_raw;
    use core::arch::x86_64::{
        __m128i, _mm_loadu_si128, _mm_mullo_epi16, _mm_sad_epu8, _mm_set_epi16, _mm_setzero_si128,
        _mm_storeu_si128, _mm_unpackhi_epi8, _mm_unpacklo_epi8,
    };

    const BLOCK_LEN: usize = 16;

    #[inline]
    pub(super) fn try_accumulate_chunk(
        s1: u32,
        s2: u32,
        len: usize,
        chunk: &[u8],
    ) -> Option<(u32, u32, usize)> {
        if chunk.len() < BLOCK_LEN {
            return None;
        }

        if !std::arch::is_x86_feature_detected!("sse2") {
            return None;
        }

        Some(unsafe { accumulate_chunk_sse2(s1, s2, len, chunk) })
    }

    #[target_feature(enable = "sse2")]
    unsafe fn accumulate_chunk_sse2(
        mut s1: u32,
        mut s2: u32,
        mut len: usize,
        mut chunk: &[u8],
    ) -> (u32, u32, usize) {
        let zero = _mm_setzero_si128();
        let high_weights = _mm_set_epi16(9, 10, 11, 12, 13, 14, 15, 16);
        let low_weights = _mm_set_epi16(1, 2, 3, 4, 5, 6, 7, 8);

        while chunk.len() >= BLOCK_LEN {
            let block = _mm_loadu_si128(chunk.as_ptr() as *const __m128i);
            let block_sum = sum_block(block, zero);
            let block_prefix = prefix_sum(block, zero, high_weights, low_weights);

            s2 = s2.wrapping_add(block_prefix);
            s2 = s2.wrapping_add(s1.wrapping_mul(BLOCK_LEN as u32));
            s1 = s1.wrapping_add(block_sum);
            len = len.saturating_add(BLOCK_LEN);
            chunk = &chunk[BLOCK_LEN..];
        }

        if !chunk.is_empty() {
            let (ns1, ns2, nlen) = accumulate_chunk_scalar_raw(s1, s2, len, chunk);
            s1 = ns1;
            s2 = ns2;
            len = nlen;
        }

        (s1, s2, len)
    }

    #[inline]
    #[target_feature(enable = "sse2")]
    unsafe fn sum_block(block: __m128i, zero: __m128i) -> u32 {
        let sad = _mm_sad_epu8(block, zero);
        let mut sums = [0i64; 2];
        _mm_storeu_si128(sums.as_mut_ptr() as *mut __m128i, sad);
        (sums[0] as u64 + sums[1] as u64) as u32
    }

    #[inline]
    #[target_feature(enable = "sse2")]
    unsafe fn prefix_sum(
        block: __m128i,
        zero: __m128i,
        high_weights: __m128i,
        low_weights: __m128i,
    ) -> u32 {
        let high = _mm_unpacklo_epi8(block, zero);
        let low = _mm_unpackhi_epi8(block, zero);

        let weighted_high = _mm_mullo_epi16(high, high_weights);
        let weighted_low = _mm_mullo_epi16(low, low_weights);

        let mut buf = [0u16; 8];
        _mm_storeu_si128(buf.as_mut_ptr() as *mut __m128i, weighted_high);
        let mut sum = buf.iter().fold(0u32, |acc, &value| acc + u32::from(value));
        _mm_storeu_si128(buf.as_mut_ptr() as *mut __m128i, weighted_low);
        sum += buf.iter().fold(0u32, |acc, &value| acc + u32::from(value));
        sum
    }

    #[cfg(test)]
    pub(crate) fn accumulate_chunk_sse2_for_tests(
        s1: u32,
        s2: u32,
        len: usize,
        chunk: &[u8],
    ) -> (u32, u32, usize) {
        unsafe { accumulate_chunk_sse2(s1, s2, len, chunk) }
    }
}

#[cfg(target_arch = "aarch64")]
#[allow(unsafe_code)]
pub(crate) mod neon {
    #![allow(unsafe_op_in_unsafe_fn)]
    use super::accumulate_chunk_scalar_raw;
    use core::arch::aarch64::{
        uint16x8_t, vaddvq_u16, vget_high_u8, vget_low_u8, vld1q_u8, vld1q_u16, vmovl_u8, vmulq_u16,
    };

    const BLOCK_LEN: usize = 16;
    const HIGH_WEIGHTS: [u16; 8] = [16, 15, 14, 13, 12, 11, 10, 9];
    const LOW_WEIGHTS: [u16; 8] = [8, 7, 6, 5, 4, 3, 2, 1];

    #[inline]
    pub(super) fn accumulate_chunk(
        s1: u32,
        s2: u32,
        len: usize,
        chunk: &[u8],
    ) -> (u32, u32, usize) {
        unsafe { accumulate_chunk_neon_impl(s1, s2, len, chunk) }
    }

    #[target_feature(enable = "neon")]
    unsafe fn accumulate_chunk_neon_impl(
        mut s1: u32,
        mut s2: u32,
        mut len: usize,
        mut chunk: &[u8],
    ) -> (u32, u32, usize) {
        let high_weights: uint16x8_t = vld1q_u16(HIGH_WEIGHTS.as_ptr());
        let low_weights: uint16x8_t = vld1q_u16(LOW_WEIGHTS.as_ptr());

        while chunk.len() >= BLOCK_LEN {
            let bytes = vld1q_u8(chunk.as_ptr());
            let high = vmovl_u8(vget_low_u8(bytes));
            let low = vmovl_u8(vget_high_u8(bytes));

            let sum_high = vaddvq_u16(high);
            let sum_low = vaddvq_u16(low);
            let block_sum = (sum_high + sum_low) as u32;

            let weighted_high = vmulq_u16(high, high_weights);
            let weighted_low = vmulq_u16(low, low_weights);
            let block_prefix = (vaddvq_u16(weighted_high) + vaddvq_u16(weighted_low)) as u32;

            s2 = s2.wrapping_add(block_prefix);
            s2 = s2.wrapping_add(s1.wrapping_mul(BLOCK_LEN as u32));
            s1 = s1.wrapping_add(block_sum);
            len = len.saturating_add(BLOCK_LEN);
            chunk = &chunk[BLOCK_LEN..];
        }

        if !chunk.is_empty() {
            let (ns1, ns2, nlen) = accumulate_chunk_scalar_raw(s1, s2, len, chunk);
            s1 = ns1;
            s2 = ns2;
            len = nlen;
        }

        (s1, s2, len)
    }

    #[cfg(test)]
    pub(crate) fn accumulate_chunk_neon_for_tests(
        s1: u32,
        s2: u32,
        len: usize,
        chunk: &[u8],
    ) -> (u32, u32, usize) {
        unsafe { accumulate_chunk_neon_impl(s1, s2, len, chunk) }
    }
}
