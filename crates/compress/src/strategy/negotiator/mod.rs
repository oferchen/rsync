//! Compression algorithm negotiation abstraction.
//!
//! Defines the [`CompressionNegotiator`] trait that decouples algorithm
//! selection logic from the wire-level vstring I/O in the `protocol` crate.
//! This follows the Dependency Inversion principle - callers depend on the
//! trait abstraction rather than concrete negotiation logic.
//!
//! The [`DefaultCompressionNegotiator`] wraps the upstream-compatible
//! selection algorithm from `protocol::negotiation::capabilities::algorithms`,
//! providing the default preference order: zstd > lz4 > zlibx > zlib > none.

mod default;
mod fixed;
mod protocol_aware;
mod trait_def;

#[cfg(test)]
mod tests;

pub use default::DefaultCompressionNegotiator;
pub use fixed::FixedCompressionNegotiator;
pub use protocol_aware::ProtocolAwareCompressionNegotiator;
pub use trait_def::CompressionNegotiator;
