//! Concrete compression strategy implementations.

use super::{CompressionAlgorithmKind, CompressionStrategy};
use crate::zlib::{CompressionLevel, CountingZlibEncoder};
use std::io::{self, Write};

#[cfg(feature = "zstd")]
use crate::zstd::{self, CountingZstdEncoder};

#[cfg(feature = "lz4")]
use crate::lz4::raw;

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

        let final_len = returned_output.len();
        Ok(final_len - initial_len)
    }

    fn decompress(&self, input: &[u8], output: &mut Vec<u8>) -> io::Result<usize> {
        let initial_len = output.len();
        let mut decoder = flate2::read::DeflateDecoder::new(input);
        io::copy(&mut decoder, output)?;
        Ok(output.len() - initial_len)
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
        zstd::decompress_into(input, output)
    }

    fn algorithm_kind(&self) -> CompressionAlgorithmKind {
        CompressionAlgorithmKind::Zstd
    }
}

/// LZ4 compression strategy using raw block format for wire compatibility.
///
/// Uses raw LZ4 blocks with rsync's 2-byte wire protocol header, matching
/// upstream rsync 3.4.1's `token.c` implementation. This differs from the
/// frame format (which includes magic bytes and checksums) - raw blocks are
/// required for interoperability with upstream rsync.
///
/// Only available when the `lz4` feature is enabled.
///
/// # Upstream Reference
///
/// `token.c:send_deflated_token()` - wire format definition.
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
        let compressed = raw::compress_block_to_vec(input).map_err(|e| {
            io::Error::new(io::ErrorKind::InvalidData, e.to_string())
        })?;
        let len = compressed.len();
        output.extend_from_slice(&compressed);
        Ok(len)
    }

    fn decompress(&self, input: &[u8], output: &mut Vec<u8>) -> io::Result<usize> {
        // Allocate output buffer based on raw module's safety limit.
        // The raw header encodes compressed size; decompressed size is bounded
        // by MAX_DECOMPRESSED_SIZE to prevent memory exhaustion.
        let decompressed = raw::decompress_block_to_vec(input, raw::MAX_BLOCK_SIZE)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
        let len = decompressed.len();
        output.extend_from_slice(&decompressed);
        Ok(len)
    }

    fn algorithm_kind(&self) -> CompressionAlgorithmKind {
        CompressionAlgorithmKind::Lz4
    }
}
