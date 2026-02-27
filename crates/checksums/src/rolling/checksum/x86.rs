//! x86/x86_64 SIMD-accelerated rolling checksum implementation.
//!
//! This module provides AVX2 and SSE2 implementations of the rolling checksum
//! accumulation used for rsync's delta transfer algorithm.
//!
//! # Safety
//!
//! This module contains `unsafe` code for SIMD operations. Safety is ensured by:
//!
//! - **Runtime CPU feature detection**: All SIMD paths are guarded by
//!   `std::arch::is_x86_feature_detected!` checks cached in a `OnceLock`. The
//!   AVX2 and SSE2 functions are only called after confirming CPU support.
//!
//! - **Memory alignment**: SIMD load operations (`_mm_loadu_si128`, `_mm256_loadu_si256`)
//!   use unaligned variants, so no alignment requirements are imposed on input data.
//!
//! - **Bounds checking**: All slice accesses are bounds-checked before SIMD processing.
//!   The loop conditions (`chunk.len() >= AVX2_BLOCK_LEN`) ensure sufficient data exists.
//!
//! - **No data races**: All operations work on local variables or immutable slice references.
//!   The `OnceLock` for feature detection provides thread-safe initialization.
//!
//! # Performance
//!
//! AVX2 processes 32 bytes per iteration, SSE2 processes 16 bytes. Remaining bytes
//! fall back to the scalar implementation to ensure correctness for all input sizes.

#![allow(unsafe_code)]
#![allow(unsafe_op_in_unsafe_fn)]

use super::accumulate_chunk_scalar_raw;
#[cfg(target_arch = "x86")]
use core::arch::x86::{
    __m128i, __m256i, _mm_add_epi64, _mm_loadu_si128, _mm_mullo_epi16, _mm_sad_epu8, _mm_set_epi16,
    _mm_setzero_si128, _mm_srli_si128, _mm_storeu_si128, _mm_unpackhi_epi8, _mm_unpacklo_epi8,
    _mm256_add_epi32, _mm256_castsi256_si128, _mm256_cvtepu8_epi16, _mm256_extracti128_si256,
    _mm256_loadu_si256, _mm256_madd_epi16, _mm256_sad_epu8, _mm256_set_epi16, _mm256_setzero_si256,
    _mm256_storeu_si256,
};
#[cfg(target_arch = "x86_64")]
use core::arch::x86_64::{
    __m128i, __m256i, __m512i, _mm_add_epi64, _mm_cvtsi128_si64, _mm_loadu_si128, _mm_mullo_epi16,
    _mm_sad_epu8, _mm_set_epi16, _mm_setzero_si128, _mm_srli_si128, _mm_storeu_si128,
    _mm_unpackhi_epi8, _mm_unpacklo_epi8, _mm256_add_epi32, _mm256_castsi256_si128,
    _mm256_cvtepu8_epi16, _mm256_extracti128_si256, _mm256_loadu_si256, _mm256_madd_epi16,
    _mm256_sad_epu8, _mm256_set_epi16, _mm256_setzero_si256, _mm256_storeu_si256, _mm512_add_epi32,
    _mm512_castsi512_si256, _mm512_cvtepu8_epi16, _mm512_extracti64x4_epi64, _mm512_loadu_si512,
    _mm512_madd_epi16, _mm512_sad_epu8, _mm512_set_epi16, _mm512_setzero_si512,
};

use std::sync::OnceLock;

const SSE2_BLOCK_LEN: usize = 16;
const AVX2_BLOCK_LEN: usize = 32;
#[cfg(target_arch = "x86_64")]
const AVX512_BLOCK_LEN: usize = 64;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FeatureLevel {
    #[cfg(target_arch = "x86_64")]
    avx512bw: bool,
    avx2: bool,
    sse2: bool,
}

static FEATURES: OnceLock<FeatureLevel> = OnceLock::new();

#[inline]
fn cpu_features() -> FeatureLevel {
    *FEATURES.get_or_init(|| FeatureLevel {
        #[cfg(target_arch = "x86_64")]
        avx512bw: std::arch::is_x86_feature_detected!("avx512bw"),
        avx2: std::arch::is_x86_feature_detected!("avx2"),
        sse2: std::arch::is_x86_feature_detected!("sse2"),
    })
}

#[inline]
pub(super) fn simd_available() -> bool {
    let features = cpu_features();
    #[cfg(target_arch = "x86_64")]
    if features.avx512bw {
        return true;
    }
    features.avx2 || features.sse2
}

