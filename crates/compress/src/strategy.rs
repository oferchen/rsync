//! Strategy pattern for runtime compression algorithm selection.
//!
//! This module provides a Strategy pattern implementation that allows runtime
//! selection of compression algorithms based on protocol version, negotiated
//! capabilities, or explicit configuration.
//!
//! # Overview
//!
//! The Strategy pattern separates the algorithm selection logic from the
//! compression/decompression operations, enabling:
//!
//! - Runtime algorithm selection without code duplication
//! - Protocol version-aware defaults
//! - Clean interface for adding new algorithms
//! - Type-safe level handling
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────────┐
//! │                  CompressionStrategy (trait)                    │
//! │  ┌──────────────────────────────────────────────────────────┐   │
//! │  │ + compress(&self, input: &[u8], output: &mut Vec<u8>)    │   │
//! │  │     -> io::Result<usize>                                 │   │
//! │  │ + decompress(&self, input: &[u8], output: &mut Vec<u8>)  │   │
//! │  │     -> io::Result<usize>                                 │   │
//! │  │ + algorithm_kind(&self) -> CompressionAlgorithmKind      │   │
//! │  │ + algorithm_name(&self) -> &'static str                  │   │
//! │  └──────────────────────────────────────────────────────────┘   │
//! └─────────────────────────────────────────────────────────────────┘
//!                                 ▲
//!                                 │ implements
//!         ┌───────────────────────┼───────────────────────┐
//!         │                       │                       │
//! ┌───────┴───────┐       ┌───────┴───────┐       ┌───────┴───────┐
//! │ ZlibStrategy  │       │ ZstdStrategy  │       │  Lz4Strategy  │
//! └───────────────┘       └───────────────┘       └───────────────┘
//!         │
//! ┌───────┴───────────────┐
//! │ NoCompressionStrategy │
//! └───────────────────────┘
//!
//! ┌─────────────────────────────────────────────────────────────────┐
//! │            CompressionStrategySelector (factory)                │
//! │  ┌──────────────────────────────────────────────────────────┐   │
//! │  │ + for_protocol(version) -> Box<dyn CompressionStrategy>  │   │
//! │  │ + for_algorithm(algo, level) -> Box<dyn ...>             │   │
//! │  │ + negotiate(local, remote) -> Box<dyn ...>               │   │
//! │  └──────────────────────────────────────────────────────────┘   │
//! └─────────────────────────────────────────────────────────────────┘
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

use crate::algorithm::CompressionAlgorithm;
use crate::zlib::{self, CompressionLevel, CountingZlibEncoder};
use std::fmt;
use std::io::{self, Write};

#[cfg(feature = "zstd")]
use crate::zstd::{self, CountingZstdEncoder};

#[cfg(feature = "lz4")]
use crate::lz4::{frame, CountingLz4Encoder};

// ============================================================================
// CompressionAlgorithmKind - Algorithm enumeration
// ============================================================================

/// Enumeration of supported compression algorithms.
///
/// This enum identifies the compression algorithm without carrying level/config
/// data, making it suitable for algorithm selection and comparison.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum CompressionAlgorithmKind {
    /// No compression - data is passed through uncompressed.
    None,
    /// Zlib/DEFLATE - Default for rsync protocol < 36.
    Zlib,
    /// Zstandard - Default for rsync protocol >= 36.
    #[cfg(feature = "zstd")]
    Zstd,
    /// LZ4 - Fast compression option.
    #[cfg(feature = "lz4")]
    Lz4,
}

