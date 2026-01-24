//! SIMD-accelerated parallel MD5 hashing.
//!
//! This crate provides high-throughput MD5 hashing by processing multiple
//! independent inputs in parallel using SIMD instructions.

#![cfg_attr(docsrs, feature(doc_cfg))]

mod scalar;

/// MD5 digest type (16 bytes / 128 bits).
pub type Digest = [u8; 16];

/// Compute MD5 digests for multiple inputs in parallel.
///
/// Returns digests in the same order as inputs.
pub fn digest_batch<T: AsRef<[u8]>>(inputs: &[T]) -> Vec<Digest> {
    inputs.iter().map(|i| scalar::digest(i.as_ref())).collect()
}

/// Compute MD5 digest for a single input.
pub fn digest(input: &[u8]) -> Digest {
    scalar::digest(input)
}
