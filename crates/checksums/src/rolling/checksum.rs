use std::io::{self, IoSlice, Read};

use super::digest::RollingDigest;
use super::error::RollingError;

/// Rolling checksum used by rsync for weak block matching (often called `rsum`).
///
/// Mirrors upstream rsync's Adler-32 style weak checksum: `s1` accumulates the byte sum,
/// `s2` accumulates prefix sums, both truncated to 16 bits.
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

        for slice in buffers {
            (s1, s2, len) = accumulate_chunk_dispatch(s1, s2, len, slice.as_ref());
        }

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

    /// Convenience wrapper that allocates a stack buffer.
    pub fn update_reader<R: Read>(&mut self, reader: &mut R) -> io::Result<u64> {
        let mut buffer = [0u8; Self::DEFAULT_READER_BUFFER_LEN];
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

        for (&out_b, &in_b) in outgoing.iter().zip(incoming.iter()) {
            let out = u32::from(out_b);
            let inn = u32::from(in_b);

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
    fn from(digest: RollingDigest) -> Self {
        Self::from_digest(digest)
    }
}

impl From<RollingChecksum> for RollingDigest {
    fn from(checksum: RollingChecksum) -> Self {
        checksum.digest()
    }
}

impl From<&RollingChecksum> for RollingDigest {
    fn from(checksum: &RollingChecksum) -> Self {
        checksum.digest()
    }
}

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
#[inline]
fn accumulate_chunk_arch(s1: u32, s2: u32, len: usize, chunk: &[u8]) -> Option<(u32, u32, usize)> {
    #[cfg(target_arch = "aarch64")]
    {
        return Some(neon::accumulate_chunk(s1, s2, len, chunk));
    }

    #[cfg(target_arch = "x86_64")]
    {
        if let Some(result) = x86::try_accumulate_chunk(s1, s2, len, chunk) {
            return Some(result);
        }
    }

    None
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
#[allow(unsafe_op_in_unsafe_fn)]
pub(crate) mod x86 {
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
        let mut sum = buf.iter().fold(0u32, |acc, &v| acc + u32::from(v));
        _mm_storeu_si128(buf.as_mut_ptr() as *mut __m128i, weighted_low);
        sum += buf.iter().fold(0u32, |acc, &v| acc + u32::from(v));
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
#[allow(unsafe_op_in_unsafe_fn)]
pub(crate) mod neon {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_reader_buffer_is_rejected() {
        let mut c = RollingChecksum::new();
        let mut rdr = &b""[..];
        let mut buf: [u8; 0] = [];
        let err = c.update_reader_with_buffer(&mut rdr, &mut buf).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }
}
