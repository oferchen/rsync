//! ARM NEON 4-lane parallel MD5 implementation.

use crate::Digest;

/// Compute MD5 digests for up to 4 inputs in parallel using NEON.
///
/// # Safety
/// Caller must ensure NEON is available (always true on aarch64).
#[cfg(target_arch = "aarch64")]
pub unsafe fn digest_x4(inputs: &[&[u8]; 4]) -> [Digest; 4] {
    // TODO: Implement SIMD MD5
    // For now, fall back to scalar
    std::array::from_fn(|i| crate::scalar::digest(inputs[i]))
}