impl CompressionAlgorithmKind {
    /// Returns the canonical name for the algorithm.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Zlib => "zlib",
            #[cfg(feature = "zstd")]
            Self::Zstd => "zstd",
            #[cfg(feature = "lz4")]
            Self::Lz4 => "lz4",
        }
    }

    /// Returns `true` if this algorithm is available in the current build.
    #[must_use]
    pub const fn is_available(&self) -> bool {
        match self {
            Self::None | Self::Zlib => true,
            #[cfg(feature = "zstd")]
            Self::Zstd => true,
            #[cfg(not(feature = "zstd"))]
            Self::Zstd => false,
            #[cfg(feature = "lz4")]
            Self::Lz4 => true,
            #[cfg(not(feature = "lz4"))]
            Self::Lz4 => false,
        }
    }

    /// Returns the default compression level for the algorithm.
    #[must_use]
    pub const fn default_level(&self) -> CompressionLevel {
        match self {
            Self::None => CompressionLevel::None,
            Self::Zlib | Self::Lz4 | Self::Zstd => CompressionLevel::Default,
        }
    }

    /// Parses an algorithm from a string name.
    ///
    /// Accepts canonical names and common aliases (case-insensitive).
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        match name.to_ascii_lowercase().as_str() {
            "none" => Some(Self::None),
            "zlib" | "zlibx" | "deflate" => Some(Self::Zlib),
            #[cfg(feature = "zstd")]
            "zstd" | "zstandard" => Some(Self::Zstd),
            #[cfg(feature = "lz4")]
            "lz4" => Some(Self::Lz4),
            _ => None,
        }
    }

    /// Returns all supported algorithm kinds in the current build.
    #[must_use]
    pub fn all() -> Vec<Self> {
        let mut algorithms = vec![Self::None, Self::Zlib];
        #[cfg(feature = "zstd")]
        algorithms.push(Self::Zstd);
        #[cfg(feature = "lz4")]
        algorithms.push(Self::Lz4);
        algorithms
    }

    /// Returns the default algorithm for a given protocol version.
    #[must_use]
    pub const fn for_protocol_version(protocol_version: u8) -> Self {
        if protocol_version >= 36 {
            #[cfg(feature = "zstd")]
            return Self::Zstd;
            #[cfg(not(feature = "zstd"))]
            return Self::Zlib;
        } else {
            Self::Zlib
        }
    }
}

impl fmt::Display for CompressionAlgorithmKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

impl From<CompressionAlgorithm> for CompressionAlgorithmKind {
    fn from(algo: CompressionAlgorithm) -> Self {
        match algo {
            CompressionAlgorithm::Zlib => Self::Zlib,
            #[cfg(feature = "lz4")]
            CompressionAlgorithm::Lz4 => Self::Lz4,
            #[cfg(feature = "zstd")]
            CompressionAlgorithm::Zstd => Self::Zstd,
        }
    }
}

// ============================================================================
// CompressionStrategy - Core trait
// ============================================================================

/// Strategy trait for compression operations.
///
/// Implementations provide algorithm-specific compression and decompression
/// while exposing a uniform interface for callers.
///
/// # Thread Safety
///
/// Implementations must be `Send + Sync` to support concurrent usage.
///
/// # Example
///
/// ```
/// use compress::strategy::{CompressionStrategy, ZlibStrategy};
/// use compress::zlib::CompressionLevel;
///
/// let strategy = ZlibStrategy::new(CompressionLevel::Default);
/// let mut compressed = Vec::new();
/// let bytes = strategy.compress(b"hello world", &mut compressed).unwrap();
/// assert!(bytes > 0);
/// ```
pub trait CompressionStrategy: Send + Sync {
    /// Compresses the input data and appends it to the output vector.
    ///
    /// Returns the number of compressed bytes written to `output`.
    fn compress(&self, input: &[u8], output: &mut Vec<u8>) -> io::Result<usize>;

    /// Decompresses the input data and appends it to the output vector.
    ///
    /// Returns the number of decompressed bytes written to `output`.
    fn decompress(&self, input: &[u8], output: &mut Vec<u8>) -> io::Result<usize>;

    /// Returns the algorithm kind for this strategy.
    fn algorithm_kind(&self) -> CompressionAlgorithmKind;

    /// Returns the human-readable algorithm name.
    fn algorithm_name(&self) -> &'static str {
        self.algorithm_kind().name()
    }
}

// ============================================================================
// Concrete Strategy Implementations
// ============================================================================

/// No compression strategy - passes data through unchanged.
///
/// Useful for testing, benchmarking, or when compression is explicitly disabled.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoCompressionStrategy;

impl NoCompressionStrategy {
    /// Creates a new no-compression strategy.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl CompressionStrategy for NoCompressionStrategy {
    fn compress(&self, input: &[u8], output: &mut Vec<u8>) -> io::Result<usize> {
        output.extend_from_slice(input);
        Ok(input.len())
    }

