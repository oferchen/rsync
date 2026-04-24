//! Compression algorithm kind enumeration.

use crate::algorithm::CompressionAlgorithm;
use crate::zlib::CompressionLevel;
use std::fmt;

/// Enumeration of supported compression algorithms.
///
/// Identifies the compression algorithm without carrying level or configuration
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
            #[cfg(feature = "lz4")]
            Self::Lz4 => true,
        }
    }

    /// Returns the default compression level for the algorithm.
    #[must_use]
    pub const fn default_level(&self) -> CompressionLevel {
        match self {
            Self::None => CompressionLevel::None,
            Self::Zlib => CompressionLevel::Default,
            #[cfg(feature = "zstd")]
            Self::Zstd => CompressionLevel::Default,
            #[cfg(feature = "lz4")]
            Self::Lz4 => CompressionLevel::Default,
        }
    }

    /// Parses an algorithm from a string name.
    ///
    /// Accepts canonical names and common aliases (case-insensitive).
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
        #[allow(unused_mut)] // REASON: mutated when zstd or lz4 features are enabled
        let mut algorithms = vec![Self::None, Self::Zlib];
        #[cfg(feature = "zstd")]
        algorithms.push(Self::Zstd);
        #[cfg(feature = "lz4")]
        algorithms.push(Self::Lz4);
        algorithms
    }

    /// Returns the default algorithm for a given protocol version.
    ///
    /// Protocol < 36 defaults to Zlib. Protocol >= 36 defaults to Zstd when
    /// available, falling back to Zlib otherwise.
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
