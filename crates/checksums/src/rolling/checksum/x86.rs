//! x86/x86_64 SIMD-accelerated rolling checksum implementation.
//!
//! This module provides AVX2 and SSE2 implementations of the rolling checksum
//! accumulation used for rsync's delta transfer algorithm.
//!
//! # Upstream Reference
//!
//! - `checksum.c:get_checksum1()` - scalar rolling checksum this accelerates
//! - `match.c:hash_search()` - consumer of rolling checksums during delta detection
//!
//! # Runtime Feature Detection
//!
//! CPU features are detected once at first use via `std::arch::is_x86_feature_detected!`
//! and cached in a `OnceLock`. The dispatch order is:
//!
//! 1. **AVX2** (32 bytes/iteration) - if `is_x86_feature_detected!("avx2")`
//! 2. **SSE2** (16 bytes/iteration) - if `is_x86_feature_detected!("sse2")`
//! 3. **Scalar fallback** - 4-byte unrolled loop
//!
//! Use [`simd_available()`](super::simd_acceleration_available) to query whether
//! AVX2 or SSE2 acceleration is active on the current CPU.
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
//! AVX2 processes 32 bytes per iteration, SSE2 processes 16 bytes per iteration.
//! Remaining bytes fall back to the scalar implementation to ensure correctness
//! for all input sizes.

#![allow(unsafe_code)]
#![allow(unsafe_op_in_unsafe_fn)]

use super::accumulate_chunk_scalar_raw;
#[cfg(target_arch = "x86")]
use core::arch::x86::{
    __m128i, __m256i, _mm_add_epi32, _mm_cmplt_epi8, _mm_loadu_si128, _mm_madd_epi16,
    _mm_set1_epi16, _mm_set_epi16, _mm_setzero_si128, _mm_storeu_si128, _mm_unpackhi_epi8,
    _mm_unpacklo_epi8, _mm256_add_epi32, _mm256_castsi256_si128, _mm256_cvtepi8_epi16,
    _mm256_extracti128_si256, _mm256_loadu_si256, _mm256_madd_epi16, _mm256_set1_epi16,
    _mm256_set_epi16, _mm256_storeu_si256,
};
#[cfg(target_arch = "x86_64")]
use core::arch::x86_64::{
    __m128i, __m256i, _mm_add_epi32, _mm_cmplt_epi8, _mm_loadu_si128, _mm_madd_epi16,
    _mm_set1_epi16, _mm_set_epi16, _mm_setzero_si128, _mm_storeu_si128, _mm_unpackhi_epi8,
    _mm_unpacklo_epi8, _mm256_add_epi32, _mm256_castsi256_si128, _mm256_cvtepi8_epi16,
    _mm256_extracti128_si256, _mm256_loadu_si256, _mm256_madd_epi16, _mm256_set1_epi16,
    _mm256_set_epi16, _mm256_storeu_si256,
};

use std::sync::OnceLock;

const SSE2_BLOCK_LEN: usize = 16;
const AVX2_BLOCK_LEN: usize = 32;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FeatureLevel {
    avx2: bool,
    sse2: bool,
}

static FEATURES: OnceLock<FeatureLevel> = OnceLock::new();

#[inline]
fn cpu_features() -> FeatureLevel {
    *FEATURES.get_or_init(|| FeatureLevel {
        avx2: std::arch::is_x86_feature_detected!("avx2"),
        sse2: std::arch::is_x86_feature_detected!("sse2"),
    })
}

