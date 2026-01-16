//! Compressed reader that wraps multiplexed streams with decompression.
//!
//! This module implements decompression on top of multiplexed rsync protocol streams,
//! mirroring upstream rsync's io.c:io_start_buffering_in() behavior where decompression
//! is applied after multiplex framing.

use std::io::{self, Read};

use compress::algorithm::CompressionAlgorithm;
use compress::zlib::CountingZlibDecoder;

#[cfg(feature = "lz4")]
use compress::lz4::CountingLz4Decoder;

#[cfg(feature = "zstd")]
use compress::zstd::CountingZstdDecoder;

/// Wraps a reader with decompression, reading compressed input.
///
/// Mirrors upstream rsync's io.c:io_start_buffering_in() behavior where
/// decompression is applied on top of the multiplexed stream.
///
/// The reader decompresses input data from the underlying reader, matching
/// upstream's buffering strategy where compressed data is received through
/// the multiplex layer and decompressed before being consumed.
pub struct CompressedReader<R: Read> {
    /// Active decompression decoder variant
    /// The decoder owns the underlying reader
    decoder: DecoderVariant<R>,
}

/// Enum wrapper around different compression decoder types.
///
/// Each variant holds a decoder configured to read from the underlying stream.
#[allow(clippy::large_enum_variant)]
enum DecoderVariant<R: Read> {
    /// zlib decoder
    Zlib(CountingZlibDecoder<R>),
    /// LZ4 decoder
    #[cfg(feature = "lz4")]
    Lz4(CountingLz4Decoder<R>),
    /// Zstandard decoder
    #[cfg(feature = "zstd")]
    Zstd(CountingZstdDecoder<R>),
}

impl<R: Read> CompressedReader<R> {
    /// Creates a new compressed reader wrapping the given reader.
    ///
    /// The decompressor is initialized based on the negotiated algorithm and reads
    /// compressed data from the underlying reader.
    ///
    /// # Arguments
    ///
    /// * `inner` - The underlying reader (typically `MultiplexReader`)
    /// * `algorithm` - Negotiated compression algorithm
    ///
    /// # Errors
    ///
    /// Returns an error if the compression algorithm is not supported in this build
    /// (e.g., LZ4 or Zstd without the corresponding feature flag).
    pub fn new(inner: R, algorithm: CompressionAlgorithm) -> io::Result<Self> {
        let decoder = match algorithm {
            CompressionAlgorithm::Zlib => DecoderVariant::Zlib(CountingZlibDecoder::new(inner)),
            #[cfg(feature = "lz4")]
            CompressionAlgorithm::Lz4 => DecoderVariant::Lz4(CountingLz4Decoder::new(inner)),
            #[cfg(feature = "zstd")]
            CompressionAlgorithm::Zstd => DecoderVariant::Zstd(CountingZstdDecoder::new(inner)?),
            #[allow(unreachable_patterns)]
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!(
                        "compression algorithm {} is not supported",
                        algorithm.name()
                    ),
                ));
            }
        };

        Ok(Self { decoder })
    }

    /// Returns the number of compressed bytes read so far.
    #[must_use]
    pub const fn bytes_read(&self) -> u64 {
        match &self.decoder {
            DecoderVariant::Zlib(decoder) => decoder.bytes_read(),
            #[cfg(feature = "lz4")]
            DecoderVariant::Lz4(decoder) => decoder.bytes_read(),
            #[cfg(feature = "zstd")]
            DecoderVariant::Zstd(decoder) => decoder.bytes_read(),
        }
    }
}

impl<R: Read> Read for CompressedReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        // Decompress data from underlying reader
        match &mut self.decoder {
            DecoderVariant::Zlib(decoder) => decoder.read(buf),
            #[cfg(feature = "lz4")]
            DecoderVariant::Lz4(decoder) => decoder.read(buf),
            #[cfg(feature = "zstd")]
            DecoderVariant::Zstd(decoder) => decoder.read(buf),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use compress::zlib::{CompressionLevel, compress_to_vec};
    use std::io::Cursor;

    #[test]
    fn decompress_round_trip_zlib() {
        let original = b"test data that should be compressed and decompressed";

        // Compress the data first
        let compressed = compress_to_vec(original, CompressionLevel::Default).unwrap();

        // Now decompress using CompressedReader
        let cursor = Cursor::new(compressed);
        let mut reader = CompressedReader::new(cursor, CompressionAlgorithm::Zlib).unwrap();

        let mut decompressed = Vec::new();
        reader.read_to_end(&mut decompressed).unwrap();
        assert_eq!(decompressed, original);
    }

    #[test]
    fn decompress_multiple_reads() {
        let original = b"first chunk second chunk third chunk";

        // Compress the data first
        let compressed = compress_to_vec(original, CompressionLevel::Default).unwrap();

        // Decompress using multiple read calls
        let cursor = Cursor::new(compressed);
        let mut reader = CompressedReader::new(cursor, CompressionAlgorithm::Zlib).unwrap();

        let mut decompressed = Vec::new();
        let mut buf = [0u8; 16];
        loop {
            let n = reader.read(&mut buf).unwrap();
            if n == 0 {
                break;
            }
            decompressed.extend_from_slice(&buf[..n]);
        }

        assert_eq!(decompressed, original);
    }

    #[test]
    fn decompress_large_data() {
        // Create large data that will require multiple internal reads
        let original = vec![b'x'; 8192];

        // Compress the data first
        let compressed = compress_to_vec(&original, CompressionLevel::Fast).unwrap();

        // Decompress
        let cursor = Cursor::new(compressed);
        let mut reader = CompressedReader::new(cursor, CompressionAlgorithm::Zlib).unwrap();

        let mut decompressed = Vec::new();
        reader.read_to_end(&mut decompressed).unwrap();

        assert_eq!(decompressed, original);
    }
}