#[inline]
pub(super) fn try_accumulate_chunk(
    s1: u32,
    s2: u32,
    len: usize,
    chunk: &[u8],
) -> Option<(u32, u32, usize)> {
    let features = cpu_features();

    #[cfg(target_arch = "x86_64")]
    if chunk.len() >= AVX512_BLOCK_LEN && features.avx512bw {
        // SAFETY: AVX-512BW is available (checked above) and chunk.len() >= AVX512_BLOCK_LEN.
        return Some(unsafe { accumulate_chunk_avx512(s1, s2, len, chunk) });
    }

    if chunk.len() >= AVX2_BLOCK_LEN && features.avx2 {
        // SAFETY: AVX2 is available (checked above) and chunk.len() >= AVX2_BLOCK_LEN.
        return Some(unsafe { accumulate_chunk_avx2(s1, s2, len, chunk) });
    }

    if chunk.len() >= SSE2_BLOCK_LEN && features.sse2 {
        // SAFETY: SSE2 is available (checked above) and chunk.len() >= SSE2_BLOCK_LEN.
        return Some(unsafe { accumulate_chunk_sse2(s1, s2, len, chunk) });
    }

    None
}

#[cfg(test)]
pub(super) fn load_cpu_features_for_tests() {
    let _ = cpu_features();
}

#[cfg(test)]
pub(super) fn cpu_features_cached_for_tests() -> bool {
    FEATURES.get().is_some()
}

/// Accumulates rolling checksum using SSE2 SIMD instructions.
///
/// Processes 16 bytes at a time using vectorized prefix sum calculation.
#[target_feature(enable = "sse2")]
unsafe fn accumulate_chunk_sse2(
    mut s1: u32,
    mut s2: u32,
    mut len: usize,
    mut chunk: &[u8],
) -> (u32, u32, usize) {
    let zero = _mm_setzero_si128();
    // Weights for prefix sum: byte i contributes (16-i) times to s2.
    // high_weights covers bytes 0-7 (weights 16,15,14,...,9)
    // low_weights covers bytes 8-15 (weights 8,7,6,...,1)
    // Note: _mm_set_epi16 takes arguments in reverse order (last element first).
    let high_weights = _mm_set_epi16(9, 10, 11, 12, 13, 14, 15, 16);
    let low_weights = _mm_set_epi16(1, 2, 3, 4, 5, 6, 7, 8);

    while chunk.len() >= SSE2_BLOCK_LEN {
        let block = _mm_loadu_si128(chunk.as_ptr() as *const __m128i);
        let block_sum = sum_block(block, zero);
        let block_prefix = prefix_sum(block, zero, high_weights, low_weights);

        s2 = s2.wrapping_add(block_prefix);
        s2 = s2.wrapping_add(s1.wrapping_mul(SSE2_BLOCK_LEN as u32));
        s1 = s1.wrapping_add(block_sum);
        len = len.saturating_add(SSE2_BLOCK_LEN);
        chunk = &chunk[SSE2_BLOCK_LEN..];
    }

    if !chunk.is_empty() {
        let (ns1, ns2, nlen) = accumulate_chunk_scalar_raw(s1, s2, len, chunk);
        s1 = ns1;
        s2 = ns2;
        len = nlen;
    }

    (s1, s2, len)
}

#[target_feature(enable = "avx2")]
unsafe fn accumulate_chunk_avx2(
    mut s1: u32,
    mut s2: u32,
    mut len: usize,
    mut chunk: &[u8],
) -> (u32, u32, usize) {
    let zero = _mm256_setzero_si256();
    let first_half_weights = _mm256_set_epi16(
        17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31, 32,
    );
    let second_half_weights =
        _mm256_set_epi16(1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16);

    while chunk.len() >= AVX2_BLOCK_LEN {
        let block = _mm256_loadu_si256(chunk.as_ptr() as *const __m256i);

        let block_sum = sum_block_avx2(block, zero);
        let block_prefix = prefix_sum_avx2(block, first_half_weights, second_half_weights);

        s2 = s2.wrapping_add(block_prefix);
        s2 = s2.wrapping_add(s1.wrapping_mul(AVX2_BLOCK_LEN as u32));
        s1 = s1.wrapping_add(block_sum);
        len = len.saturating_add(AVX2_BLOCK_LEN);
        chunk = &chunk[AVX2_BLOCK_LEN..];
    }

    if chunk.len() >= SSE2_BLOCK_LEN {
        let (ns1, ns2, nlen) = accumulate_chunk_sse2(s1, s2, len, chunk);
        s1 = ns1;
        s2 = ns2;
        len = nlen;
    } else if !chunk.is_empty() {
        let (ns1, ns2, nlen) = accumulate_chunk_scalar_raw(s1, s2, len, chunk);
        s1 = ns1;
        s2 = ns2;
        len = nlen;
    }

    (s1, s2, len)
}

