#![allow(clippy::module_name_repetitions)]

//! Streaming Zstandard helpers shared across the workspace.
//!
//! The interface mirrors the zlib helpers so higher layers can swap algorithms
//! without reworking their plumbing. Encoders implement [`std::io::Write`] and
//! keep track of the number of compressed bytes produced, allowing bandwidth
//! accounting to reuse the same code paths as zlib compression.

use std::io::{self, BufReader, IoSliceMut, Read, Write};

use crate::algorithm::CompressionAlgorithm;
use crate::common::{CountingSink, CountingWriter};
use crate::zlib::CompressionLevel;
use zstd::stream::{read::Decoder as ZstdDecoder, write::Encoder as ZstdEncoder};

/// Streaming encoder that records the number of compressed bytes produced.
pub struct CountingZstdEncoder<W = CountingSink>
where
    W: Write,
{
    inner: ZstdEncoder<'static, CountingWriter<W>>,
}

impl CountingZstdEncoder<CountingSink> {
    /// Creates a new encoder that discards the compressed output while tracking its length.
    ///
    /// # Example
    ///
    /// ```
    /// use compress::zstd::CountingZstdEncoder;
    /// use compress::zlib::CompressionLevel;
    ///
    /// let mut encoder = CountingZstdEncoder::new(CompressionLevel::Default).unwrap();
    /// encoder.write(b"data to compress").unwrap();
    /// let compressed_bytes = encoder.finish().unwrap();
    /// ```
    pub fn new(level: CompressionLevel) -> io::Result<Self> {
        Self::with_sink(CountingSink, level)
    }

    /// Completes the stream and returns the total number of compressed bytes generated.
    pub fn finish(self) -> io::Result<u64> {
        let (_sink, bytes) = self.finish_into_inner()?;
        Ok(bytes)
    }
}

impl<W> CountingZstdEncoder<W>
where
    W: Write,
{
    /// Creates a new encoder that writes compressed bytes into `sink`.
    ///
    /// # Example
    ///
    /// ```
    /// use compress::zstd::CountingZstdEncoder;
    /// use compress::zlib::CompressionLevel;
    ///
    /// let mut output = Vec::new();
    /// let mut encoder = CountingZstdEncoder::with_sink(&mut output, CompressionLevel::Fast).unwrap();
    /// encoder.write(b"payload").unwrap();
    /// let (_, bytes_written) = encoder.finish_into_inner().unwrap();
    /// assert!(bytes_written > 0);
    /// ```
    pub fn with_sink(sink: W, level: CompressionLevel) -> io::Result<Self> {
        let writer = CountingWriter::new(sink);
        let encoder = ZstdEncoder::new(writer, zstd_level(level)).map_err(io::Error::other)?;
        Ok(Self { inner: encoder })
    }

    /// Appends data to the compression stream.
    pub fn write(&mut self, input: &[u8]) -> io::Result<()> {
        self.inner.write_all(input)
    }

    /// Returns the number of compressed bytes produced so far.
    #[inline]
    #[must_use]
    pub fn bytes_written(&self) -> u64 {
        self.inner.get_ref().bytes()
    }

    /// Provides immutable access to the underlying sink.
    #[inline]
    #[must_use]
    pub fn get_ref(&self) -> &W {
        self.inner.get_ref().inner_ref()
    }

    /// Provides mutable access to the underlying sink.
    #[inline]
    #[must_use]
    pub fn get_mut(&mut self) -> &mut W {
        self.inner.get_mut().inner_mut()
    }

    /// Completes the stream and returns the sink together with the number of compressed bytes.
    pub fn finish_into_inner(self) -> io::Result<(W, u64)> {
        let writer = self.inner.finish().map_err(io::Error::other)?;
        Ok(writer.into_parts())
    }
}

/// Streaming decoder that records the number of decompressed bytes produced.
pub struct CountingZstdDecoder<R> {
    inner: ZstdDecoder<'static, BufReader<R>>,
    bytes: u64,
}

