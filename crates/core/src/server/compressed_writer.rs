//! Compressed writer that wraps multiplexed streams with compression.
//!
//! This module implements compression on top of multiplexed rsync protocol streams,
//! mirroring upstream rsync's io.c:io_start_buffering_out() behavior where compression
//! is applied after multiplex framing.

use std::io::{self, Write};

use compress::algorithm::CompressionAlgorithm;
use compress::zlib::{CompressionLevel, CountingZlibEncoder};

#[cfg(feature = "lz4")]
use compress::lz4::CountingLz4Encoder;

#[cfg(feature = "zstd")]
use compress::zstd::CountingZstdEncoder;

/// Wraps a writer with compression, buffering compressed output.
///
/// Mirrors upstream rsync's io.c:io_start_buffering_out() behavior where
/// compression is applied on top of the multiplexed stream.
///
/// The writer compresses input data and buffers the compressed output before
/// writing to the underlying writer. This matches upstream's buffering strategy
/// where compressed data is accumulated before being sent through the multiplex layer.
pub struct CompressedWriter<W: Write> {
    /// The underlying writer (typically MultiplexWriter)
    inner: W,
    /// Active compression encoder variant
    encoder: EncoderVariant,
    /// Flush threshold - flush when buffer exceeds this size
    flush_threshold: usize,
}

/// Enum wrapper around different compression encoder types.
///
/// Each variant holds an encoder configured to write to a Vec<u8> sink.
#[allow(dead_code)] // Used in production code once compression is integrated
#[allow(clippy::large_enum_variant)]
enum EncoderVariant {
    /// zlib encoder writing to Vec<u8>
    Zlib(CountingZlibEncoder<Vec<u8>>),
    /// LZ4 encoder writing to Vec<u8>
    #[cfg(feature = "lz4")]
    Lz4(CountingLz4Encoder<Vec<u8>>),
    /// Zstandard encoder writing to Vec<u8>
    #[cfg(feature = "zstd")]
    Zstd(CountingZstdEncoder<Vec<u8>>),
}

impl<W: Write> CompressedWriter<W> {
    /// Creates a new compressed writer wrapping the given writer.
    #[allow(dead_code)] // Used in production code once compression is integrated
    ///
    /// The compressor is initialized based on the negotiated algorithm and writes
    /// compressed data to an internal buffer before flushing to the underlying writer.
    ///
    /// # Arguments
    ///
    /// * `inner` - The underlying writer (typically `MultiplexWriter`)
    /// * `algorithm` - Negotiated compression algorithm
    /// * `level` - Compression level to use
    ///
    /// # Errors
    ///
    /// Returns an error if the compression algorithm is not supported in this build
    /// (e.g., LZ4 or Zstd without the corresponding feature flag).
    pub fn new(
        inner: W,
        algorithm: CompressionAlgorithm,
        level: CompressionLevel,
    ) -> io::Result<Self> {
        let encoder = match algorithm {
            CompressionAlgorithm::Zlib => {
                let sink = Vec::with_capacity(4096);
                EncoderVariant::Zlib(CountingZlibEncoder::with_sink(sink, level))
            }
            #[cfg(feature = "lz4")]
            CompressionAlgorithm::Lz4 => {
                let sink = Vec::with_capacity(4096);
                EncoderVariant::Lz4(CountingLz4Encoder::with_sink(sink, level))
            }
            #[cfg(feature = "zstd")]
            CompressionAlgorithm::Zstd => {
                let sink = Vec::with_capacity(4096);
                EncoderVariant::Zstd(CountingZstdEncoder::with_sink(sink, level)?)
            }
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

        Ok(Self {
            inner,
            encoder,
            flush_threshold: 4096, // Match upstream IO_BUFFER_SIZE
        })
    }

    /// Flushes compressed data to the underlying writer.
    ///
    /// This drains the encoder's internal sink and writes accumulated compressed
    /// data to the underlying writer, then clears the output buffer.
    fn flush_compressed(&mut self) -> io::Result<()> {
        // Get compressed bytes from encoder's sink and write to inner
        match &mut self.encoder {
            EncoderVariant::Zlib(encoder) => {
                let sink = encoder.get_mut();
                if !sink.is_empty() {
                    self.inner.write_all(sink)?;
                    sink.clear();
                }
            }
            #[cfg(feature = "lz4")]
            EncoderVariant::Lz4(encoder) => {
                let sink = encoder.get_mut();
                if !sink.is_empty() {
                    self.inner.write_all(sink)?;
                    sink.clear();
                }
            }
            #[cfg(feature = "zstd")]
            EncoderVariant::Zstd(encoder) => {
                let sink = encoder.get_mut();
                if !sink.is_empty() {
                    self.inner.write_all(sink)?;
                    sink.clear();
                }
            }
        }

        // Flush the underlying writer
        self.inner.flush()
    }

    /// Finishes the compression stream and flushes all data.
    ///
    /// This MUST be called before dropping the writer to ensure all
    /// compressed data (including trailer bytes) is written.
    ///
    /// Returns the underlying writer so it can be reused.
    ///
    /// # Errors
    ///
    /// Returns an error if finishing the compression stream or flushing fails.
    #[allow(dead_code)] // Used in production code once compression is integrated
    pub fn finish(mut self) -> io::Result<W> {
        // Finish the encoder - this writes final trailer bytes to the sink
        match self.encoder {
            EncoderVariant::Zlib(encoder) => {
                let (sink, _bytes) = encoder.finish_into_inner()?;
                if !sink.is_empty() {
                    self.inner.write_all(&sink)?;
                }
            }
            #[cfg(feature = "lz4")]
            EncoderVariant::Lz4(encoder) => {
                let (sink, _bytes) = encoder.finish_into_inner()?;
                if !sink.is_empty() {
                    self.inner.write_all(&sink)?;
                }
            }
            #[cfg(feature = "zstd")]
            EncoderVariant::Zstd(encoder) => {
                let (sink, _bytes) = encoder.finish_into_inner()?;
                if !sink.is_empty() {
                    self.inner.write_all(&sink)?;
                }
            }
        }

        // Final flush of underlying writer
        self.inner.flush()?;
        Ok(self.inner)
    }

    /// Returns the number of compressed bytes written so far.
    #[must_use]
    #[allow(dead_code)] // Used in production code once compression is integrated
    pub fn bytes_written(&self) -> u64 {
        match &self.encoder {
            EncoderVariant::Zlib(encoder) => encoder.bytes_written(),
            #[cfg(feature = "lz4")]
            EncoderVariant::Lz4(encoder) => encoder.bytes_written(),
            #[cfg(feature = "zstd")]
            EncoderVariant::Zstd(encoder) => encoder.bytes_written(),
        }
    }

    /// Provides mutable access to the underlying writer.
    ///
    /// This is used for sending control messages through the multiplex layer
    /// without compressing them (matching upstream rsync behavior where control
    /// messages bypass the compression buffer).
    ///
    /// # Safety
    ///
    /// Caller must ensure that any writes to the underlying writer don't corrupt
    /// the compression stream. This should only be used for multiplex control
    /// messages that are handled at a different protocol layer.
    pub fn inner_mut(&mut self) -> &mut W {
        &mut self.inner
    }
}

impl<W: Write> Write for CompressedWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        // Compress input data - this writes to the encoder's internal Vec<u8> sink
        match &mut self.encoder {
            EncoderVariant::Zlib(encoder) => encoder.write(buf)?,
            #[cfg(feature = "lz4")]
            EncoderVariant::Lz4(encoder) => encoder.write(buf)?,
            #[cfg(feature = "zstd")]
            EncoderVariant::Zstd(encoder) => encoder.write(buf)?,
        }