    fn decompress(&self, input: &[u8], output: &mut Vec<u8>) -> io::Result<usize> {
        output.extend_from_slice(input);
        Ok(input.len())
    }

    fn algorithm_kind(&self) -> CompressionAlgorithmKind {
        CompressionAlgorithmKind::None
    }
}

/// Zlib/DEFLATE compression strategy.
///
/// Used by rsync protocol versions < 36 as the default compression algorithm.
#[derive(Clone, Copy, Debug)]
pub struct ZlibStrategy {
    level: CompressionLevel,
}

impl ZlibStrategy {
    /// Creates a new Zlib strategy with the specified compression level.
    #[must_use]
    pub const fn new(level: CompressionLevel) -> Self {
        Self { level }
    }

    /// Creates a Zlib strategy with default compression level.
    #[must_use]
    pub const fn with_default_level() -> Self {
        Self::new(CompressionLevel::Default)
    }
}

impl Default for ZlibStrategy {
    fn default() -> Self {
        Self::with_default_level()
    }
}

impl CompressionStrategy for ZlibStrategy {
    fn compress(&self, input: &[u8], output: &mut Vec<u8>) -> io::Result<usize> {
        let initial_len = output.len();
        let mut encoder = CountingZlibEncoder::with_sink(output, self.level);
        encoder.write_all(input)?;
        let (returned_output, _bytes_written) = encoder.finish_into_inner()?;

        // Calculate actual bytes written
        let final_len = returned_output.len();
        Ok(final_len - initial_len)
    }

    fn decompress(&self, input: &[u8], output: &mut Vec<u8>) -> io::Result<usize> {
        let decompressed = zlib::decompress_to_vec(input)?;
        let len = decompressed.len();
        output.extend_from_slice(&decompressed);
        Ok(len)
    }

    fn algorithm_kind(&self) -> CompressionAlgorithmKind {
        CompressionAlgorithmKind::Zlib
    }
}

/// Zstandard compression strategy.
///
/// Used by rsync protocol versions >= 36 as the default compression algorithm.
/// Only available when the `zstd` feature is enabled.
#[cfg(feature = "zstd")]
#[derive(Clone, Copy, Debug)]
pub struct ZstdStrategy {
    level: CompressionLevel,
}

#[cfg(feature = "zstd")]
impl ZstdStrategy {
    /// Creates a new Zstd strategy with the specified compression level.
    #[must_use]
    pub const fn new(level: CompressionLevel) -> Self {
        Self { level }
    }

    /// Creates a Zstd strategy with default compression level.
    #[must_use]
    pub const fn with_default_level() -> Self {
        Self::new(CompressionLevel::Default)
    }
}

#[cfg(feature = "zstd")]
impl Default for ZstdStrategy {
    fn default() -> Self {
        Self::with_default_level()
    }
}

#[cfg(feature = "zstd")]
impl CompressionStrategy for ZstdStrategy {
    fn compress(&self, input: &[u8], output: &mut Vec<u8>) -> io::Result<usize> {
        let initial_len = output.len();
        let mut encoder = CountingZstdEncoder::with_sink(output, self.level)?;
        encoder.write(input)?;
        let (returned_output, _bytes_written) = encoder.finish_into_inner()?;

        let final_len = returned_output.len();
        Ok(final_len - initial_len)
    }

    fn decompress(&self, input: &[u8], output: &mut Vec<u8>) -> io::Result<usize> {
        let decompressed = zstd::decompress_to_vec(input)?;
        let len = decompressed.len();
        output.extend_from_slice(&decompressed);
        Ok(len)
    }

    fn algorithm_kind(&self) -> CompressionAlgorithmKind {
        CompressionAlgorithmKind::Zstd
    }
}

/// LZ4 compression strategy.
///
/// Provides fast compression with moderate compression ratios.
/// Only available when the `lz4` feature is enabled.
#[cfg(feature = "lz4")]
#[derive(Clone, Copy, Debug)]
pub struct Lz4Strategy {
    level: CompressionLevel,
}

#[cfg(feature = "lz4")]
impl Lz4Strategy {
    /// Creates a new LZ4 strategy with the specified compression level.
    #[must_use]
    pub const fn new(level: CompressionLevel) -> Self {
        Self { level }
    }

