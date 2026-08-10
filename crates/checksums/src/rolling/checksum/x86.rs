//! x86/x86_64 SIMD-accelerated rolling checksum implementation.
//!
//! This module provides AVX2 and SSE2 implementations of the rolling checksum
//! accumulation used for rsync's delta transfer algorithm.
//!
//! # Upstream Reference
//!
//! - `checksum.c:get_checksum1()` - scalar rolling checksum this accelerates
//! - `match.c:hash_search()` - consumer of rolling checksums during delta detection
//! - `simd-checksum-x86_64.cpp:113-313` - upstream SSSE3/SSE2 vector-register loop
//! - `simd-checksum-x86_64.cpp:338-432` - upstream AVX2 vector-register loop
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
//! Both keep the rolling `s1` and `s2` accumulators resident in vector registers
//! across the full stripe (mirroring upstream `simd-checksum-x86_64.cpp:343-396`)
//! and only extract once at the end of the loop. Remaining bytes fall back to
//! the scalar implementation to ensure correctness for all input sizes.

#![allow(unsafe_code)]
#![allow(unsafe_op_in_unsafe_fn)]

use super::accumulate_chunk_scalar_raw;
#[cfg(target_arch = "x86")]
use core::arch::x86::{
    __m128i, __m256i, _mm_add_epi32, _mm_cmplt_epi8, _mm_cvtsi32_si128, _mm_cvtsi128_si32,
    _mm_loadu_si128, _mm_madd_epi16, _mm_set_epi16, _mm_set1_epi16, _mm_setzero_si128,
    _mm_shuffle_epi32, _mm_slli_epi32, _mm_unpackhi_epi8, _mm_unpacklo_epi8, _mm256_add_epi32,
    _mm256_castsi256_si128, _mm256_cvtepi8_epi16, _mm256_extracti128_si256, _mm256_loadu_si256,
    _mm256_madd_epi16, _mm256_set_epi16, _mm256_set1_epi16,
};
#[cfg(target_arch = "x86_64")]
use core::arch::x86_64::{
    __m128i, __m256i, _mm_add_epi32, _mm_cmplt_epi8, _mm_cvtsi32_si128, _mm_cvtsi128_si32,
    _mm_loadu_si128, _mm_madd_epi16, _mm_set_epi16, _mm_set1_epi16, _mm_setzero_si128,
    _mm_shuffle_epi32, _mm_slli_epi32, _mm_unpackhi_epi8, _mm_unpacklo_epi8, _mm256_add_epi32,
    _mm256_castsi256_si128, _mm256_cvtepi8_epi16, _mm256_extracti128_si256, _mm256_loadu_si256,
    _mm256_madd_epi16, _mm256_set_epi16, _mm256_set1_epi16,
};

use crate::cpu_features::{SimdFeature, feature_allowed};
use std::sync::OnceLock;

const SSE2_BLOCK_LEN: usize = 16;
const AVX2_BLOCK_LEN: usize = 32;
// log2(SSE2_BLOCK_LEN); used by `_mm_slli_epi32(ss1, SSE2_BLOCK_SHIFT)` to
// add `SSE2_BLOCK_LEN * s1` to s2 each iteration.
const SSE2_BLOCK_SHIFT: i32 = 4;
// log2(AVX2_BLOCK_LEN); used by `_mm_slli_epi32(ss1, AVX2_BLOCK_SHIFT)` to
// add `AVX2_BLOCK_LEN * s1` to s2 each iteration.
const AVX2_BLOCK_SHIFT: i32 = 5;

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

/// Returns the AVX2/SSE2 capabilities permitted by both the CLI override and CPUID.
#[inline]
fn effective_features() -> FeatureLevel {
    let detected = cpu_features();
    FeatureLevel {
        avx2: detected.avx2 && feature_allowed(SimdFeature::Avx2),
        sse2: detected.sse2 && feature_allowed(SimdFeature::Sse2),
    }
}

#[inline]
pub(super) fn simd_available() -> bool {
    let features = effective_features();
    features.avx2 || features.sse2
}

