//! ARM NEON SIMD-accelerated rolling checksum implementation.
//!
//! This module provides NEON (Advanced SIMD) implementation of the rolling checksum
//! accumulation used for rsync's delta transfer algorithm on aarch64 platforms.
//!
//! # Upstream Reference
//!
//! - `checksum.c:get_checksum1()` - scalar rolling checksum this accelerates
//! - `match.c:hash_search()` - consumer of rolling checksums during delta detection
//!
//! Upstream rsync has no NEON path; aarch64 falls through to the scalar loop
//! at `target/interop/upstream-src/rsync-3.4.1/checksum.c:285-300`. The
//! vector-register-resident `s1` / `s2` pattern below mirrors the
//! x86 SSSE3/SSE2 loop in `simd-checksum-x86_64.cpp:113-313`, adapted to
//! NEON intrinsics.
//!
//! # Runtime Feature Detection
//!
//! NEON availability is detected once at first use via
//! `std::arch::is_aarch64_feature_detected!("neon")` and cached in a `OnceLock`.
//! On aarch64, NEON is mandatory so this always returns `true`. The detection is
//! still performed for correctness on hypothetical future platforms.
//!
//! Use [`simd_available()`](super::simd_acceleration_available) to query whether
//! NEON acceleration is active on the current CPU.
//!
//! # Safety
//!
//! This module contains `unsafe` code for SIMD operations. Safety is ensured by:
//!
//! - **Runtime CPU feature detection**: NEON availability is checked via
//!   `std::arch::is_aarch64_feature_detected!("neon")` and cached in a `OnceLock`.
//!   SIMD functions are only called after confirming CPU support.
//!
//! - **Memory alignment**: SIMD load operations (`vld1q_u8`, `vld1q_u16`) do not
//!   require aligned memory on ARM, so no alignment constraints are imposed.
//!
//! - **Bounds checking**: The loop condition (`chunk.len() >= BLOCK_LEN`) ensures
//!   sufficient data exists before SIMD processing. Remaining bytes use scalar fallback.
//!
//! - **No data races**: All operations work on local variables or immutable slice references.
//!   The `OnceLock` provides thread-safe lazy initialization of feature detection.
//!
//! # Performance
//!
//! NEON processes 16 bytes per iteration. The `s1` / `s2` accumulators stay
//! resident in `int32x4_t` vectors across the full stripe; per-iteration
//! `block_sum` / `block_prefix` partial sums are folded in-register via
//! `vpaddq_s32`. The final horizontal extraction (`vgetq_lane_s32`) happens
//! once after the loop instead of four times per iteration.

#![allow(unsafe_code)]
#![allow(unsafe_op_in_unsafe_fn)]

use super::accumulate_chunk_scalar_raw;
use crate::cpu_features::{SimdFeature, feature_allowed};
use core::arch::aarch64::{
    int16x8_t, int32x4_t, vaddq_s16, vaddq_s32, vdupq_n_s32, vget_high_s8, vget_low_s8,
    vgetq_lane_s32, vld1q_s16, vld1q_u8, vmovl_s8, vmulq_s16, vpaddlq_s16, vpaddq_s32,
    vreinterpretq_s8_u8, vshlq_n_s32,
};
use std::sync::OnceLock;

const BLOCK_LEN: usize = 16;
// upstream: checksum.c:285 - schar *buf treats bytes as signed [-128,127].
// Weights map byte i to (BLOCK_LEN - i) for the prefix-sum contribution to s2.
const HIGH_WEIGHTS: [i16; 8] = [16, 15, 14, 13, 12, 11, 10, 9];
const LOW_WEIGHTS: [i16; 8] = [8, 7, 6, 5, 4, 3, 2, 1];
// log2(BLOCK_LEN); per-iteration s2 += BLOCK_LEN * s1 becomes
// `vshlq_n_s32(ss1, BLOCK_SHIFT)`.
const BLOCK_SHIFT: i32 = 4;

static NEON_AVAILABLE: OnceLock<bool> = OnceLock::new();

#[inline]
fn neon_available() -> bool {
    *NEON_AVAILABLE.get_or_init(|| std::arch::is_aarch64_feature_detected!("neon"))
}

/// Honours the CLI override on top of CPUID-style feature detection.
#[inline]
fn neon_enabled() -> bool {
    neon_available() && feature_allowed(SimdFeature::Neon)
}

#[inline]
pub(super) fn simd_available() -> bool {
    neon_enabled()
}