impl<R> CountingZstdDecoder<R>
where
    R: Read,
{
    /// Creates a new decoder that wraps the provided reader.
    pub fn new(reader: R) -> io::Result<Self> {
        let decoder = ZstdDecoder::new(reader).map_err(io::Error::other)?;
        Ok(Self {
            inner: decoder,
            bytes: 0,
        })
    }

    /// Returns the number of decompressed bytes read so far.
    #[inline]
    #[must_use]
    pub const fn bytes_read(&self) -> u64 {
        self.bytes
    }

    /// Returns a mutable reference to the underlying reader.
    #[inline]
    #[must_use]
    pub fn get_mut(&mut self) -> &mut R {
        self.inner.get_mut().get_mut()
    }

    /// Returns an immutable reference to the wrapped reader.
    #[inline]
    #[must_use]
    pub fn get_ref(&self) -> &R {
        self.inner.get_ref().get_ref()
    }

    /// Consumes the decoder and returns the wrapped reader.
    #[inline]
    #[must_use]
    pub fn into_inner(self) -> R {
        self.inner.finish().into_inner()
    }
}

impl<R> Read for CountingZstdDecoder<R>
where
    R: Read,
{
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let read = self.inner.read(buf)?;
        self.bytes = self.bytes.saturating_add(read as u64);
        Ok(read)
    }

    fn read_vectored(&mut self, bufs: &mut [IoSliceMut<'_>]) -> io::Result<usize> {
        let read = self.inner.read_vectored(bufs)?;
        self.bytes = self.bytes.saturating_add(read as u64);
        Ok(read)
    }
}

/// Compresses `input` into a new [`Vec`].
pub fn compress_to_vec(input: &[u8], level: CompressionLevel) -> io::Result<Vec<u8>> {
    let mut encoder = ZstdEncoder::new(Vec::new(), zstd_level(level)).map_err(io::Error::other)?;
    encoder.write_all(input)?;
    encoder.finish().map_err(io::Error::other)
}

/// Decompresses `input` into a new [`Vec`].
pub fn decompress_to_vec(input: &[u8]) -> io::Result<Vec<u8>> {
    let mut decoder = ZstdDecoder::new(input).map_err(io::Error::other)?;
    let mut output = Vec::new();
    io::copy(&mut decoder, &mut output)?;
    Ok(output)
}

/// Returns the preferred compression algorithm when callers do not provide one explicitly.
#[must_use]
pub const fn default_algorithm() -> CompressionAlgorithm {
    CompressionAlgorithm::Zstd
}

