//! SIMD-accelerated zero-byte detection for sparse file writing.
//!
//! Provides hardware-accelerated primitives for identifying zero runs in data
//! buffers - the core operation for sparse file hole detection. On supported
//! platforms, this module uses SIMD instructions to scan 32 bytes (AVX2) or
//! 16 bytes (SSE2/NEON) per instruction cycle, with a scalar fallback for
//! other architectures.
//!
//! # Runtime Feature Detection
//!
//! CPU features are detected once at first use and cached in a `OnceLock`.
//! The dispatch order is:
//!
//! - **x86_64**: AVX2 (32 bytes/iter) > SSE2 (16 bytes/iter) > scalar
//! - **aarch64**: NEON (16 bytes/iter) > scalar
//! - **other**: scalar (16 bytes/iter via `u128`)
//!
//! # Public API
//!
//! - [`find_first_nonzero`] - Returns index of the first non-zero byte
//! - [`is_all_zeros`] - Fast check whether an entire buffer is zero
//!
//! # Safety
//!
//! This module contains `unsafe` code for SIMD operations. Safety is ensured by:
//!
//! - **Runtime CPU feature detection**: All SIMD paths are guarded by
//!   `is_x86_feature_detected!` / `is_aarch64_feature_detected!` checks cached
//!   in a `OnceLock`. SIMD functions are only called after confirming support.
//! - **Unaligned loads**: All SIMD load operations use unaligned variants
//!   (`_mm_loadu_si128`, `_mm256_loadu_si256`, `vld1q_u8`), imposing no
//!   alignment requirements on input data.
//! - **Bounds checking**: Loop conditions ensure sufficient data before SIMD
//!   processing. Remainder bytes fall through to the scalar path.
//! - **No data races**: All operations work on local variables or immutable
//!   slice references.

use std::sync::OnceLock;

/// Returns the index of the first non-zero byte in `buf`, or `buf.len()` if
/// all bytes are zero.
///
/// This is the core primitive for sparse file detection - it identifies where
/// the leading zero run ends so the caller can seek past it rather than writing
/// zeros to disk.
///
/// # Examples
///
/// ```
/// use fast_io::zero_detect::find_first_nonzero;
///
/// assert_eq!(find_first_nonzero(&[0, 0, 0, 1, 0]), 3);
/// assert_eq!(find_first_nonzero(&[0; 4096]), 4096);
/// assert_eq!(find_first_nonzero(&[1, 0, 0, 0]), 0);
/// assert_eq!(find_first_nonzero(&[]), 0);
/// ```
#[inline]
pub fn find_first_nonzero(buf: &[u8]) -> usize {
    dispatch()(buf)
}

/// Returns `true` if every byte in `buf` is zero.
///
/// Equivalent to `find_first_nonzero(buf) == buf.len()` but may short-circuit
/// earlier on some implementations.
///
/// # Examples
///
/// ```
/// use fast_io::zero_detect::is_all_zeros;
///
/// assert!(is_all_zeros(&[0; 1024]));
/// assert!(is_all_zeros(&[]));
/// assert!(!is_all_zeros(&[0, 0, 1]));
/// ```
#[inline]
pub fn is_all_zeros(buf: &[u8]) -> bool {
    find_first_nonzero(buf) == buf.len()
}

type FindFn = fn(&[u8]) -> usize;

static DISPATCH: OnceLock<FindFn> = OnceLock::new();

#[inline]
fn dispatch() -> FindFn {
    *DISPATCH.get_or_init(select_impl)
}

fn select_impl() -> FindFn {
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    {
        if is_x86_feature_detected!("avx2") {
            return find_first_nonzero_avx2;
        }
        if is_x86_feature_detected!("sse2") {
            return find_first_nonzero_sse2;
        }
    }

    #[cfg(target_arch = "aarch64")]
    {
        if std::arch::is_aarch64_feature_detected!("neon") {
            return find_first_nonzero_neon;
        }
    }

    find_first_nonzero_scalar
}

/// Scalar fallback - processes 16 bytes per iteration via `u128`.
fn find_first_nonzero_scalar(buf: &[u8]) -> usize {
    let mut offset = 0;
    let mut iter = buf.chunks_exact(16);

    for chunk in &mut iter {
        let word = u128::from_ne_bytes(chunk.try_into().unwrap());
        if word != 0 {
            return offset + chunk.iter().position(|&b| b != 0).unwrap_or(16);
        }
        offset += 16;
    }

    for &b in iter.remainder() {
        if b != 0 {
            return offset;
        }
        offset += 1;
    }

    offset
}

