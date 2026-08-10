//! Strategy pattern for runtime checksum algorithm selection.
//!
//! This module provides a Strategy pattern implementation that allows runtime
//! selection of checksum algorithms based on protocol version, negotiated
//! capabilities, or explicit configuration.
//!
//! # Overview
//!
//! The Strategy pattern separates the algorithm selection logic from the
//! checksum computation, enabling:
//!
//! - Runtime algorithm selection without code duplication
//! - Protocol version-aware defaults
//! - Clean interface for adding new algorithms
//! - Type-safe seed handling
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────────┐
//! │                    ChecksumStrategy (trait)                     │
//! │  ┌──────────────────────────────────────────────────────────┐   │
//! │  │ + compute(&self, data: &[u8]) -> ChecksumDigest          │   │
//! │  │ + compute_into(&self, data: &[u8], out: &mut [u8])       │   │
//! │  │ + digest_len(&self) -> usize                             │   │
//! │  │ + algorithm_name(&self) -> &'static str                  │   │
//! │  └──────────────────────────────────────────────────────────┘   │
//! └─────────────────────────────────────────────────────────────────┘
//!                                 ▲
//!                                 │ implements
//!         ┌───────────────────────┼───────────────────────┐
//!         │                       │                       │
//! ┌───────┴───────┐       ┌───────┴───────┐       ┌───────┴───────┐
//! │  Md4Strategy  │       │  Md5Strategy  │       │ Xxh3Strategy  │
//! └───────────────┘       └───────────────┘       └───────────────┘
//!
//! ┌─────────────────────────────────────────────────────────────────┐
//! │              ChecksumStrategySelector (factory)                 │
//! │  ┌──────────────────────────────────────────────────────────┐   │
//! │  │ + for_protocol(version, seed) -> Box<dyn ChecksumStrategy>│  │
//! │  │ + for_algorithm(algo, seed) -> Box<dyn ChecksumStrategy>  │  │
//! │  └──────────────────────────────────────────────────────────┘   │
//! └─────────────────────────────────────────────────────────────────┘
//! ```
//!
//! # Example
//!
//! ```
//! use checksums::strong::strategy::{
//!     ChecksumStrategy, ChecksumStrategySelector, ChecksumAlgorithmKind,
//! };
//!
//! // Select algorithm based on protocol version
//! let strategy = ChecksumStrategySelector::for_protocol_version(30, 0x12345678);
//! let digest = strategy.compute(b"data");
//! println!("Digest length: {} bytes", digest.len());
//!
//! // Select algorithm explicitly
//! let xxh3_strategy = ChecksumStrategySelector::for_algorithm(
//!     ChecksumAlgorithmKind::Xxh3,
//!     0x12345678,
//! );
//! let xxh3_digest = xxh3_strategy.compute(b"fast hash");
//! ```
//!
//! # Protocol Version Defaults
//!
//! | Protocol | Default Algorithm |
//! |----------|-------------------|
//! | < 30     | MD4               |
//! | >= 30    | MD5               |
//! | >= 31*   | XXH3 (if negotiated) |
//!
//! *XXH3 requires explicit negotiation in protocol 31+

mod digest;
mod impls;
mod kind;
mod seed;
mod selector;
mod trait_def;

#[cfg(test)]
mod tests;

pub use digest::{ChecksumDigest, MAX_DIGEST_LEN};
pub use impls::{
    Md4Strategy, Md5Strategy, Sha1Strategy, Sha256Strategy, Sha512Strategy, Xxh3_128Strategy,
    Xxh3Strategy, Xxh64Strategy,
};
pub use kind::ChecksumAlgorithmKind;
pub use seed::{Md5SeedConfig, SeedConfig};
pub use selector::ChecksumStrategySelector;
pub use trait_def::ChecksumStrategy;
