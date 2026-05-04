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
//! NEON processes 16 bytes per iteration using vector multiply-accumulate operations.
//! Bytes are reinterpreted as `int8x16_t` and sign-extended via `vmovl_s8` so the
//! accumulators carry the same -128..127 contribution as upstream's `schar *buf`.
//! The weighted sum uses `vmulq_s16` (safe, max product magnitude 2048 fits in i16)
//! and `vaddlvq_s16` for widening reduction into i32 to avoid horizontal overflow.

#![allow(unsafe_code)]
#![allow(unsafe_op_in_unsafe_fn)]

use super::accumulate_chunk_scalar_raw;
use core::arch::aarch64::{
    int16x8_t, vaddlvq_s16, vget_high_s8, vget_low_s8, vld1q_s16, vld1q_u8, vmovl_s8, vmulq_s16,
    vreinterpretq_s8_u8,
};
use std::sync::OnceLock;

const BLOCK_LEN: usize = 16;
// upstream: checksum.c:285 - schar *buf treats bytes as signed [-128,127].
// Weights map byte i to (BLOCK_LEN - i) for the prefix-sum contribution to s2.
const HIGH_WEIGHTS: [i16; 8] = [16, 15, 14, 13, 12, 11, 10, 9];
const LOW_WEIGHTS: [i16; 8] = [8, 7, 6, 5, 4, 3, 2, 1];

static NEON_AVAILABLE: OnceLock<bool> = OnceLock::new();

#[inline]
fn neon_available() -> bool {
    *NEON_AVAILABLE.get_or_init(|| std::arch::is_aarch64_feature_detected!("neon"))
}

#[inline]
pub(super) fn simd_available() -> bool {
    neon_available()
}

#[inline]
pub(super) fn accumulate_chunk(s1: u32, s2: u32, len: usize, chunk: &[u8]) -> (u32, u32, usize) {
    if !neon_available() {
        return accumulate_chunk_scalar_raw(s1, s2, len, chunk);
    }

    // SAFETY: NEON is available (checked above). The `accumulate_chunk_neon_impl`
    // function safely handles any chunk length by processing aligned blocks and
    // falling back to scalar for remainders.
    unsafe { accumulate_chunk_neon_impl(s1, s2, len, chunk) }
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

    while chunk.len() >= BLOCK_LEN {
        // Load as u8x16 then reinterpret as i8x16 - upstream `schar *buf` cast.
        let bytes_signed = vreinterpretq_s8_u8(vld1q_u8(chunk.as_ptr()));
        // Sign-extend each half to int16x8_t so multiplications and weighted
        // sums stay in the signed [-128,127] domain.
        let high = vmovl_s8(vget_low_s8(bytes_signed));
        let low = vmovl_s8(vget_high_s8(bytes_signed));

        // vaddlvq_s16 widens the reduction to i32, avoiding the i16 overflow
        // risk inherent in vaddvq_s16 when many lanes share a sign.
        let sum_high = vaddlvq_s16(high);
        let sum_low = vaddlvq_s16(low);
        let block_sum = sum_high.wrapping_add(sum_low) as u32;

        // vmulq_s16 truncates to i16, but products are in [-2048, 2032]
        // which fits with margin.
        let weighted_high = vmulq_s16(high, high_weights);
        let weighted_low = vmulq_s16(low, low_weights);
        let block_prefix =
            vaddlvq_s16(weighted_high).wrapping_add(vaddlvq_s16(weighted_low)) as u32;

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
    if !neon_available() {
        return accumulate_chunk_scalar_raw(s1, s2, len, chunk);
    }

    unsafe { accumulate_chunk_neon_impl(s1, s2, len, chunk) }
}
