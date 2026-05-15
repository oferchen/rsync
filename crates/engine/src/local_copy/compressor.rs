use std::io;
use std::num::NonZeroU8;

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
        Self::new_with_workers(algorithm, level, None)
    }

    /// Creates a compressor with an optional zstd worker thread count.
    /// `workers` only affects zstd; other algorithms ignore the value.
    /// upstream: `token.c:701` plumbs `do_compression_threads` into
    /// `ZSTD_c_nbWorkers`.
    pub fn new_with_workers(
        algorithm: CompressionAlgorithm,
        level: CompressionLevel,
        workers: Option<NonZeroU8>,
    ) -> io::Result<Self> {
        match algorithm {
            CompressionAlgorithm::Zlib => Ok(Self::Zlib(CountingZlibEncoder::new(level))),
            #[cfg(feature = "lz4")]
            CompressionAlgorithm::Lz4 => Ok(Self::Lz4(CountingLz4Encoder::new(level))),
            #[cfg(feature = "zstd")]
            CompressionAlgorithm::Zstd => {
                CountingZstdEncoder::new_with_workers(level, workers).map(Self::Zstd)
            }
            #[allow(unreachable_patterns)]
            _ => {
                let _ = workers;
                Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!(
                        "compression algorithm {} is not supported",
                        algorithm.name()
                    ),
                ))
            }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn active_compressor_new_zlib() {
        let compressor =
            ActiveCompressor::new(CompressionAlgorithm::Zlib, CompressionLevel::Default);
        assert!(compressor.is_ok());
        let compressor = compressor.unwrap();
        assert!(matches!(compressor, ActiveCompressor::Zlib(_)));
    }

    #[test]
    fn active_compressor_zlib_bytes_written_initially_zero() {
        let compressor =
            ActiveCompressor::new(CompressionAlgorithm::Zlib, CompressionLevel::Default)
                .expect("zlib compressor");
        assert_eq!(compressor.bytes_written(), 0);
    }

    #[test]
    fn active_compressor_zlib_write_and_finish() {
        let mut compressor =
            ActiveCompressor::new(CompressionAlgorithm::Zlib, CompressionLevel::Default)
                .expect("zlib compressor");

        let data = b"Hello, world! This is some test data to compress.";
        compressor.write(data).expect("write data");

        // After writing, bytes_written may or may not be updated (depends on buffering)
        // But after finish, we should have some compressed bytes
        let total = compressor.finish().expect("finish compression");
        assert!(total > 0);
    }

    #[test]
    fn active_compressor_zlib_empty_input() {
        let compressor =
            ActiveCompressor::new(CompressionAlgorithm::Zlib, CompressionLevel::Default)
                .expect("zlib compressor");

        // Even with no data, finish should succeed
        // Zlib produces header bytes even for empty input, so finish returns Ok
        let _total = compressor.finish().expect("finish compression");
    }

    #[test]
    fn active_compressor_zlib_multiple_writes() {
        let mut compressor =
            ActiveCompressor::new(CompressionAlgorithm::Zlib, CompressionLevel::Default)
                .expect("zlib compressor");

        compressor.write(b"First chunk of data. ").expect("write 1");
        compressor
            .write(b"Second chunk of data. ")
            .expect("write 2");
        compressor.write(b"Third chunk of data.").expect("write 3");

        let total = compressor.finish().expect("finish compression");
        assert!(total > 0);
    }

    #[cfg(feature = "lz4")]
    #[test]
    fn active_compressor_new_lz4() {
        let compressor =
            ActiveCompressor::new(CompressionAlgorithm::Lz4, CompressionLevel::Default);
        assert!(compressor.is_ok());
        let compressor = compressor.unwrap();
        assert!(matches!(compressor, ActiveCompressor::Lz4(_)));
    }

    #[cfg(feature = "lz4")]
    #[test]
    fn active_compressor_lz4_write_and_finish() {
        let mut compressor =
            ActiveCompressor::new(CompressionAlgorithm::Lz4, CompressionLevel::Default)
                .expect("lz4 compressor");

        let data = b"Test data for LZ4 compression.";
        compressor.write(data).expect("write data");
        let total = compressor.finish().expect("finish compression");
        assert!(total > 0);
    }

    #[cfg(feature = "zstd")]
    #[test]
    fn active_compressor_new_zstd() {
        let compressor =
            ActiveCompressor::new(CompressionAlgorithm::Zstd, CompressionLevel::Default);
        assert!(compressor.is_ok());
        let compressor = compressor.unwrap();
        assert!(matches!(compressor, ActiveCompressor::Zstd(_)));
    }

    #[cfg(feature = "zstd")]
    #[test]
    fn active_compressor_zstd_workers_dispatches_to_encoder() {
        // None produces a single-threaded encoder. Some(_) either succeeds
        // (zstdmt on) or returns Unsupported (zstdmt off). Never silently drops.
        let none = ActiveCompressor::new_with_workers(
            CompressionAlgorithm::Zstd,
            CompressionLevel::Default,
            None,
        )
        .expect("workers=None");
        assert!(matches!(none, ActiveCompressor::Zstd(_)));

        let some = ActiveCompressor::new_with_workers(
            CompressionAlgorithm::Zstd,
            CompressionLevel::Default,
            NonZeroU8::new(4),
        );
        if compress::zstd::SUPPORTS_MULTITHREAD {
            assert!(some.is_ok());
        } else {
            assert_eq!(some.unwrap_err().kind(), io::ErrorKind::Unsupported);
        }
    }

    #[cfg(feature = "zstd")]
    #[test]
    fn active_compressor_zstd_write_and_finish() {
        let mut compressor =
            ActiveCompressor::new(CompressionAlgorithm::Zstd, CompressionLevel::Default)
                .expect("zstd compressor");

        let data = b"Test data for Zstandard compression.";
        compressor.write(data).expect("write data");
        let total = compressor.finish().expect("finish compression");
        assert!(total > 0);
    }
}
