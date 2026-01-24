//! AVX2 8-lane parallel MD5 implementation.

#[cfg(target_arch = "x86_64")]
use std::arch::x86_64::*;

use crate::Digest;

/// MD5 state for 8 parallel computations.
#[repr(C, align(32))]
#[allow(dead_code)]
pub struct Md5x8 {
    a: __m256i,
    b: __m256i,
    c: __m256i,
    d: __m256i,
}

/// Compute MD5 digests for up to 8 inputs in parallel using AVX2.
///
/// # Safety
/// Caller must ensure AVX2 is available (use `is_x86_feature_detected!`).
#[target_feature(enable = "avx2")]
pub unsafe fn digest_x8(inputs: &[&[u8]; 8]) -> [Digest; 8] {
    // TODO: Implement SIMD MD5
    // For now, fall back to scalar
    std::array::from_fn(|i| crate::scalar::digest(inputs[i]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn avx2_matches_scalar() {
        if !is_x86_feature_detected!("avx2") {
            eprintln!("AVX2 not available, skipping test");
            return;
        }

        let inputs: [&[u8]; 8] = [
            b"input 0",
            b"input 1",
            b"input 2",
            b"input 3",
            b"input 4",
            b"input 5",
            b"input 6",
            b"input 7",
        ];

        let results = unsafe { digest_x8(&inputs) };

        for (i, input) in inputs.iter().enumerate() {
            let expected = crate::scalar::digest(input);
            assert_eq!(results[i], expected, "Mismatch at lane {i}");
        }
    }
}
