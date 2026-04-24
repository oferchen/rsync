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

mod md5_dispatcher;
mod md5_scalar;
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
mod md5_simd;

pub mod md4;

pub use md5_dispatcher::Backend;

/// MD5 digest type (16 bytes / 128 bits).
/// Also used for MD4 (same output size).
pub type Digest = [u8; 16];

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