    /// Creates an LZ4 strategy with default compression level.
    #[must_use]
    pub const fn with_default_level() -> Self {
        Self::new(CompressionLevel::Default)
    }
}

#[cfg(feature = "lz4")]
impl Default for Lz4Strategy {
    fn default() -> Self {
        Self::with_default_level()
    }
}

#[cfg(feature = "lz4")]
impl CompressionStrategy for Lz4Strategy {
    fn compress(&self, input: &[u8], output: &mut Vec<u8>) -> io::Result<usize> {
        let initial_len = output.len();
        let mut encoder = CountingLz4Encoder::with_sink(output, self.level);
        encoder.write(input)?;
        let (returned_output, _bytes_written) = encoder.finish_into_inner()?;

        let final_len = returned_output.len();
        Ok(final_len - initial_len)
    }

    fn decompress(&self, input: &[u8], output: &mut Vec<u8>) -> io::Result<usize> {
        let decompressed = frame::decompress_to_vec(input)?;
        let len = decompressed.len();
        output.extend_from_slice(&decompressed);
        Ok(len)
    }

    fn algorithm_kind(&self) -> CompressionAlgorithmKind {
        CompressionAlgorithmKind::Lz4
    }
}

// ============================================================================
// CompressionStrategySelector - Factory for strategy selection
// ============================================================================

/// Factory for creating compression strategies based on algorithm selection.
///
/// This selector provides the Strategy pattern's context, allowing runtime
/// selection of compression algorithms based on:
///
/// - Protocol version
/// - Explicit algorithm choice
/// - Negotiated capabilities
pub struct CompressionStrategySelector;

impl CompressionStrategySelector {
    /// Selects the default algorithm for a given protocol version.
    ///
    /// # Protocol Defaults
    ///
    /// - Protocol < 36: Zlib
    /// - Protocol >= 36: Zstd (if available), otherwise Zlib
    ///
    /// # Example
    ///
    /// ```
    /// use compress::strategy::CompressionStrategySelector;
    ///
    /// let strategy = CompressionStrategySelector::for_protocol_version(36);
    /// # #[cfg(feature = "zstd")]
    /// assert_eq!(strategy.algorithm_name(), "zstd");
    /// # #[cfg(not(feature = "zstd"))]
    /// # assert_eq!(strategy.algorithm_name(), "zlib");
    /// ```
    #[must_use]
    pub fn for_protocol_version(protocol_version: u8) -> Box<dyn CompressionStrategy> {
        let kind = CompressionAlgorithmKind::for_protocol_version(protocol_version);
        Self::for_algorithm_kind(kind, kind.default_level())
            .unwrap_or_else(|_| Box::new(ZlibStrategy::with_default_level()))
    }

    /// Creates a strategy for the specified algorithm kind with a given level.
    ///
    /// # Errors
    ///
    /// Returns an error if the algorithm is not available in the current build.
    ///
    /// # Example
    ///
    /// ```
    /// use compress::strategy::{CompressionStrategySelector, CompressionAlgorithmKind};
    /// use compress::zlib::CompressionLevel;
    ///
    /// let strategy = CompressionStrategySelector::for_algorithm(
    ///     CompressionAlgorithmKind::Zlib,
    ///     CompressionLevel::Best,
    /// ).unwrap();
    /// assert_eq!(strategy.algorithm_name(), "zlib");
    /// ```
    pub fn for_algorithm(
        kind: CompressionAlgorithmKind,
        level: CompressionLevel,
    ) -> io::Result<Box<dyn CompressionStrategy>> {
        Self::for_algorithm_kind(kind, level)
    }

