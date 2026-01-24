//! SIMD-accelerated parallel MD4 and MD5 hashing.
//!
//! This crate provides high-throughput MD4/MD5 hashing by processing multiple
//! independent inputs in parallel using SIMD instructions.
//!
//! # Example
//!
//! ```
//! use md5_simd::{digest, digest_batch, md4};
//!
//! // MD5 hashing
//! let hash = digest(b"hello world");
//!
//! // Batch hash (uses SIMD when available)
//! let inputs = [b"input1".as_slice(), b"input2", b"input3"];
//! let hashes = digest_batch(&inputs);
//!
//! // MD4 hashing
//! let md4_hash = md4::digest(b"hello world");
//! let md4_hashes = md4::digest_batch(&inputs);
//! ```

#![cfg_attr(docsrs, feature(doc_cfg))]

mod dispatcher;
pub mod md4;
mod scalar;
mod simd;

#[cfg(feature = "rayon")]
#[cfg_attr(docsrs, doc(cfg(feature = "rayon")))]
mod rayon_support;

pub use dispatcher::Backend;

#[cfg(feature = "rayon")]
#[cfg_attr(docsrs, doc(cfg(feature = "rayon")))]
pub use rayon_support::{digest_files, ParallelMd5};

/// MD5 digest type (16 bytes / 128 bits).
/// Also used for MD4 (same output size).
pub type Digest = [u8; 16];

/// Compute MD5 digests for multiple inputs in parallel.
///
/// Uses SIMD instructions when available to process multiple hashes
/// simultaneously. Returns digests in the same order as inputs.
pub fn digest_batch<T: AsRef<[u8]>>(inputs: &[T]) -> Vec<Digest> {
    dispatcher::global().digest_batch(inputs)
}

/// Compute MD5 digest for a single input.
pub fn digest(input: &[u8]) -> Digest {
    dispatcher::global().digest(input)
}

/// Get the currently active SIMD backend.
///
/// Useful for logging or diagnostics.
pub fn active_backend() -> Backend {
    dispatcher::global().backend()
}

/// Returns whether SIMD acceleration is available on this platform.
///
/// Returns `true` for any backend other than `Scalar`.
pub fn simd_available() -> bool {
    active_backend() != Backend::Scalar
}

/// Returns the number of parallel lanes used by the current backend.
///
/// - AVX-512: 16 lanes
/// - AVX2: 8 lanes
/// - SSE2/NEON/WASM: 4 lanes
/// - Scalar: 1 lane
pub fn parallel_lanes() -> usize {
    active_backend().lanes()
}
