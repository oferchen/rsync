//! ARM NEON SIMD-accelerated rolling checksum implementation.
//!
//! This module provides NEON (Advanced SIMD) implementation of the rolling checksum
//! accumulation used for rsync's delta transfer algorithm on aarch64 platforms.
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
//! The weighted sum computation uses `vmulq_u16` and `vaddvq_u16` for efficient reduction.

#![allow(unsafe_code)]
#![allow(unsafe_op_in_unsafe_fn)]

use super::accumulate_chunk_scalar_raw;
use core::arch::aarch64::{
    uint16x8_t, vaddvq_u16, vget_high_u8, vget_low_u8, vld1q_u8, vld1q_u16, vmovl_u8, vmulq_u16,
};
use std::sync::OnceLock;

const BLOCK_LEN: usize = 16;
const HIGH_WEIGHTS: [u16; 8] = [16, 15, 14, 13, 12, 11, 10, 9];
const LOW_WEIGHTS: [u16; 8] = [8, 7, 6, 5, 4, 3, 2, 1];

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
    if !neon_available() {
        return accumulate_chunk_scalar_raw(s1, s2, len, chunk);
    }

    unsafe { accumulate_chunk_neon_impl(s1, s2, len, chunk) }
}
