//! Strategy pattern for runtime compression algorithm selection.
//!
//! This module provides a Strategy pattern implementation that allows runtime
//! selection of compression algorithms based on protocol version, negotiated
//! capabilities, or explicit configuration.
//!
//! # Architecture
//!
//! ```text
//! CompressionStrategy (trait)
//!     |-- NoCompressionStrategy   passthrough, no compression
//!     |-- ZlibStrategy            DEFLATE, protocol < 36 default
//!     |-- ZstdStrategy            Zstandard, protocol >= 36 default
//!     |-- Lz4Strategy             LZ4, fast compression
//!
//! CompressionNegotiator (trait)
//!     |-- DefaultCompressionNegotiator       protocol-agnostic algorithm selection
//!     |-- ProtocolAwareCompressionNegotiator  version-gated (proto < 30: zlib only)
//!     |-- FixedCompressionNegotiator          testing/override with predetermined algorithm
//!
//! CompressionStrategySelector (factory)
//!     |-- for_protocol_version()  protocol-aware default selection
//!     |-- for_algorithm()         explicit algorithm choice
//!     |-- negotiate()             local/remote capability matching
//! ```
//!
//! # Example
//!
//! ```
//! use compress::strategy::{
//!     CompressionStrategy, CompressionStrategySelector, CompressionAlgorithmKind,
//! };
//! use compress::zlib::CompressionLevel;
//!
//! // Select algorithm based on protocol version
//! let strategy = CompressionStrategySelector::for_protocol_version(36);
//! let mut compressed = Vec::new();
//! strategy.compress(b"data", &mut compressed).unwrap();
//!
//! // Select algorithm explicitly
//! let zstd_strategy = CompressionStrategySelector::for_algorithm(
//!     CompressionAlgorithmKind::Zstd,
//!     CompressionLevel::Default,
//! ).unwrap();
//! let mut output = Vec::new();
//! zstd_strategy.compress(b"fast compression", &mut output).unwrap();
//! ```
//!
//! # Protocol Version Defaults
//!
//! | Protocol | Default Algorithm |
//! |----------|-------------------|
//! | < 36     | Zlib              |
//! | >= 36    | Zstd              |

mod impls;
mod kind;
/// Compression algorithm negotiation abstraction.
pub mod negotiator;
mod selector;
mod traits;

#[cfg(test)]
mod tests;

#[cfg(feature = "lz4")]
pub use impls::Lz4Strategy;
#[cfg(feature = "zstd")]
pub use impls::ZstdStrategy;
pub use impls::{NoCompressionStrategy, ZlibStrategy};
pub use kind::CompressionAlgorithmKind;
pub use negotiator::{
    CompressionNegotiator, DefaultCompressionNegotiator, FixedCompressionNegotiator,
    ProtocolAwareCompressionNegotiator,
};
pub use selector::CompressionStrategySelector;
pub use traits::CompressionStrategy;