    /// Internal method to create strategies with error handling.
    fn for_algorithm_kind(
        kind: CompressionAlgorithmKind,
        level: CompressionLevel,
    ) -> io::Result<Box<dyn CompressionStrategy>> {
        match kind {
            CompressionAlgorithmKind::None => Ok(Box::new(NoCompressionStrategy::new())),
            CompressionAlgorithmKind::Zlib => Ok(Box::new(ZlibStrategy::new(level))),
            #[cfg(feature = "zstd")]
            CompressionAlgorithmKind::Zstd => Ok(Box::new(ZstdStrategy::new(level))),
            #[cfg(not(feature = "zstd"))]
            CompressionAlgorithmKind::Zstd => Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "zstd compression not available (feature not enabled)",
            )),
            #[cfg(feature = "lz4")]
            CompressionAlgorithmKind::Lz4 => Ok(Box::new(Lz4Strategy::new(level))),
            #[cfg(not(feature = "lz4"))]
            CompressionAlgorithmKind::Lz4 => Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "lz4 compression not available (feature not enabled)",
            )),
        }
    }

    /// Negotiates the best compression algorithm from local and remote preferences.
    ///
    /// Selects the first algorithm from `local_algorithms` that also appears
    /// in `remote_algorithms`. If no match is found, returns a no-compression
    /// strategy.
    ///
    /// # Example
    ///
    /// ```
    /// use compress::strategy::{CompressionStrategySelector, CompressionAlgorithmKind};
    /// use compress::zlib::CompressionLevel;
    ///
    /// let local = vec![
    ///     CompressionAlgorithmKind::Zstd,
    ///     CompressionAlgorithmKind::Zlib,
    /// ];
    /// let remote = vec![
    ///     CompressionAlgorithmKind::Zlib,
    ///     CompressionAlgorithmKind::Lz4,
    /// ];
    ///
    /// let strategy = CompressionStrategySelector::negotiate(
    ///     &local,
    ///     &remote,
    ///     CompressionLevel::Default,
    /// );
    /// assert_eq!(strategy.algorithm_name(), "zlib");
    /// ```
    #[must_use]
    pub fn negotiate(
        local_algorithms: &[CompressionAlgorithmKind],
        remote_algorithms: &[CompressionAlgorithmKind],
        level: CompressionLevel,
    ) -> Box<dyn CompressionStrategy> {
        // Find first local algorithm that remote also supports
        for &local_algo in local_algorithms {
            if remote_algorithms.contains(&local_algo) && local_algo.is_available() {
                if let Ok(strategy) = Self::for_algorithm_kind(local_algo, level) {
                    return strategy;
                }
            }
        }

        // No match found - use no compression as fallback
        Box::new(NoCompressionStrategy::new())
    }

    /// Creates a concrete (non-boxed) no-compression strategy.
    #[must_use]
    pub const fn none() -> NoCompressionStrategy {
        NoCompressionStrategy::new()
    }

    /// Creates a concrete Zlib strategy.
    #[must_use]
    pub const fn zlib(level: CompressionLevel) -> ZlibStrategy {
        ZlibStrategy::new(level)
    }

    /// Creates a concrete Zlib strategy with default level.
    #[must_use]
    pub const fn zlib_default() -> ZlibStrategy {
        ZlibStrategy::with_default_level()
    }

    /// Creates a concrete Zstd strategy.
    #[cfg(feature = "zstd")]
    #[must_use]
    pub const fn zstd(level: CompressionLevel) -> ZstdStrategy {
        ZstdStrategy::new(level)
    }

    /// Creates a concrete Zstd strategy with default level.
    #[cfg(feature = "zstd")]
    #[must_use]
    pub const fn zstd_default() -> ZstdStrategy {
        ZstdStrategy::with_default_level()
    }

    /// Creates a concrete LZ4 strategy.
    #[cfg(feature = "lz4")]
    #[must_use]
    pub const fn lz4(level: CompressionLevel) -> Lz4Strategy {
        Lz4Strategy::new(level)
    }

    /// Creates a concrete LZ4 strategy with default level.
    #[cfg(feature = "lz4")]
    #[must_use]
    pub const fn lz4_default() -> Lz4Strategy {
        Lz4Strategy::with_default_level()
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // Test data for compression/decompression
    const TEST_DATA: &[u8] = b"The quick brown fox jumps over the lazy dog. \
                                This is test data for compression algorithms.";
    const COMPRESSIBLE_DATA: &[u8] = b"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\
                                        bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\
                                        cccccccccccccccccccccccccccccccccccccc";

    // ------------------------------------------------------------------------
    // CompressionAlgorithmKind tests
    // ------------------------------------------------------------------------

    #[test]
    fn algorithm_kind_name() {
        assert_eq!(CompressionAlgorithmKind::None.name(), "none");
        assert_eq!(CompressionAlgorithmKind::Zlib.name(), "zlib");
        #[cfg(feature = "zstd")]
        assert_eq!(CompressionAlgorithmKind::Zstd.name(), "zstd");
        #[cfg(feature = "lz4")]
        assert_eq!(CompressionAlgorithmKind::Lz4.name(), "lz4");
    }

    #[test]
    fn algorithm_kind_is_available() {
        assert!(CompressionAlgorithmKind::None.is_available());
        assert!(CompressionAlgorithmKind::Zlib.is_available());
    }

    #[test]
    fn algorithm_kind_default_level() {
        assert_eq!(
            CompressionAlgorithmKind::None.default_level(),
            CompressionLevel::None
        );
        assert_eq!(
            CompressionAlgorithmKind::Zlib.default_level(),
            CompressionLevel::Default
        );
    }

    #[test]
    fn algorithm_kind_from_name() {
        assert_eq!(
            CompressionAlgorithmKind::from_name("none"),
            Some(CompressionAlgorithmKind::None)
        );
        assert_eq!(
            CompressionAlgorithmKind::from_name("zlib"),
            Some(CompressionAlgorithmKind::Zlib)
        );
        assert_eq!(
            CompressionAlgorithmKind::from_name("ZLIB"),
            Some(CompressionAlgorithmKind::Zlib)
        );
        #[cfg(feature = "zstd")]
        assert_eq!(
            CompressionAlgorithmKind::from_name("zstd"),
            Some(CompressionAlgorithmKind::Zstd)
        );
        assert_eq!(CompressionAlgorithmKind::from_name("invalid"), None);
    }

    #[test]
    fn algorithm_kind_all() {
        let all = CompressionAlgorithmKind::all();
        assert!(!all.is_empty());
        assert!(all.contains(&CompressionAlgorithmKind::None));
        assert!(all.contains(&CompressionAlgorithmKind::Zlib));
    }

    #[test]
    fn algorithm_kind_for_protocol_version() {
        assert_eq!(
            CompressionAlgorithmKind::for_protocol_version(30),
            CompressionAlgorithmKind::Zlib
        );
        assert_eq!(
            CompressionAlgorithmKind::for_protocol_version(35),
            CompressionAlgorithmKind::Zlib
        );
        #[cfg(feature = "zstd")]
        assert_eq!(
            CompressionAlgorithmKind::for_protocol_version(36),
            CompressionAlgorithmKind::Zstd
        );
    }

    // ------------------------------------------------------------------------
    // NoCompressionStrategy tests
    // ------------------------------------------------------------------------

    #[test]
    fn no_compression_strategy_roundtrip() {
        let strategy = NoCompressionStrategy::new();
        let mut compressed = Vec::new();
        let mut decompressed = Vec::new();

        let comp_bytes = strategy.compress(TEST_DATA, &mut compressed).unwrap();
        assert_eq!(comp_bytes, TEST_DATA.len());
        assert_eq!(&compressed, TEST_DATA);

        let decomp_bytes = strategy.decompress(&compressed, &mut decompressed).unwrap();
        assert_eq!(decomp_bytes, TEST_DATA.len());
        assert_eq!(&decompressed, TEST_DATA);
    }

    #[test]
    fn no_compression_strategy_algorithm_name() {
        let strategy = NoCompressionStrategy::new();
        assert_eq!(strategy.algorithm_name(), "none");
        assert_eq!(
            strategy.algorithm_kind(),
            CompressionAlgorithmKind::None
        );
    }

    // ------------------------------------------------------------------------
    // ZlibStrategy tests
    // ------------------------------------------------------------------------

    #[test]
    fn zlib_strategy_compress_decompress() {
        let strategy = ZlibStrategy::new(CompressionLevel::Default);
        let mut compressed = Vec::new();
        let mut decompressed = Vec::new();

        let comp_bytes = strategy.compress(TEST_DATA, &mut compressed).unwrap();
        assert!(comp_bytes > 0);
        assert!(comp_bytes < TEST_DATA.len()); // Should compress

        let decomp_bytes = strategy.decompress(&compressed, &mut decompressed).unwrap();
        assert_eq!(decomp_bytes, TEST_DATA.len());
        assert_eq!(&decompressed, TEST_DATA);
    }

    #[test]
    fn zlib_strategy_algorithm_name() {
        let strategy = ZlibStrategy::with_default_level();
        assert_eq!(strategy.algorithm_name(), "zlib");
        assert_eq!(
            strategy.algorithm_kind(),
            CompressionAlgorithmKind::Zlib
        );
    }

    #[test]
    fn zlib_strategy_different_levels() {
        let fast = ZlibStrategy::new(CompressionLevel::Fast);
        let best = ZlibStrategy::new(CompressionLevel::Best);

        let mut fast_out = Vec::new();
        let mut best_out = Vec::new();

        fast.compress(COMPRESSIBLE_DATA, &mut fast_out).unwrap();
        best.compress(COMPRESSIBLE_DATA, &mut best_out).unwrap();

        // Best should generally produce smaller or similar output
        // For small highly compressible data, the difference may be minimal
        assert!(best_out.len() <= fast_out.len() + 5);

        // Both should decompress correctly
        let mut fast_decompressed = Vec::new();
        let mut best_decompressed = Vec::new();
        fast.decompress(&fast_out, &mut fast_decompressed).unwrap();
        best.decompress(&best_out, &mut best_decompressed).unwrap();
        assert_eq!(&fast_decompressed, COMPRESSIBLE_DATA);
        assert_eq!(&best_decompressed, COMPRESSIBLE_DATA);
    }

    // ------------------------------------------------------------------------
    // ZstdStrategy tests
    // ------------------------------------------------------------------------

    #[cfg(feature = "zstd")]
    #[test]
    fn zstd_strategy_compress_decompress() {
        let strategy = ZstdStrategy::new(CompressionLevel::Default);
        let mut compressed = Vec::new();
        let mut decompressed = Vec::new();

        let comp_bytes = strategy.compress(TEST_DATA, &mut compressed).unwrap();
        assert!(comp_bytes > 0);

        let decomp_bytes = strategy.decompress(&compressed, &mut decompressed).unwrap();
        assert_eq!(decomp_bytes, TEST_DATA.len());
        assert_eq!(&decompressed, TEST_DATA);
    }

    #[cfg(feature = "zstd")]
    #[test]
    fn zstd_strategy_algorithm_name() {
        let strategy = ZstdStrategy::with_default_level();
        assert_eq!(strategy.algorithm_name(), "zstd");
        assert_eq!(
            strategy.algorithm_kind(),
            CompressionAlgorithmKind::Zstd
        );
    }

    // ------------------------------------------------------------------------
    // Lz4Strategy tests
    // ------------------------------------------------------------------------

    #[cfg(feature = "lz4")]
    #[test]
    fn lz4_strategy_compress_decompress() {
        let strategy = Lz4Strategy::new(CompressionLevel::Default);
        let mut compressed = Vec::new();
        let mut decompressed = Vec::new();

        let comp_bytes = strategy.compress(TEST_DATA, &mut compressed).unwrap();
        assert!(comp_bytes > 0);

        let decomp_bytes = strategy.decompress(&compressed, &mut decompressed).unwrap();
        assert_eq!(decomp_bytes, TEST_DATA.len());
        assert_eq!(&decompressed, TEST_DATA);
    }

    #[cfg(feature = "lz4")]
    #[test]
    fn lz4_strategy_algorithm_name() {
        let strategy = Lz4Strategy::with_default_level();
        assert_eq!(strategy.algorithm_name(), "lz4");
        assert_eq!(strategy.algorithm_kind(), CompressionAlgorithmKind::Lz4);
    }

    // ------------------------------------------------------------------------
    // CompressionStrategySelector tests
    // ------------------------------------------------------------------------

    #[test]
    fn selector_for_protocol_version_35() {
        let strategy = CompressionStrategySelector::for_protocol_version(35);
        assert_eq!(strategy.algorithm_name(), "zlib");
    }

    #[test]
    fn selector_for_protocol_version_36() {
        let strategy = CompressionStrategySelector::for_protocol_version(36);
        #[cfg(feature = "zstd")]
        assert_eq!(strategy.algorithm_name(), "zstd");
        #[cfg(not(feature = "zstd"))]
        assert_eq!(strategy.algorithm_name(), "zlib");
    }

    #[test]
    fn selector_for_algorithm_zlib() {
        let strategy = CompressionStrategySelector::for_algorithm(
            CompressionAlgorithmKind::Zlib,
            CompressionLevel::Best,
        )
        .unwrap();
        assert_eq!(strategy.algorithm_name(), "zlib");
    }

    #[test]
    fn selector_for_algorithm_none() {
        let strategy = CompressionStrategySelector::for_algorithm(
            CompressionAlgorithmKind::None,
            CompressionLevel::None,
        )
        .unwrap();
        assert_eq!(strategy.algorithm_name(), "none");
    }

    #[test]
    fn selector_negotiate_finds_common_algorithm() {
        let local = vec![
            CompressionAlgorithmKind::Zstd,
            CompressionAlgorithmKind::Zlib,
            CompressionAlgorithmKind::None,
        ];
        let remote = vec![
            CompressionAlgorithmKind::Lz4,
            CompressionAlgorithmKind::Zlib,
        ];

        let strategy =
            CompressionStrategySelector::negotiate(&local, &remote, CompressionLevel::Default);
        assert_eq!(strategy.algorithm_name(), "zlib");
    }

    #[test]
    fn selector_negotiate_no_match_returns_none() {
        let local = vec![CompressionAlgorithmKind::Zlib];
        let remote = vec![CompressionAlgorithmKind::None];

        let strategy =
            CompressionStrategySelector::negotiate(&local, &remote, CompressionLevel::Default);
        // Should still work - None is always available
        assert_eq!(strategy.algorithm_name(), "none");
    }

    #[test]
    fn selector_concrete_factories() {
        let none = CompressionStrategySelector::none();
        assert_eq!(none.algorithm_name(), "none");

        let zlib = CompressionStrategySelector::zlib_default();
        assert_eq!(zlib.algorithm_name(), "zlib");

        #[cfg(feature = "zstd")]
        {
            let zstd = CompressionStrategySelector::zstd_default();
            assert_eq!(zstd.algorithm_name(), "zstd");
        }

        #[cfg(feature = "lz4")]
        {
            let lz4 = CompressionStrategySelector::lz4_default();
            assert_eq!(lz4.algorithm_name(), "lz4");
        }
    }

    // ------------------------------------------------------------------------
    // Strategy trait object tests
    // ------------------------------------------------------------------------

    #[test]
    fn strategies_are_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}

        assert_send_sync::<NoCompressionStrategy>();
        assert_send_sync::<ZlibStrategy>();
        #[cfg(feature = "zstd")]
        assert_send_sync::<ZstdStrategy>();
        #[cfg(feature = "lz4")]
        assert_send_sync::<Lz4Strategy>();
    }

    #[test]
    fn boxed_strategy_works() {
        let strategies: Vec<Box<dyn CompressionStrategy>> = vec![
            Box::new(NoCompressionStrategy::new()),
            Box::new(ZlibStrategy::with_default_level()),
        ];

        for strategy in &strategies {
            let mut compressed = Vec::new();
            let mut decompressed = Vec::new();

            strategy.compress(TEST_DATA, &mut compressed).unwrap();
            strategy.decompress(&compressed, &mut decompressed).unwrap();
            assert_eq!(&decompressed, TEST_DATA);
        }
    }

    #[test]
    fn empty_input_produces_valid_output() {
        let strategy = ZlibStrategy::with_default_level();
        let mut compressed = Vec::new();
        let mut decompressed = Vec::new();

        strategy.compress(b"", &mut compressed).unwrap();
        strategy.decompress(&compressed, &mut decompressed).unwrap();
        assert_eq!(&decompressed, b"");
    }

    #[test]
    fn consistent_results_across_calls() {
        let strategy = ZlibStrategy::new(CompressionLevel::Default);
        let mut out1 = Vec::new();
        let mut out2 = Vec::new();

        strategy.compress(TEST_DATA, &mut out1).unwrap();
        strategy.compress(TEST_DATA, &mut out2).unwrap();
        assert_eq!(out1, out2);
    }
}
