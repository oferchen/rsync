//! SIMD-accelerated parallel MD5 hashing.
//!
//! This crate provides high-throughput MD5 hashing by processing multiple
//! independent inputs in parallel using SIMD instructions.
//!
//! # Example
//!
//! ```
//! use md5_simd::{digest, digest_batch};
//!
//! // Single hash
//! let hash = digest(b"hello world");
//!
//! // Batch hash (uses SIMD when available)
//! let inputs = [b"input1".as_slice(), b"input2", b"input3"];
//! let hashes = digest_batch(&inputs);
//! ```

#![cfg_attr(docsrs, feature(doc_cfg))]

mod dispatcher;
mod scalar;

pub use dispatcher::Backend;

/// MD5 digest type (16 bytes / 128 bits).
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
