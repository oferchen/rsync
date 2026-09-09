//! SIMD-accelerated parallel MD4 and MD5 batch hashing.
//!
//! This module provides high-throughput MD4/MD5 hashing by processing multiple
//! independent inputs in parallel using SIMD instructions.
//!
//! # Features
//!
//! - **AVX-512**: 16 parallel lanes (x86_64 with AVX-512F + AVX-512BW)
//! - **AVX2**: 8 parallel lanes (x86_64)
//! - **SSE4.1/SSSE3/SSE2**: 4 parallel lanes (x86_64)
//! - **NEON**: 4 parallel lanes (aarch64)
//! - **WASM SIMD**: 4 parallel lanes (wasm32)
//! - **Scalar**: Fallback for other platforms

#![cfg_attr(docsrs, feature(doc_cfg))]

pub(crate) mod md5_dispatcher;
mod md5_scalar;
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
mod md5_simd;

/// MD4 hashing implementations with optional SIMD batch acceleration.
pub mod md4;

pub use md5_dispatcher::Backend;

/// MD5 digest type (16 bytes / 128 bits).
/// Also used for MD4 (same output size).
pub type Digest = [u8; 16];

/// Largest per-input length any SIMD batch backend will hash in-lane.
///
/// Every backend pads each lane into a freshly allocated 64-byte-block
/// multiple, so the transient allocation is bounded by lanes x this cap
/// (16 MiB on the widest, AVX-512 16-lane path). Batches whose longest input
/// exceeds the cap fall back to the scalar digest instead.
///
/// This is the single owner for the bound: MD4 and MD5, every architecture.
/// Upstream rsync has no multi-buffer MD4/MD5, so there is no upstream
/// counterpart to mirror - the value is an oc-side allocation-tuning choice.
#[cfg(any(
    target_arch = "x86_64",
    target_arch = "aarch64",
    target_arch = "wasm32"
))]
const MAX_INPUT_SIZE: usize = 1_024 * 1_024;

/// Compute MD5 digests for multiple inputs in parallel.
///
/// Uses SIMD instructions when available to process multiple hashes
/// simultaneously. Returns digests in the same order as inputs.
#[must_use]
pub fn digest_batch<T: AsRef<[u8]>>(inputs: &[T]) -> Vec<Digest> {
    md5_dispatcher::global().digest_batch(inputs)
}

/// Computes an MD5 digest for a single input.
///
/// Uses the global dispatcher's scalar path. Prefer [`digest_batch`] for
/// multiple inputs to benefit from SIMD parallelism.
#[must_use]
#[allow(dead_code)] // REASON: public API exercised by simd_parity_tests
pub fn digest(input: &[u8]) -> Digest {
    md5_dispatcher::global().digest(input)
}

/// Returns the currently active SIMD backend detected at runtime.
///
/// Useful for logging, diagnostics, and SIMD parity tests.
#[must_use]
#[allow(dead_code)] // REASON: public API exercised by simd_parity_tests
pub fn active_backend() -> Backend {
    md5_dispatcher::global().backend()
}

/// Returns whether SIMD acceleration is available for batch MD5 hashing.
///
/// Returns `true` for any backend other than `Scalar`.
#[must_use]
#[allow(dead_code)] // REASON: public API exercised by simd_parity_tests
pub fn simd_available() -> bool {
    active_backend() != Backend::Scalar
}

/// Returns the number of parallel lanes used by the current backend.
///
/// - AVX-512: 16 lanes
/// - AVX2: 8 lanes
/// - SSE2/NEON/WASM: 4 lanes
/// - Scalar: 1 lane
#[must_use]
#[allow(dead_code)] // REASON: public API exercised by simd_parity_tests
pub fn parallel_lanes() -> usize {
    active_backend().lanes()
}

#[cfg(all(
    test,
    any(
        target_arch = "x86_64",
        target_arch = "aarch64",
        target_arch = "wasm32"
    )
))]
mod max_input_size_tests {
    use super::{MAX_INPUT_SIZE, digest_batch};

    /// The cap exists to bound the transient padding allocation, so its value
    /// must stay compatible with how the backends allocate: each lane is padded
    /// to a whole 64-byte MD4/MD5 block, and the widest backend runs 16 lanes.
    /// Changing the value changes the worst-case burst these hot paths take.
    #[test]
    fn cap_bounds_the_worst_case_padded_allocation() {
        assert_eq!(MAX_INPUT_SIZE, 1_024 * 1_024);
        assert_eq!(
            MAX_INPUT_SIZE % 64,
            0,
            "cap must be a whole number of 64-byte hash blocks"
        );
        assert_eq!(
            MAX_INPUT_SIZE * 16,
            16 * 1_024 * 1_024,
            "AVX-512 runs 16 lanes, so the worst-case burst is 16x the cap"
        );
    }

    /// Both sides of the cap must produce identical digests: below it the SIMD
    /// kernel runs, above it every backend bails to the scalar reference. A
    /// backend that read a different bound than the scalar oracle assumes would
    /// still have to agree here, which is what makes the shared owner safe.
    #[test]
    fn digests_agree_with_scalar_on_both_sides_of_the_cap() {
        for len in [MAX_INPUT_SIZE, MAX_INPUT_SIZE + 1] {
            let long = vec![0xA5u8; len];
            let inputs: [&[u8]; 2] = [&long, b"short"];

            let md5_batch = digest_batch(&inputs);
            let md4_batch = super::md4::digest_batch(&inputs);

            for (i, input) in inputs.iter().enumerate() {
                assert_eq!(
                    md5_batch[i],
                    super::md5_scalar::digest(input),
                    "md5 lane {i} diverged at len {len}"
                );
                assert_eq!(
                    md4_batch[i],
                    super::md4::scalar::digest(input),
                    "md4 lane {i} diverged at len {len}"
                );
            }
        }
    }
}