/// Accumulates rolling checksum using AVX-512BW SIMD instructions.
///
/// Processes 64 bytes per iteration — 2x throughput over AVX2. Falls back to
/// AVX2 for 32–63 byte remainders, SSE2 for 16–31, scalar for <16.
///
/// Available on Intel Skylake-X+, Ice Lake+, and AMD Zen 4+.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx512bw")]
unsafe fn accumulate_chunk_avx512(
    mut s1: u32,
    mut s2: u32,
    mut len: usize,
    mut chunk: &[u8],
) -> (u32, u32, usize) {
    let zero = _mm512_setzero_si512();
    // Weights for bytes 0..31 of a 64-byte block: [64,63,...,33]
    // _mm512_set_epi16 takes args in reverse element order (element [31] first).
    let first_half_weights = _mm512_set_epi16(
        33, 34, 35, 36, 37, 38, 39, 40, 41, 42, 43, 44, 45, 46, 47, 48, 49, 50, 51, 52, 53, 54, 55,
        56, 57, 58, 59, 60, 61, 62, 63, 64,
    );
    // Weights for bytes 32..63: [32,31,...,1]
    let second_half_weights = _mm512_set_epi16(
        1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25,
        26, 27, 28, 29, 30, 31, 32,
    );

    while chunk.len() >= AVX512_BLOCK_LEN {
        let block = _mm512_loadu_si512(chunk.as_ptr().cast());

        let block_sum = sum_block_avx512(block, zero);
        let block_prefix = prefix_sum_avx512(block, first_half_weights, second_half_weights);

        s2 = s2.wrapping_add(block_prefix);
        s2 = s2.wrapping_add(s1.wrapping_mul(AVX512_BLOCK_LEN as u32));
        s1 = s1.wrapping_add(block_sum);
        len = len.saturating_add(AVX512_BLOCK_LEN);
        chunk = &chunk[AVX512_BLOCK_LEN..];
    }

    if chunk.len() >= AVX2_BLOCK_LEN {
        let (ns1, ns2, nlen) = accumulate_chunk_avx2(s1, s2, len, chunk);
        s1 = ns1;
        s2 = ns2;
        len = nlen;
    } else if chunk.len() >= SSE2_BLOCK_LEN {
        let (ns1, ns2, nlen) = accumulate_chunk_sse2(s1, s2, len, chunk);
        s1 = ns1;
        s2 = ns2;
        len = nlen;
    } else if !chunk.is_empty() {
        let (ns1, ns2, nlen) = accumulate_chunk_scalar_raw(s1, s2, len, chunk);
        s1 = ns1;
        s2 = ns2;
        len = nlen;
    }

    (s1, s2, len)
}

/// Computes the byte sum of a 512-bit (64-byte) block.
///
/// Uses `_mm512_sad_epu8` which produces 8 partial sums (one per 8-byte lane),
/// then reduces to a single u32 via cascading 128-bit additions.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx512bw")]
unsafe fn sum_block_avx512(block: __m512i, zero: __m512i) -> u32 {
    let sad = _mm512_sad_epu8(block, zero);
    // Extract four __m128i quarters and reduce with existing helper
    let lo256 = _mm512_castsi512_si256(sad);
    let hi256 = _mm512_extracti64x4_epi64(sad, 1);
    let lo128a = _mm256_castsi256_si128(lo256);
    let lo128b = _mm256_extracti128_si256(lo256, 1);
    let hi128a = _mm256_castsi256_si128(hi256);
    let hi128b = _mm256_extracti128_si256(hi256, 1);
    let sum01 = _mm_add_epi64(lo128a, lo128b);
    let sum23 = _mm_add_epi64(hi128a, hi128b);
    let sum = _mm_add_epi64(sum01, sum23);
    horizontal_sum_epi64(sum) as u32
}