#[inline]
pub(super) fn accumulate_chunk(s1: u32, s2: u32, len: usize, chunk: &[u8]) -> (u32, u32, usize) {
    if !neon_enabled() {
        return accumulate_chunk_scalar_raw(s1, s2, len, chunk);
    }

    // SAFETY: NEON is available (checked above). The `accumulate_chunk_neon_impl`
    // function safely handles any chunk length by processing aligned blocks and
    // falling back to scalar for remainders.
    unsafe { accumulate_chunk_neon_impl(s1, s2, len, chunk) }
}

/// Reduces a 4-lane `int32x4_t` so that lane 0 contains the sum of the four
/// original lanes. Other lanes also hold the total (a consequence of using
/// `vpaddq_s32` twice) but are conventionally ignored.
#[inline]
#[target_feature(enable = "neon")]
unsafe fn fold_to_lane0_neon(v: int32x4_t) -> int32x4_t {
    let pairs = vpaddq_s32(v, v); // [v0+v1, v2+v3, v0+v1, v2+v3]
    vpaddq_s32(pairs, pairs) // [total, total, total, total]
}

#[target_feature(enable = "neon")]
unsafe fn accumulate_chunk_neon_impl(
    mut s1: u32,
    mut s2: u32,
    mut len: usize,
    mut chunk: &[u8],
) -> (u32, u32, usize) {
    let high_weights: int16x8_t = vld1q_s16(HIGH_WEIGHTS.as_ptr());
    let low_weights: int16x8_t = vld1q_s16(LOW_WEIGHTS.as_ptr());

    if chunk.len() >= BLOCK_LEN {
        // Keep s1 / s2 in lane 0 of int32x4_t accumulators for the full
        // stripe. Lanes 1-3 are garbage but never read; per-lane operations
        // (`vaddq_s32`, `vshlq_n_s32`) keep lane 0 isolated. Mirrors the
        // x86 SSE2 vector-register pattern in `x86.rs` and upstream
        // `simd-checksum-x86_64.cpp:225-227`.
        let mut ss1: int32x4_t = vdupq_n_s32(s1 as i32);
        let mut ss2: int32x4_t = vdupq_n_s32(s2 as i32);

        while chunk.len() >= BLOCK_LEN {
            // Load as u8x16 then reinterpret as i8x16 - upstream `schar *buf` cast.
            let bytes_signed = vreinterpretq_s8_u8(vld1q_u8(chunk.as_ptr()));
            // Sign-extend each half to int16x8_t so multiplications and weighted
            // sums stay in the signed [-128,127] domain.
            let high = vmovl_s8(vget_low_s8(bytes_signed));
            let low = vmovl_s8(vget_high_s8(bytes_signed));

            // s2 += BLOCK_LEN * s1, per-lane. ss1 carries the s1 value from
            // BEFORE this iteration's byte-sum is folded in, matching the
            // scalar `s2 += BLOCK_LEN * s1_old; s1 += S_k` ordering.
            // Upstream parallel: `simd-checksum-x86_64.cpp:255`
            // `ss2 = _mm_add_epi32(ss2, _mm_slli_epi32(ss1, 5))`.
            ss2 = vaddq_s32(ss2, vshlq_n_s32::<BLOCK_SHIFT>(ss1));

            // Byte sum: combine the two halves (i16 lanes), widen-pair-add to
            // i32 lanes, then fold all four lanes into lane 0 in-register via
            // two `vpaddq_s32` calls (no scalar round-trip).
            let combined16 = vaddq_s16(high, low);
            let sum32 = vpaddlq_s16(combined16);
            ss1 = vaddq_s32(ss1, fold_to_lane0_neon(sum32));

            // Weighted prefix sum: products fit comfortably in i16 (max abs
            // 2048), so vmulq_s16 is safe. Widen via vpaddlq_s16 before
            // horizontal reduction to avoid overflow when many lanes share
            // a sign.
            let weighted_high = vmulq_s16(high, high_weights);
            let weighted_low = vmulq_s16(low, low_weights);
            let combined_w16 = vaddq_s16(weighted_high, weighted_low);
            let weighted32 = vpaddlq_s16(combined_w16);
            ss2 = vaddq_s32(ss2, fold_to_lane0_neon(weighted32));

            len = len.saturating_add(BLOCK_LEN);
            chunk = &chunk[BLOCK_LEN..];
        }

        s1 = vgetq_lane_s32(ss1, 0) as u32;
        s2 = vgetq_lane_s32(ss2, 0) as u32;
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
    if !neon_available() {
        return accumulate_chunk_scalar_raw(s1, s2, len, chunk);
    }

    unsafe { accumulate_chunk_neon_impl(s1, s2, len, chunk) }
}
