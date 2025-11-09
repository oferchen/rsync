use std::io;

use compress::algorithm::CompressionAlgorithm;
use compress::zlib::{CompressionLevel, CountingZlibEncoder};

#[cfg(feature = "lz4")]
use compress::lz4::CountingLz4Encoder;

#[cfg(feature = "zstd")]
use compress::zstd::CountingZstdEncoder;

/// Wrapper around the active compression encoder used during local copies.
#[allow(clippy::large_enum_variant)]
pub enum ActiveCompressor {
    /// zlib-based encoder compatible with upstream rsync's historical default.
    Zlib(CountingZlibEncoder),
    /// LZ4 frame encoder negotiated via `--compress-choice=lz4`.
    #[cfg(feature = "lz4")]
    Lz4(CountingLz4Encoder),
    /// Zstandard encoder negotiated via `--compress-choice=zstd`.
    #[cfg(feature = "zstd")]
    Zstd(CountingZstdEncoder),
}

impl ActiveCompressor {
    /// Creates a compressor for `algorithm` using the provided compression `level`.
    pub fn new(algorithm: CompressionAlgorithm, level: CompressionLevel) -> io::Result<Self> {
        match algorithm {
            CompressionAlgorithm::Zlib => Ok(Self::Zlib(CountingZlibEncoder::new(level))),
            #[cfg(feature = "lz4")]
            CompressionAlgorithm::Lz4 => Ok(Self::Lz4(CountingLz4Encoder::new(level))),
            #[cfg(feature = "zstd")]
            CompressionAlgorithm::Zstd => CountingZstdEncoder::new(level).map(Self::Zstd),
            #[allow(unreachable_patterns)]
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "compression algorithm {} is not supported",
                    algorithm.name()
                ),
            )),
        }
    }

    /// Appends `chunk` to the compressed stream.
    pub fn write(&mut self, chunk: &[u8]) -> io::Result<()> {
        match self {
            Self::Zlib(encoder) => encoder.write(chunk),
            #[cfg(feature = "lz4")]
            Self::Lz4(encoder) => encoder.write(chunk),
            #[cfg(feature = "zstd")]
            Self::Zstd(encoder) => encoder.write(chunk),
        }
    }

    /// Returns the number of compressed bytes produced so far.
    #[must_use]
    pub fn bytes_written(&self) -> u64 {
        match self {
            Self::Zlib(encoder) => encoder.bytes_written(),
            #[cfg(feature = "lz4")]
            Self::Lz4(encoder) => encoder.bytes_written(),
            #[cfg(feature = "zstd")]
            Self::Zstd(encoder) => encoder.bytes_written(),
        }
    }

    /// Finalises the compression stream and returns the total number of compressed bytes.
    pub fn finish(self) -> io::Result<u64> {
        match self {
            Self::Zlib(encoder) => encoder.finish(),
            #[cfg(feature = "lz4")]
            Self::Lz4(encoder) => encoder.finish(),
            #[cfg(feature = "zstd")]
            Self::Zstd(encoder) => encoder.finish(),
        }
    }
}