/// AVX2 implementation processing 32 bytes per iteration on x86/x86_64.
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
fn find_first_nonzero_avx2(buf: &[u8]) -> usize {
    // SAFETY: Caller is only reached when AVX2 is detected at runtime.
    unsafe { find_first_nonzero_avx2_inner(buf) }
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
#[target_feature(enable = "avx2")]
#[allow(unsafe_code)]
unsafe fn find_first_nonzero_avx2_inner(buf: &[u8]) -> usize {
    #[cfg(target_arch = "x86")]
    use core::arch::x86::{
        _mm256_cmpeq_epi8, _mm256_loadu_si256, _mm256_movemask_epi8, _mm256_setzero_si256,
    };
    #[cfg(target_arch = "x86_64")]
    use core::arch::x86_64::{
        _mm256_cmpeq_epi8, _mm256_loadu_si256, _mm256_movemask_epi8, _mm256_setzero_si256,
    };

    // SAFETY: All intrinsics here require AVX2, guaranteed by #[target_feature(enable = "avx2")]
    // and the runtime check in the caller. Pointer arithmetic is bounds-checked by the loop guard.
    unsafe {
        let zero = _mm256_setzero_si256();
        let mut offset = 0;
        let len = buf.len();

        while offset + 32 <= len {
            let ptr = buf.as_ptr().add(offset).cast();
            let chunk = _mm256_loadu_si256(ptr);
            let cmp = _mm256_cmpeq_epi8(chunk, zero);
            let mask = _mm256_movemask_epi8(cmp) as u32;

            // mask has bit set for each byte that IS zero.
            // We want the first byte that is NOT zero, i.e. first 0-bit.
            if mask != 0xFFFF_FFFF {
                let first_nonzero = (!mask).trailing_zeros() as usize;
                return offset + first_nonzero;
            }
            offset += 32;
        }

        // Handle remaining bytes with scalar
        offset + find_first_nonzero_scalar(&buf[offset..])
    }
}

/// SSE2 implementation processing 16 bytes per iteration on x86/x86_64.
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
fn find_first_nonzero_sse2(buf: &[u8]) -> usize {
    // SAFETY: Caller is only reached when SSE2 is detected at runtime.
    unsafe { find_first_nonzero_sse2_inner(buf) }
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
#[target_feature(enable = "sse2")]
#[allow(unsafe_code)]
unsafe fn find_first_nonzero_sse2_inner(buf: &[u8]) -> usize {
    #[cfg(target_arch = "x86")]
    use core::arch::x86::{_mm_cmpeq_epi8, _mm_loadu_si128, _mm_movemask_epi8, _mm_setzero_si128};
    #[cfg(target_arch = "x86_64")]
    use core::arch::x86_64::{
        _mm_cmpeq_epi8, _mm_loadu_si128, _mm_movemask_epi8, _mm_setzero_si128,
    };

    // SAFETY: All intrinsics here require SSE2, guaranteed by #[target_feature(enable = "sse2")]
    // and the runtime check in the caller. Pointer arithmetic is bounds-checked by the loop guard.
    unsafe {
        let zero = _mm_setzero_si128();
        let mut offset = 0;
        let len = buf.len();

        while offset + 16 <= len {
            let ptr = buf.as_ptr().add(offset).cast();
            let chunk = _mm_loadu_si128(ptr);
            let cmp = _mm_cmpeq_epi8(chunk, zero);
            let mask = _mm_movemask_epi8(cmp) as u16;

            if mask != 0xFFFF {
                let first_nonzero = (!mask).trailing_zeros() as usize;
                return offset + first_nonzero;
            }
            offset += 16;
        }

        offset + find_first_nonzero_scalar(&buf[offset..])
    }
}

/// NEON implementation processing 16 bytes per iteration on aarch64.
#[cfg(target_arch = "aarch64")]
fn find_first_nonzero_neon(buf: &[u8]) -> usize {
    // SAFETY: Caller is only reached when NEON is detected at runtime.
    unsafe { find_first_nonzero_neon_inner(buf) }
}

#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
#[allow(unsafe_code, unsafe_op_in_unsafe_fn)]
unsafe fn find_first_nonzero_neon_inner(buf: &[u8]) -> usize {
    use core::arch::aarch64::{
        uint8x16_t, vceqq_u8, vdupq_n_u8, vgetq_lane_u64, vld1q_u8, vmaxvq_u8, vreinterpretq_u64_u8,
    };

    let zero_vec: uint8x16_t = vdupq_n_u8(0);
    let mut offset = 0;
    let len = buf.len();

    while offset + 16 <= len {
        let ptr = buf.as_ptr().add(offset);
        let chunk = vld1q_u8(ptr);
        // vmaxvq_u8 returns the maximum byte in the vector - if 0, all zero
        if vmaxvq_u8(chunk) == 0 {
            offset += 16;
            continue;
        }

        // At least one non-zero byte in this 16-byte chunk - find it.
        // Compare each byte with zero: 0xFF where equal, 0x00 where not.
        let cmp = vceqq_u8(chunk, zero_vec);
        // Reinterpret as two u64 lanes and check byte-by-byte
        let cmp_u64 = vreinterpretq_u64_u8(cmp);
        let lo = vgetq_lane_u64(cmp_u64, 0);
        let hi = vgetq_lane_u64(cmp_u64, 1);

        // In the comparison result, 0xFF means zero byte (match), 0x00 means non-zero.
        // We want the first byte that is NOT all-ones (0xFF), i.e. the first 0x00 byte.
        // Invert: now 0x00 = was zero, 0xFF = was non-zero.
        let lo_inv = !lo;
        let hi_inv = !hi;

        if lo_inv != 0 {
            // First non-zero byte is in the low 8 bytes.
            // trailing_zeros on the inverted value gives us the bit position,
            // divide by 8 for byte position.
            return offset + (lo_inv.trailing_zeros() as usize / 8);
        }
        // Must be in the high 8 bytes
        return offset + 8 + (hi_inv.trailing_zeros() as usize / 8);
    }

    offset + find_first_nonzero_scalar(&buf[offset..])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_empty_buffer() {
        assert_eq!(find_first_nonzero(&[]), 0);
        assert!(is_all_zeros(&[]));
    }

    #[test]
    fn zero_single_zero_byte() {
        assert_eq!(find_first_nonzero(&[0]), 1);
        assert!(is_all_zeros(&[0]));
    }

    #[test]
    fn zero_single_nonzero_byte() {
        assert_eq!(find_first_nonzero(&[42]), 0);
        assert!(!is_all_zeros(&[42]));
    }

    #[test]
    fn zero_all_zeros_various_sizes() {
        for size in [1, 2, 7, 15, 16, 17, 31, 32, 33, 63, 64, 128, 4096] {
            let buf = vec![0u8; size];
            assert_eq!(find_first_nonzero(&buf), size, "failed for size {size}");
            assert!(is_all_zeros(&buf), "is_all_zeros failed for size {size}");
        }
    }

    #[test]
    fn zero_nonzero_at_every_position() {
        for size in [16, 32, 48, 64, 100] {
            for pos in 0..size {
                let mut buf = vec![0u8; size];
                buf[pos] = 0xFF;
                assert_eq!(
                    find_first_nonzero(&buf),
                    pos,
                    "failed for size={size}, pos={pos}"
                );
            }
        }
    }

    #[test]
    fn zero_unaligned_lengths() {
        for len in 1..=65 {
            let buf = vec![0u8; len];
            assert_eq!(find_first_nonzero(&buf), len);
        }
    }

    #[test]
    fn zero_nonzero_in_last_byte() {
        let mut buf = vec![0u8; 4096];
        buf[4095] = 1;
        assert_eq!(find_first_nonzero(&buf), 4095);
        assert!(!is_all_zeros(&buf));
    }

    #[test]
    fn zero_all_nonzero() {
        let buf = vec![0xAA; 128];
        assert_eq!(find_first_nonzero(&buf), 0);
    }

    #[test]
    fn zero_parity_scalar_vs_dispatch() {
        let test_cases: Vec<Vec<u8>> = vec![
            vec![0; 0],
            vec![0; 1],
            vec![0; 15],
            vec![0; 16],
            vec![0; 31],
            vec![0; 32],
            vec![0; 33],
            vec![0; 64],
            vec![0; 128],
            vec![0; 4096],
            vec![1; 1],
            vec![1; 32],
            {
                let mut v = vec![0; 100];
                v[50] = 7;
                v
            },
            {
                let mut v = vec![0; 64];
                v[0] = 1;
                v
            },
            {
                let mut v = vec![0; 64];
                v[63] = 1;
                v
            },
            {
                let mut v = vec![0; 64];
                v[31] = 1;
                v
            },
            {
                let mut v = vec![0; 64];
                v[32] = 1;
                v
            },
        ];

        for (i, buf) in test_cases.iter().enumerate() {
            let scalar = find_first_nonzero_scalar(buf);
            let dispatched = find_first_nonzero(buf);
            assert_eq!(
                scalar, dispatched,
                "parity failure at case {i}: scalar={scalar}, dispatch={dispatched}"
            );
        }
    }

    #[test]
    fn zero_parity_all_implementations_explicit() {
        // Test all compiled implementations directly for parity
        let mut buf = vec![0u8; 256];
        for pos in (0..256).step_by(7) {
            buf.fill(0);
            buf[pos] = 0xAB;

            let scalar = find_first_nonzero_scalar(&buf);
            assert_eq!(scalar, pos);

            #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
            {
                if is_x86_feature_detected!("avx2") {
                    let avx2 = find_first_nonzero_avx2(&buf);
                    assert_eq!(avx2, pos, "AVX2 parity failure at pos={pos}");
                }
                if is_x86_feature_detected!("sse2") {
                    let sse2 = find_first_nonzero_sse2(&buf);
                    assert_eq!(sse2, pos, "SSE2 parity failure at pos={pos}");
                }
            }

            #[cfg(target_arch = "aarch64")]
            {
                if std::arch::is_aarch64_feature_detected!("neon") {
                    let neon = find_first_nonzero_neon(&buf);
                    assert_eq!(neon, pos, "NEON parity failure at pos={pos}");
                }
            }
        }
    }

    #[test]
    fn zero_parity_property_random_buffers() {
        // Pseudo-random property test without pulling in proptest
        let mut seed: u64 = 0xDEAD_BEEF_CAFE_BABE;
        for _ in 0..500 {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;

            let len = (seed % 512) as usize + 1;
            let mut buf = vec![0u8; len];

            // Randomly fill some bytes
            let num_nonzero = (seed % 10) as usize;
            let mut s = seed;
            for _ in 0..num_nonzero {
                s ^= s << 13;
                s ^= s >> 7;
                s ^= s << 17;
                let idx = (s % len as u64) as usize;
                buf[idx] = ((s >> 8) & 0xFF) as u8;
                if buf[idx] == 0 {
                    buf[idx] = 1;
                }
            }

            let scalar = find_first_nonzero_scalar(&buf);
            let dispatched = find_first_nonzero(&buf);
            assert_eq!(
                scalar, dispatched,
                "random parity failure: len={len}, expected={scalar}, got={dispatched}"
            );
        }
    }

    #[test]
    fn zero_buffer_smaller_than_simd_width() {
        for len in 1..32 {
            let mut buf = vec![0u8; len];
            assert_eq!(find_first_nonzero(&buf), len);
            buf[len - 1] = 1;
            assert_eq!(find_first_nonzero(&buf), len - 1);
        }
    }

    #[test]
    fn zero_exactly_one_simd_width() {
        // Exactly 16 bytes (SSE2/NEON width)
        let mut buf = [0u8; 16];
        assert_eq!(find_first_nonzero(&buf), 16);
        buf[15] = 1;
        assert_eq!(find_first_nonzero(&buf), 15);

        // Exactly 32 bytes (AVX2 width)
        let mut buf = [0u8; 32];
        assert_eq!(find_first_nonzero(&buf), 32);
        buf[31] = 1;
        assert_eq!(find_first_nonzero(&buf), 31);
    }

    #[test]
    fn zero_large_buffer_performance_sanity() {
        // Ensure large buffers work correctly (regression guard)
        let size = 1024 * 1024; // 1 MiB
        let buf = vec![0u8; size];
        assert_eq!(find_first_nonzero(&buf), size);

        let mut buf = vec![0u8; size];
        buf[size - 1] = 0xFF;
        assert_eq!(find_first_nonzero(&buf), size - 1);
    }
}