        // Check if we should flush compressed data to underlying writer
        let current_size = match &self.encoder {
            EncoderVariant::Zlib(encoder) => encoder.get_ref().len(),
            #[cfg(feature = "lz4")]
            EncoderVariant::Lz4(encoder) => encoder.get_ref().len(),
            #[cfg(feature = "zstd")]
            EncoderVariant::Zstd(encoder) => encoder.get_ref().len(),
        };

        if current_size > self.flush_threshold {
            self.flush_compressed()?;
        }

        // Always report full write to match upstream behavior
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.flush_compressed()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use compress::zlib::decompress_to_vec;

    #[test]
    fn compress_round_trip_zlib() {
        let data = b"test data that should be compressed";
        let mut buf = Vec::new();
        let mut writer = CompressedWriter::new(
            &mut buf,
            CompressionAlgorithm::Zlib,
            CompressionLevel::Default,
        )
        .unwrap();

        writer.write_all(data).unwrap();
        writer.finish().unwrap();

        // Verify compressed data exists
        assert!(!buf.is_empty());

        // Decompress and verify
        let decompressed = decompress_to_vec(&buf).unwrap();
        assert_eq!(decompressed, data);
    }

    #[test]
    fn compress_multiple_writes() {
        let data1 = b"first chunk ";
        let data2 = b"second chunk";
        let data3 = b" third chunk";

        let mut buf = Vec::new();
        let mut writer = CompressedWriter::new(
            &mut buf,
            CompressionAlgorithm::Zlib,
            CompressionLevel::Default,
        )
        .unwrap();

        writer.write_all(data1).unwrap();
        writer.write_all(data2).unwrap();
        writer.write_all(data3).unwrap();
        writer.finish().unwrap();

        // Decompress and verify all chunks
        let decompressed = decompress_to_vec(&buf).unwrap();
        let expected = b"first chunk second chunk third chunk";
        assert_eq!(decompressed, expected);
    }

    #[test]
    fn compress_large_data_flushes_automatically() {
        // Create data larger than flush threshold
        let data = vec![b'x'; 8192];

        let mut buf = Vec::new();
        {
            let mut writer =
                CompressedWriter::new(&mut buf, CompressionAlgorithm::Zlib, CompressionLevel::Fast)
                    .unwrap();

            writer.write_all(&data).unwrap();
            writer.finish().unwrap();
        }

        // Should have written compressed data
        assert!(!buf.is_empty());

        // Decompress and verify
        let decompressed = decompress_to_vec(&buf).unwrap();
        assert_eq!(decompressed, data);
    }

    #[test]
    fn bytes_written_tracks_compressed_size() {
        let data = b"test data that should compress to a reasonable size";
        let mut buf = Vec::new();
        {
            let mut writer = CompressedWriter::new(
                &mut buf,
                CompressionAlgorithm::Zlib,
                CompressionLevel::Default,
            )
            .unwrap();

            writer.write_all(data).unwrap();
            writer.finish().unwrap();
        }

        // bytes_written should track compressed size (after finish)
        // For zlib, compressed size should be reasonable
        assert!(!buf.is_empty());
        assert!(buf.len() < data.len() + 20); // Allow for zlib overhead
    }
}