/// Computes the weighted prefix sum of a 512-bit (64-byte) block.
///
/// Each byte `i` in [0..63] is weighted by `(64 - i)` to produce the prefix sum
/// contribution. The block is split into two 32-byte halves, extended to 16-bit,
/// multiplied by their respective weight vectors, and accumulated via
/// `_mm512_madd_epi16` (pairwise multiply-add to 32-bit).
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx512bw")]
unsafe fn prefix_sum_avx512(
    block: __m512i,
    first_half_weights: __m512i,
    second_half_weights: __m512i,
) -> u32 {
    let lower_bytes = _mm512_castsi512_si256(block);
    let upper_bytes = _mm512_extracti64x4_epi64(block, 1);

    let lower_extended = _mm512_cvtepu8_epi16(lower_bytes);
    let upper_extended = _mm512_cvtepu8_epi16(upper_bytes);

    let lower_weighted = _mm512_madd_epi16(lower_extended, first_half_weights);
    let upper_weighted = _mm512_madd_epi16(upper_extended, second_half_weights);

    let combined = _mm512_add_epi32(lower_weighted, upper_weighted);
    // Reduce 16 x i32 → single u32 via cascading 256→128-bit reduction
    let lo256 = _mm512_castsi512_si256(combined);
    let hi256 = _mm512_extracti64x4_epi64(combined, 1);
    let sum256 = _mm256_add_epi32(lo256, hi256);
    let mut buffer = [0i32; 8];
    _mm256_storeu_si256(buffer.as_mut_ptr().cast(), sum256);
    buffer
        .iter()
        .fold(0u32, |acc, &value| acc.wrapping_add(value as u32))
}

#[target_feature(enable = "avx2")]
unsafe fn sum_block_avx2(block: __m256i, zero: __m256i) -> u32 {
    let sad = _mm256_sad_epu8(block, zero);
    let lower = _mm256_castsi256_si128(sad);
    let upper = _mm256_extracti128_si256(sad, 1);
    let combined = _mm_add_epi64(lower, upper);
    horizontal_sum_epi64(combined) as u32
}

#[target_feature(enable = "avx2")]
unsafe fn prefix_sum_avx2(
    block: __m256i,
    first_half_weights: __m256i,
    second_half_weights: __m256i,
) -> u32 {
    let lower_bytes = _mm256_castsi256_si128(block);
    let upper_bytes = _mm256_extracti128_si256(block, 1);

    let lower_extended = _mm256_cvtepu8_epi16(lower_bytes);
    let upper_extended = _mm256_cvtepu8_epi16(upper_bytes);

    let lower_weighted = _mm256_madd_epi16(lower_extended, first_half_weights);
    let upper_weighted = _mm256_madd_epi16(upper_extended, second_half_weights);

    let combined = _mm256_add_epi32(lower_weighted, upper_weighted);
    let mut buffer = [0i32; 8];
    _mm256_storeu_si256(buffer.as_mut_ptr() as *mut __m256i, combined);

    buffer
        .iter()
        .fold(0u32, |acc, &value| acc.wrapping_add(value as u32))
}

#[inline]
#[target_feature(enable = "sse2")]
unsafe fn sum_block(block: __m128i, zero: __m128i) -> u32 {
    let sad = _mm_sad_epu8(block, zero);
    horizontal_sum_epi64(sad) as u32
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

#[inline]
#[target_feature(enable = "sse2")]
unsafe fn horizontal_sum_epi64(values: __m128i) -> u64 {
    #[cfg(target_arch = "x86_64")]
    {
        let low = _mm_cvtsi128_si64(values) as u64;
        let high = _mm_cvtsi128_si64(_mm_srli_si128(values, 8)) as u64;
        low.wrapping_add(high)
    }
    #[cfg(target_arch = "x86")]
    {
        let mut buf = [0i64; 2];
        _mm_storeu_si128(buf.as_mut_ptr() as *mut __m128i, values);
        buf[0] as u64 + buf[1] as u64
    }
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

#[cfg(test)]
pub(crate) fn accumulate_chunk_avx2_for_tests(
    s1: u32,
    s2: u32,
    len: usize,
    chunk: &[u8],
) -> (u32, u32, usize) {
    unsafe { accumulate_chunk_avx2(s1, s2, len, chunk) }
}

#[cfg(all(test, target_arch = "x86_64"))]
pub(crate) fn accumulate_chunk_avx512_for_tests(
    s1: u32,
    s2: u32,
    len: usize,
    chunk: &[u8],
) -> (u32, u32, usize) {
    unsafe { accumulate_chunk_avx512(s1, s2, len, chunk) }
}