#[inline]
pub(super) fn simd_available() -> bool {
    let features = cpu_features();
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
/// Bytes are sign-extended to match upstream's `schar *buf` interpretation
/// in `checksum.c:get_checksum1()`.
#[target_feature(enable = "sse2")]
unsafe fn accumulate_chunk_sse2(
    mut s1: u32,
    mut s2: u32,
    mut len: usize,
    mut chunk: &[u8],
) -> (u32, u32, usize) {
    let zero = _mm_setzero_si128();
    let ones = _mm_set1_epi16(1);
    // Weights for prefix sum: byte i contributes (16-i) times to s2.
    // high_weights covers bytes 0-7 (weights 16,15,14,...,9)
    // low_weights covers bytes 8-15 (weights 8,7,6,...,1)
    // Note: _mm_set_epi16 takes arguments in reverse order (last element first).
    let high_weights = _mm_set_epi16(9, 10, 11, 12, 13, 14, 15, 16);
    let low_weights = _mm_set_epi16(1, 2, 3, 4, 5, 6, 7, 8);

    while chunk.len() >= SSE2_BLOCK_LEN {
        let block = _mm_loadu_si128(chunk.as_ptr() as *const __m128i);
        // Sign-extend bytes to i16 in two halves: unpack against `cmplt(block,0)`
        // which gives 0xFF where the byte is negative, replicating the sign bit
        // into the high byte of each i16 lane.
        let sign_mask = _mm_cmplt_epi8(block, zero);
        let high_signed = _mm_unpacklo_epi8(block, sign_mask);
        let low_signed = _mm_unpackhi_epi8(block, sign_mask);

        let block_sum = sum_block_signed(high_signed, low_signed, ones);
        let block_prefix = prefix_sum_signed(high_signed, low_signed, high_weights, low_weights);

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

/// Accumulates rolling checksum using AVX2 SIMD instructions.
///
/// Processes 32 bytes at a time. Bytes are sign-extended to match upstream's
/// `schar *buf` interpretation in `checksum.c:get_checksum1()`.
#[target_feature(enable = "avx2")]
unsafe fn accumulate_chunk_avx2(
    mut s1: u32,
    mut s2: u32,
    mut len: usize,
    mut chunk: &[u8],
) -> (u32, u32, usize) {
    let ones = _mm256_set1_epi16(1);
    let first_half_weights = _mm256_set_epi16(
        17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31, 32,
    );
    let second_half_weights =
        _mm256_set_epi16(1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16);

    while chunk.len() >= AVX2_BLOCK_LEN {
        let block = _mm256_loadu_si256(chunk.as_ptr() as *const __m256i);

        let block_sum = sum_block_avx2(block, ones);
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

/// Computes the sum of 32 sign-extended bytes from a 256-bit block.
#[target_feature(enable = "avx2")]
unsafe fn sum_block_avx2(block: __m256i, ones: __m256i) -> u32 {
    let lower_bytes = _mm256_castsi256_si128(block);
    let upper_bytes = _mm256_extracti128_si256(block, 1);

    // Sign-extend the two 16-byte halves to i16 vectors.
    let lower_signed = _mm256_cvtepi8_epi16(lower_bytes);
    let upper_signed = _mm256_cvtepi8_epi16(upper_bytes);

    // Multiply-add against ones to widen-and-sum into i32 lanes (no overflow:
    // each pair of i16 values fits in i32 with margin).
    let lower_pairs = _mm256_madd_epi16(lower_signed, ones);
    let upper_pairs = _mm256_madd_epi16(upper_signed, ones);
    let combined = _mm256_add_epi32(lower_pairs, upper_pairs);

    let mut buffer = [0i32; 8];
    _mm256_storeu_si256(buffer.as_mut_ptr() as *mut __m256i, combined);
    buffer
        .iter()
        .fold(0u32, |acc, &value| acc.wrapping_add(value as u32))
}

#[target_feature(enable = "avx2")]
unsafe fn prefix_sum_avx2(
    block: __m256i,
    first_half_weights: __m256i,
    second_half_weights: __m256i,
) -> u32 {
    let lower_bytes = _mm256_castsi256_si128(block);
    let upper_bytes = _mm256_extracti128_si256(block, 1);

    // Sign-extend bytes to i16 so weighted products carry the upstream
    // `schar *buf` interpretation through to s2.
    let lower_extended = _mm256_cvtepi8_epi16(lower_bytes);
    let upper_extended = _mm256_cvtepi8_epi16(upper_bytes);

    let lower_weighted = _mm256_madd_epi16(lower_extended, first_half_weights);
    let upper_weighted = _mm256_madd_epi16(upper_extended, second_half_weights);

    let combined = _mm256_add_epi32(lower_weighted, upper_weighted);
    let mut buffer = [0i32; 8];
    _mm256_storeu_si256(buffer.as_mut_ptr() as *mut __m256i, combined);

    buffer
        .iter()
        .fold(0u32, |acc, &value| acc.wrapping_add(value as u32))
}

/// Computes the sum of 16 sign-extended bytes already widened to two i16
/// vectors via the SSE2 sign-extension trick.
#[inline]
#[target_feature(enable = "sse2")]
unsafe fn sum_block_signed(high_signed: __m128i, low_signed: __m128i, ones: __m128i) -> u32 {
    let high_pairs = _mm_madd_epi16(high_signed, ones);
    let low_pairs = _mm_madd_epi16(low_signed, ones);
    let combined = _mm_add_epi32(high_pairs, low_pairs);

    let mut buffer = [0i32; 4];
    _mm_storeu_si128(buffer.as_mut_ptr() as *mut __m128i, combined);
    buffer
        .iter()
        .fold(0u32, |acc, &value| acc.wrapping_add(value as u32))
}

/// Computes the weighted prefix sum of 16 sign-extended bytes already widened
/// to two i16 vectors. Uses `_mm_madd_epi16` to multiply-and-pairwise-add into
/// i32 lanes, avoiding any i16 truncation.
#[inline]
#[target_feature(enable = "sse2")]
unsafe fn prefix_sum_signed(
    high_signed: __m128i,
    low_signed: __m128i,
    high_weights: __m128i,
    low_weights: __m128i,
) -> u32 {
    let weighted_high = _mm_madd_epi16(high_signed, high_weights);
    let weighted_low = _mm_madd_epi16(low_signed, low_weights);
    let combined = _mm_add_epi32(weighted_high, weighted_low);

    let mut buffer = [0i32; 4];
    _mm_storeu_si128(buffer.as_mut_ptr() as *mut __m128i, combined);
    buffer
        .iter()
        .fold(0u32, |acc, &value| acc.wrapping_add(value as u32))
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
