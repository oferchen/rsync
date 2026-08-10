//! Compression algorithm kind enumeration.

use super::profile::ProtocolCompressionProfile;
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
    /// Zlib/DEFLATE - Mandatory codec for rsync protocol < 30 and the
    /// negotiation fallback for all protocol versions.
    Zlib,
    /// Zstandard - Preferred codec for protocol >= 30 when compiled with
    /// the `zstd` feature.
    #[cfg(feature = "zstd")]
    Zstd,
    /// LZ4 - Fast compression option.
    #[cfg(feature = "lz4")]
    Lz4,
}

impl CompressionAlgorithmKind {
    /// Wire-format name used during protocol negotiation.
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

    /// Upstream-compatible default compression level per algorithm.
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
    /// Protocol < 30 defaults to Zlib (upstream has no vstring negotiation
    /// before protocol 30). Protocol >= 30 defaults to Zstd when the `zstd`
    /// feature is compiled in, falling back to Zlib otherwise.
    ///
    /// Delegates to the central [`ProtocolCompressionProfile`] table.
    ///
    /// # Upstream Reference
    ///
    /// upstream: compat.c:556-563 - `recv_negotiate_str` defaults to `"zlib"`
    /// when `do_negotiated_strings == 0` (protocol < 30 unconditionally).
    /// upstream: compat.c:101-102 - zstd is the first entry in
    /// `valid_compressions_items[]` when `SUPPORT_ZSTD` is defined.
    #[must_use]
    pub const fn for_protocol_version(protocol_version: u8) -> Self {
        ProtocolCompressionProfile::for_protocol(protocol_version).default_kind()
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