#[inline]
pub(super) fn try_accumulate_chunk(
    s1: u32,
    s2: u32,
    len: usize,
    chunk: &[u8],
) -> Option<(u32, u32, usize)> {
    let features = effective_features();

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

/// Horizontally reduces a 4-lane `__m128i` to a `__m128i` whose lane 0 holds
/// the sum of all four input lanes (other lanes are unspecified).
///
/// Two `_mm_shuffle_epi32 + _mm_add_epi32` pairs only - no memory round-trip.
/// Mirrors the upstream `simd-checksum-x86_64.cpp:204` extraction style
/// (collapse via shuffle then read lane 0).
#[inline]
#[target_feature(enable = "sse2")]
unsafe fn fold_lane0_sse2(v: __m128i) -> __m128i {
    // pairs: [v0+v2, v1+v3, v2+v0, v3+v0]
    let shuf = _mm_shuffle_epi32::<0b1110>(v);
    let sum = _mm_add_epi32(v, shuf);
    // lane0 = (v0+v2) + (v1+v3) = v0+v1+v2+v3
    let shuf = _mm_shuffle_epi32::<0b0001>(sum);
    _mm_add_epi32(sum, shuf)
}

/// Extracts the scalar value held in lane 0 of `v` as a `u32`.
#[inline]
#[target_feature(enable = "sse2")]
unsafe fn extract_lane0(v: __m128i) -> u32 {
    _mm_cvtsi128_si32(v) as u32
}

/// Accumulates rolling checksum using SSE2 SIMD instructions.
///
/// Processes 16 bytes at a time using vectorized prefix sum calculation.
/// Bytes are sign-extended to match upstream's `schar *buf` interpretation
/// in `checksum.c:get_checksum1()`.
///
/// The `s1` and `s2` accumulators stay resident in `__m128i` lane 0 across
/// the full stripe (upstream `simd-checksum-x86_64.cpp:223-311` pattern),
/// folding per-iteration `block_sum` / `block_prefix` partial-sum vectors
/// in-register. The final horizontal extraction happens once after the loop.
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

    if chunk.len() >= SSE2_BLOCK_LEN {
        // Keep s1/s2 in vector lane 0 across the stripe. Per upstream
        // `simd-checksum-x86_64.cpp:225-227 _mm_loadu_si128((__m128i_u*)x)`
        // pattern, but using `_mm_cvtsi32_si128` to avoid the staging buffer.
        let mut ss1 = _mm_cvtsi32_si128(s1 as i32);
        let mut ss2 = _mm_cvtsi32_si128(s2 as i32);

        while chunk.len() >= SSE2_BLOCK_LEN {
            let block = _mm_loadu_si128(chunk.as_ptr() as *const __m128i);
            // Sign-extend bytes to i16 in two halves: unpack against `cmplt(block,0)`
            // which gives 0xFF where the byte is negative, replicating the sign bit
            // into the high byte of each i16 lane.
            let sign_mask = _mm_cmplt_epi8(block, zero);
            let high_signed = _mm_unpacklo_epi8(block, sign_mask);
            let low_signed = _mm_unpackhi_epi8(block, sign_mask);

            // s2 += SSE2_BLOCK_LEN * s1, in-vector. Uses ss1 *before* the
            // byte-sum is folded in (matches scalar s2 += BLOCK_LEN * s1_old).
            // Upstream: simd-checksum-x86_64.cpp:255
            // `ss2 = _mm_add_epi32(ss2, _mm_slli_epi32(ss1, 5))`.
            ss2 = _mm_add_epi32(ss2, _mm_slli_epi32::<SSE2_BLOCK_SHIFT>(ss1));

            // Per-iteration partial sums collapsed to lane 0 and folded into
            // ss1 / ss2. The fold is two `_mm_shuffle_epi32 + _mm_add_epi32`
            // pairs, far cheaper than the previous store+scalar-fold.
            let sum_pairs = sum_block_pairs_sse2(high_signed, low_signed, ones);
            let prefix_pairs =
                prefix_sum_pairs_sse2(high_signed, low_signed, high_weights, low_weights);

            ss1 = _mm_add_epi32(ss1, fold_lane0_sse2(sum_pairs));
            ss2 = _mm_add_epi32(ss2, fold_lane0_sse2(prefix_pairs));

            len = len.saturating_add(SSE2_BLOCK_LEN);
            chunk = &chunk[SSE2_BLOCK_LEN..];
        }

        s1 = extract_lane0(ss1);
        s2 = extract_lane0(ss2);
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
///
/// The `s1` and `s2` accumulators stay resident in `__m128i` lane 0 across
/// the full stripe (upstream `simd-checksum-x86_64.cpp:343-396` pattern),
/// folding per-iteration `block_sum` / `block_prefix` partial-sum vectors
/// in-register. The final horizontal extraction happens once after the loop.
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

    if chunk.len() >= AVX2_BLOCK_LEN {
        // Keep s1/s2 in vector lane 0 across the stripe. Mirrors upstream
        // `simd-checksum-x86_64.cpp:343-344 ss1 = _mm_cvtsi32_si128(*ps1)`.
        let mut ss1 = _mm_cvtsi32_si128(s1 as i32);
        let mut ss2 = _mm_cvtsi32_si128(s2 as i32);

        while chunk.len() >= AVX2_BLOCK_LEN {
            let block = _mm256_loadu_si256(chunk.as_ptr() as *const __m256i);

            // s2 += AVX2_BLOCK_LEN * s1, in-vector. Uses ss1 *before* the
            // byte-sum is folded in. Upstream:
            // simd-checksum-x86_64.cpp:374 `ss2 = _mm_add_epi32(ss2, _mm_slli_epi32(ss1, 6))`
            // (upstream stride is 64 -> shift 6; our stride is 32 -> shift 5).
            ss2 = _mm_add_epi32(ss2, _mm_slli_epi32::<AVX2_BLOCK_SHIFT>(ss1));

            // Per-iteration partial sums collapsed to lane 0 and folded into
            // ss1 / ss2 with no scalar round-trip.
            let sum_pairs = sum_block_pairs_avx2(block, ones);
            let prefix_pairs =
                prefix_sum_pairs_avx2(block, first_half_weights, second_half_weights);

            ss1 = _mm_add_epi32(ss1, fold_lane0_sse2(sum_pairs));
            ss2 = _mm_add_epi32(ss2, fold_lane0_sse2(prefix_pairs));

            len = len.saturating_add(AVX2_BLOCK_LEN);
            chunk = &chunk[AVX2_BLOCK_LEN..];
        }

        s1 = extract_lane0(ss1);
        s2 = extract_lane0(ss2);
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

/// Computes the per-pair partial sum of 32 sign-extended bytes, returning a
/// 4-lane `__m128i` whose total across lanes is the byte sum.
#[inline]
#[target_feature(enable = "avx2")]
unsafe fn sum_block_pairs_avx2(block: __m256i, ones: __m256i) -> __m128i {
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

    // Collapse the 256-bit pair-sum into a 128-bit vector; the caller's
    // `fold_lane0_sse2` finishes the horizontal reduction.
    let hi = _mm256_extracti128_si256(combined, 1);
    let lo = _mm256_castsi256_si128(combined);
    _mm_add_epi32(lo, hi)
}

/// Computes the per-pair weighted prefix sum of 32 sign-extended bytes,
/// returning a 4-lane `__m128i` whose total across lanes is the weighted sum.
#[inline]
#[target_feature(enable = "avx2")]
unsafe fn prefix_sum_pairs_avx2(
    block: __m256i,
    first_half_weights: __m256i,
    second_half_weights: __m256i,
) -> __m128i {
    let lower_bytes = _mm256_castsi256_si128(block);
    let upper_bytes = _mm256_extracti128_si256(block, 1);

    // Sign-extend bytes to i16 so weighted products carry the upstream
    // `schar *buf` interpretation through to s2.
    let lower_extended = _mm256_cvtepi8_epi16(lower_bytes);
    let upper_extended = _mm256_cvtepi8_epi16(upper_bytes);

    let lower_weighted = _mm256_madd_epi16(lower_extended, first_half_weights);
    let upper_weighted = _mm256_madd_epi16(upper_extended, second_half_weights);
    let combined = _mm256_add_epi32(lower_weighted, upper_weighted);

    let hi = _mm256_extracti128_si256(combined, 1);
    let lo = _mm256_castsi256_si128(combined);
    _mm_add_epi32(lo, hi)
}

/// Computes the per-pair partial sum of 16 sign-extended bytes (already
/// widened to two i16 vectors), returning a 4-lane `__m128i` whose total
/// across lanes is the byte sum.
#[inline]
#[target_feature(enable = "sse2")]
unsafe fn sum_block_pairs_sse2(
    high_signed: __m128i,
    low_signed: __m128i,
    ones: __m128i,
) -> __m128i {
    let high_pairs = _mm_madd_epi16(high_signed, ones);
    let low_pairs = _mm_madd_epi16(low_signed, ones);
    _mm_add_epi32(high_pairs, low_pairs)
}

/// Computes the per-pair weighted prefix sum of 16 sign-extended bytes already
/// widened to two i16 vectors. Returns a 4-lane `__m128i` whose total across
/// lanes is the weighted sum.
#[inline]
#[target_feature(enable = "sse2")]
unsafe fn prefix_sum_pairs_sse2(
    high_signed: __m128i,
    low_signed: __m128i,
    high_weights: __m128i,
    low_weights: __m128i,
) -> __m128i {
    let weighted_high = _mm_madd_epi16(high_signed, high_weights);
    let weighted_low = _mm_madd_epi16(low_signed, low_weights);
    _mm_add_epi32(weighted_high, weighted_low)
}

#[cfg(test)]
pub(crate) fn accumulate_chunk_sse2_for_tests(
    s1: u32,
    s2: u32,
    len: usize,
    chunk: &[u8],
) -> (u32, u32, usize) {
    // SAFETY: callers gate on `is_x86_feature_detected!("sse2")`. SSE2 is the
    // x86_64 baseline, so this precondition holds on every 64-bit Intel/AMD
    // target the test suite runs on.
    unsafe { accumulate_chunk_sse2(s1, s2, len, chunk) }
}

#[cfg(test)]
pub(crate) fn accumulate_chunk_avx2_for_tests(
    s1: u32,
    s2: u32,
    len: usize,
    chunk: &[u8],
) -> (u32, u32, usize) {
    // SAFETY: callers gate on `is_x86_feature_detected!("avx2")`; the parity
    // test in `rolling/tests/checksum/simd.rs:42` short-circuits and returns
    // early when AVX2 is unavailable on the host.
    unsafe { accumulate_chunk_avx2(s1, s2, len, chunk) }
}
