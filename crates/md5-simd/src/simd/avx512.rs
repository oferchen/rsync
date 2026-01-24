//! AVX-512 16-lane parallel MD5 implementation.
//!
//! Note: AVX-512 support requires nightly Rust due to unstable target features.

use crate::Digest;

/// Compute MD5 digests for up to 16 inputs in parallel using AVX-512.
///
/// # Safety
/// Caller must ensure AVX-512F and AVX-512BW are available.
///
/// Note: This function currently falls back to scalar until AVX-512 intrinsics
/// are stabilized in Rust.
#[cfg(target_arch = "x86_64")]
pub unsafe fn digest_x16(inputs: &[&[u8]; 16]) -> [Digest; 16] {
    // AVX-512 intrinsics require nightly Rust.
    // Fall back to scalar for now.
    std::array::from_fn(|i| crate::scalar::digest(inputs[i]))
}
