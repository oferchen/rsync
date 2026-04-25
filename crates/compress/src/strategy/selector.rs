//! Factory for creating compression strategies based on algorithm selection.

use super::{CompressionAlgorithmKind, CompressionStrategy, NoCompressionStrategy, ZlibStrategy};
use crate::zlib::CompressionLevel;
use std::io;

#[cfg(feature = "zstd")]
use super::ZstdStrategy;

#[cfg(feature = "lz4")]
use super::Lz4Strategy;

/// Factory for creating compression strategies based on algorithm selection.
///
/// Provides the Strategy pattern's context, allowing runtime selection of
/// compression algorithms based on protocol version, explicit algorithm choice,
/// or negotiated capabilities.
pub struct CompressionStrategySelector;

impl CompressionStrategySelector {
    /// Selects the default algorithm for a given protocol version.
    ///
    /// Protocol < 30 has no vstring negotiation and always defaults to Zlib.
    /// Protocol >= 30 defaults to Zstd when the feature is compiled in,
    /// falling back to Zlib otherwise.
    ///
    /// Delegates to the central
    /// [`super::ProtocolCompressionProfile`] table.
    ///
    /// # Example
    ///
    /// ```
    /// use compress::strategy::CompressionStrategySelector;
    ///
    /// let strategy = CompressionStrategySelector::for_protocol_version(32);
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

    /// Creates a strategy for the given algorithm kind and compression level.
    fn for_algorithm_kind(
        kind: CompressionAlgorithmKind,
        level: CompressionLevel,
    ) -> io::Result<Box<dyn CompressionStrategy>> {
        match kind {
            CompressionAlgorithmKind::None => Ok(Box::new(NoCompressionStrategy::new())),
            CompressionAlgorithmKind::Zlib => Ok(Box::new(ZlibStrategy::new(level))),
            #[cfg(feature = "zstd")]
            CompressionAlgorithmKind::Zstd => Ok(Box::new(ZstdStrategy::new(level))),
            #[cfg(feature = "lz4")]
            CompressionAlgorithmKind::Lz4 => Ok(Box::new(Lz4Strategy::new(level))),
        }
    }

    /// Negotiates the best compression algorithm from local and remote preferences.
    ///
    /// Selects the first algorithm from `local_algorithms` that also appears
    /// in `remote_algorithms`. Returns a no-compression strategy if no match
    /// is found.
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
        for &local_algo in local_algorithms {
            if remote_algorithms.contains(&local_algo) && local_algo.is_available() {
                if let Ok(strategy) = Self::for_algorithm_kind(local_algo, level) {
                    return strategy;
                }
            }
        }

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
