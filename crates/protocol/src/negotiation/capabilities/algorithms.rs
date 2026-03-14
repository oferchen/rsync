use std::io;

/// Supported checksum algorithms in preference order.
///
/// This list matches upstream rsync 3.4.1's default order.
/// The client will select the first algorithm in this list that it also supports.
/// Upstream order: xxh128 xxh3 xxh64 md5 md4 sha1 none
pub(super) const SUPPORTED_CHECKSUMS: &[&str] =
    &["xxh128", "xxh3", "xxh64", "md5", "md4", "sha1", "none"];

/// Returns supported compression algorithms in preference order.
///
/// This list is based on upstream rsync 3.4.1's default order, but only includes
/// algorithms that are actually available in this build (feature-gated).
/// The client will select the first algorithm in this list that it also supports.
#[allow(clippy::vec_init_then_push)] // Feature-gated pushes require incremental building
pub(super) fn supported_compressions() -> Vec<&'static str> {
    let mut list = Vec::new();
    #[cfg(feature = "zstd")]
    list.push("zstd");
    #[cfg(feature = "lz4")]
    list.push("lz4");
    // zlibx and zlib are always available (via flate2/miniz_oxide)
    list.push("zlibx");
    list.push("zlib");
    list.push("none");
    list
}

/// Checksum algorithm negotiated between rsync peers.
///
/// Protocol 30+ peers exchange space-separated lists of supported algorithms
/// and each side selects the first mutually supported entry. For protocol
/// versions below 30, [`MD4`](Self::MD4) is always used. The variants are
/// ordered from strongest/newest to weakest/oldest, matching upstream rsync
/// 3.4.1's preference order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum ChecksumAlgorithm {
    /// No checksum - used for directory listings and dry-run modes.
    None,
    /// MD4 checksum - legacy default for protocol versions below 30.
    MD4,
    /// MD5 checksum - default for protocol 30+ when negotiation is unavailable.
    MD5,
    /// SHA-1 checksum - available but rarely preferred over faster alternatives.
    SHA1,
    /// XXHash 64-bit - fast non-cryptographic hash with good distribution.
    XXH64,
    /// XXHash 3 - fastest available hash, preferred for protocol 30+ with negotiation.
    XXH3,
    /// XXHash 128-bit - strongest non-cryptographic hash, preferred when available.
    XXH128,
}

impl ChecksumAlgorithm {
    /// Returns the wire protocol name for this algorithm.
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::None => "none",
            Self::MD4 => "md4",
            Self::MD5 => "md5",
            Self::SHA1 => "sha1",
            Self::XXH64 => "xxh64",
            Self::XXH3 => "xxh3",
            Self::XXH128 => "xxh128",
        }
    }

    /// Parses an algorithm from its wire protocol name.
    ///
    /// Accepts "xxhash" as an alias for XXH64, matching upstream rsync's
    /// `valid_checksums_items` table in `checksum.c` where both "xxh64" and
    /// "xxhash" map to `CSUM_XXH64`.
    pub fn parse(name: &str) -> io::Result<Self> {
        match name {
            "none" => Ok(Self::None),
            "md4" => Ok(Self::MD4),
            "md5" => Ok(Self::MD5),
            "sha1" => Ok(Self::SHA1),
            "xxh" | "xxh64" | "xxhash" => Ok(Self::XXH64),
            "xxh3" => Ok(Self::XXH3),
            "xxh128" => Ok(Self::XXH128),
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unsupported checksum algorithm: {name}"),
            )),
        }
    }
}

/// Compression algorithm negotiated between rsync peers.
///
/// Protocol 30+ peers exchange space-separated lists of supported algorithms
/// and each side selects the first mutually supported entry. For protocol
/// versions below 30, [`Zlib`](Self::Zlib) is the default. Availability of
/// [`LZ4`](Self::LZ4) and [`Zstd`](Self::Zstd) depends on compile-time
/// feature flags (`lz4`, `zstd`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum CompressionAlgorithm {
    /// No compression - data is transferred uncompressed.
    None,
    /// Zlib compression - legacy default for all protocol versions.
    Zlib,
    /// Zlib with matched data excluded - avoids re-compressing block-match data.
    ZlibX,
    /// LZ4 compression - fast compression with lower CPU overhead than zlib.
    LZ4,
    /// Zstandard compression - modern codec with high ratio and fast decompression.
    Zstd,
}

impl CompressionAlgorithm {
    /// Returns the wire protocol name for this algorithm.
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Zlib => "zlib",
            Self::ZlibX => "zlibx",
            Self::LZ4 => "lz4",
            Self::Zstd => "zstd",
        }
    }

    /// Parses an algorithm from its wire protocol name.
    pub fn parse(name: &str) -> io::Result<Self> {
        match name {
            "none" => Ok(Self::None),
            "zlib" => Ok(Self::Zlib),
            "zlibx" => Ok(Self::ZlibX),
            "lz4" => Ok(Self::LZ4),
            "zstd" => Ok(Self::Zstd),
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unsupported compression algorithm: {name}"),
            )),
        }
    }

    /// Converts to the compression crate's algorithm enum.
    ///
    /// Returns `None` if this is the `None` variant (no compression).
    /// Returns an error if the algorithm is not supported in this build.
    ///
    /// # Errors
    ///
    /// Returns an error if the algorithm requires a feature that is not enabled
    /// (e.g., LZ4 or Zstd without the corresponding feature flag).
    pub fn to_compress_algorithm(
        &self,
    ) -> io::Result<Option<compress::algorithm::CompressionAlgorithm>> {
        match self {
            Self::None => Ok(None),
            Self::Zlib | Self::ZlibX => Ok(Some(compress::algorithm::CompressionAlgorithm::Zlib)),
            #[cfg(feature = "lz4")]
            Self::LZ4 => Ok(Some(compress::algorithm::CompressionAlgorithm::Lz4)),
            #[cfg(not(feature = "lz4"))]
            Self::LZ4 => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "LZ4 compression not available (feature not enabled)",
            )),
            #[cfg(feature = "zstd")]
            Self::Zstd => Ok(Some(compress::algorithm::CompressionAlgorithm::Zstd)),
            #[cfg(not(feature = "zstd"))]
            Self::Zstd => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Zstd compression not available (feature not enabled)",
            )),
        }
    }
}