fn zstd_level(level: CompressionLevel) -> i32 {
    match level {
        CompressionLevel::None => 0,
        CompressionLevel::Fast => 1,
        CompressionLevel::Default => 3,
        CompressionLevel::Best => 19,
        CompressionLevel::Precise(value) => i32::from(value.get()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn counting_encoder_tracks_bytes() {
        let mut encoder = CountingZstdEncoder::new(CompressionLevel::Default).expect("encoder");
        encoder.write(b"payload").expect("compress payload");
        let compressed = encoder.finish().expect("finish stream");
        assert!(compressed > 0);
    }

    #[test]
    fn encoder_with_sink_forwards_bytes() {
        let mut encoder =
            CountingZstdEncoder::with_sink(Vec::new(), CompressionLevel::Default).expect("encoder");
        encoder.write(b"payload").expect("compress payload");
        let (compressed, bytes) = encoder.finish_into_inner().expect("finish stream");
        assert_eq!(bytes as usize, compressed.len());
    }

    #[test]
    fn decoder_tracks_bytes() {
        let compressed = compress_to_vec(b"payload", CompressionLevel::Default).expect("compress");
        let mut decoder = CountingZstdDecoder::new(&compressed[..]).expect("decoder");
        let mut output = Vec::new();
        decoder.read_to_end(&mut output).expect("decompress");
        assert_eq!(output, b"payload");
        assert_eq!(decoder.bytes_read(), output.len() as u64);
    }

    #[test]
    fn level_0_no_compression_works() {
        let input = b"test data that should not be compressed";
        let compressed = compress_to_vec(input, CompressionLevel::None).expect("compress level 0");
        let decompressed = decompress_to_vec(&compressed).expect("decompress level 0");
        assert_eq!(decompressed, input);
    }

    #[test]
    fn all_levels_produce_valid_output() {
        let input = b"The quick brown fox jumps over the lazy dog";

        // Test levels 1-22 (zstd's supported range)
        for level in 1..=22 {
            let level_config = CompressionLevel::Precise(
                std::num::NonZeroU8::new(level).expect("non-zero level")
            );
            let compressed = compress_to_vec(input, level_config)
                .unwrap_or_else(|e| panic!("compress failed at level {level}: {e}"));
            let decompressed = decompress_to_vec(&compressed)
                .unwrap_or_else(|e| panic!("decompress failed at level {level}: {e}"));
            assert_eq!(
                decompressed, input,
                "round-trip failed at level {level}"
            );
        }
    }

    #[test]
    fn higher_levels_produce_smaller_output() {
        // Use highly compressible data with larger size for meaningful compression differences
        let mut input = Vec::new();
        for _ in 0..50 {
            input.extend_from_slice(b"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
            input.extend_from_slice(b"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
            input.extend_from_slice(b"cccccccccccccccccccccccccccccccccccccccc");
            input.extend_from_slice(b"dddddddddddddddddddddddddddddddddddddddd");
        }

        let level_1 = CompressionLevel::Precise(std::num::NonZeroU8::new(1).unwrap());
        let level_10 = CompressionLevel::Precise(std::num::NonZeroU8::new(10).unwrap());
        let level_19 = CompressionLevel::Precise(std::num::NonZeroU8::new(19).unwrap());

        let compressed_1 = compress_to_vec(&input, level_1).expect("compress level 1");
        let compressed_10 = compress_to_vec(&input, level_10).expect("compress level 10");
        let compressed_19 = compress_to_vec(&input, level_19).expect("compress level 19");

        // Higher levels should compress better or equal for highly compressible data
        // Note: We use level 19 instead of 22 since ultra-high levels may have diminishing returns
        assert!(
            compressed_10.len() <= compressed_1.len(),
            "level 10 ({} bytes) should be <= level 1 ({} bytes)",
            compressed_10.len(),
            compressed_1.len()
        );
        assert!(
            compressed_19.len() <= compressed_10.len(),
            "level 19 ({} bytes) should be <= level 10 ({} bytes)",
            compressed_19.len(),
            compressed_10.len()
        );

        // Verify all decompress correctly
        assert_eq!(decompress_to_vec(&compressed_1).unwrap(), input);
        assert_eq!(decompress_to_vec(&compressed_10).unwrap(), input);
        assert_eq!(decompress_to_vec(&compressed_19).unwrap(), input);
    }

    #[test]
    fn round_trip_all_levels() {
        let input = b"Lorem ipsum dolor sit amet, consectetur adipiscing elit. \
                      Sed do eiusmod tempor incididunt ut labore et dolore magna aliqua.";

        // Test level 0 (None)
        let compressed = compress_to_vec(input, CompressionLevel::None).expect("compress level 0");
        let decompressed = decompress_to_vec(&compressed).expect("decompress level 0");
        assert_eq!(decompressed, input, "round-trip failed at level 0");

        // Test preset levels
        for level_config in [
            CompressionLevel::Fast,
            CompressionLevel::Default,
            CompressionLevel::Best,
        ] {
            let compressed = compress_to_vec(input, level_config)
                .unwrap_or_else(|e| panic!("compress failed at {level_config:?}: {e}"));
            let decompressed = decompress_to_vec(&compressed)
                .unwrap_or_else(|e| panic!("decompress failed at {level_config:?}: {e}"));
            assert_eq!(
                decompressed, input,
                "round-trip failed at {level_config:?}"
            );
        }

        // Test all precise levels 1-22
        for level in 1..=22 {
            let level_config = CompressionLevel::Precise(
                std::num::NonZeroU8::new(level).expect("non-zero level")
            );
            let compressed = compress_to_vec(input, level_config)
                .unwrap_or_else(|e| panic!("compress failed at level {level}: {e}"));
            let decompressed = decompress_to_vec(&compressed)
                .unwrap_or_else(|e| panic!("decompress failed at level {level}: {e}"));
            assert_eq!(
                decompressed, input,
                "round-trip failed at level {level}"
            );
        }
    }

    #[test]
    fn counting_encoder_all_levels() {
        let input = b"test data for counting encoder";

        // Test that CountingZstdEncoder works with all levels
        for level in 1..=22 {
            let level_config = CompressionLevel::Precise(
                std::num::NonZeroU8::new(level).expect("non-zero level")
            );
            let mut encoder = CountingZstdEncoder::new(level_config)
                .unwrap_or_else(|e| panic!("encoder creation failed at level {level}: {e}"));
            encoder.write(input)
                .unwrap_or_else(|e| panic!("write failed at level {level}: {e}"));
            let bytes = encoder.finish()
                .unwrap_or_else(|e| panic!("finish failed at level {level}: {e}"));
            assert!(bytes > 0, "no bytes written at level {level}");
        }
    }

    #[test]
    fn compression_ratio_improves_with_level() {
        // Create highly repetitive data
        let mut input = Vec::new();
        for _ in 0..100 {
            input.extend_from_slice(b"The same text repeated over and over again. ");
        }

        let level_1 = CompressionLevel::Precise(std::num::NonZeroU8::new(1).unwrap());
        let level_5 = CompressionLevel::Precise(std::num::NonZeroU8::new(5).unwrap());
        let level_15 = CompressionLevel::Precise(std::num::NonZeroU8::new(15).unwrap());

        let compressed_1 = compress_to_vec(&input, level_1).expect("compress level 1");
        let compressed_5 = compress_to_vec(&input, level_5).expect("compress level 5");
        let compressed_15 = compress_to_vec(&input, level_15).expect("compress level 15");

        // Verify all produce valid output
        assert_eq!(decompress_to_vec(&compressed_1).unwrap(), input);
        assert_eq!(decompress_to_vec(&compressed_5).unwrap(), input);
        assert_eq!(decompress_to_vec(&compressed_15).unwrap(), input);

        // Verify compression ratio improves
        assert!(
            compressed_5.len() <= compressed_1.len(),
            "level 5 should compress better than level 1"
        );
        assert!(
            compressed_15.len() <= compressed_5.len(),
            "level 15 should compress better than level 5"
        );

        // Verify we actually achieved compression
        assert!(
            compressed_15.len() < input.len() / 2,
            "highly repetitive data should compress to less than 50%"
        );
    }

    #[test]
    fn edge_case_empty_input() {
        let input = b"";

        for level in [0, 1, 10, 22] {
            let level_config = if level == 0 {
                CompressionLevel::None
            } else {
                CompressionLevel::Precise(std::num::NonZeroU8::new(level).unwrap())
            };

            let compressed = compress_to_vec(input, level_config)
                .unwrap_or_else(|e| panic!("compress failed at level {level}: {e}"));
            let decompressed = decompress_to_vec(&compressed)
                .unwrap_or_else(|e| panic!("decompress failed at level {level}: {e}"));
            assert_eq!(decompressed, input, "empty input round-trip failed at level {level}");
        }
    }

    #[test]
    fn edge_case_single_byte() {
        let input = b"x";

        for level in [0, 1, 10, 22] {
            let level_config = if level == 0 {
                CompressionLevel::None
            } else {
                CompressionLevel::Precise(std::num::NonZeroU8::new(level).unwrap())
            };

            let compressed = compress_to_vec(input, level_config)
                .unwrap_or_else(|e| panic!("compress failed at level {level}: {e}"));
            let decompressed = decompress_to_vec(&compressed)
                .unwrap_or_else(|e| panic!("decompress failed at level {level}: {e}"));
            assert_eq!(decompressed, input, "single byte round-trip failed at level {level}");
        }
    }
}
