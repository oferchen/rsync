#![allow(clippy::module_name_repetitions)]

//! # Overview
//!
//! Raw deflate compression helpers for rsync wire protocol compatibility.
//!
//! This module uses raw deflate format (no zlib header/trailer) to match
//! upstream rsync's compression wire format. [`CountingZlibEncoder`] accepts
//! incremental input while tracking the number of bytes produced by the
//! compressor so higher layers can report accurate compressed sizes without
//! buffering the resulting payload in memory. The complementary
//! [`CountingZlibDecoder`] wraps a reader that produces decompressed bytes
//! while recording how much output has been yielded so far, keeping the
//! counter accurate for both scalar and vectored reads so downstream bandwidth
//! accounting mirrors upstream behaviour.
//!
//! # Wire Format
//!
//! Raw deflate produces a bare DEFLATE stream without the 2-byte zlib header
//! or 4-byte Adler-32 checksum trailer. This matches rsync's `deflateInit2()`
//! call with `windowBits = -MAX_WBITS` (negative value indicates raw deflate).
//!
//! # Examples
//!
//! Compress data incrementally and obtain the compressed length:
//!
//! ```
//! use compress::zlib::{CompressionLevel, CountingZlibEncoder};
//! use std::io::Write;
//!
//! let mut encoder = CountingZlibEncoder::new(CompressionLevel::Default);
//! encoder.write(b"payload").unwrap();
//! let compressed_len = encoder.finish().unwrap();
//! assert!(compressed_len > 0);
//! ```
//!
//! Obtain a compressed buffer, stream it through
//! [`CountingZlibDecoder`], and collect the decompressed output:
//!
//! ```
//! use compress::zlib::{
//!     compress_to_vec, CompressionLevel, CountingZlibDecoder, CountingZlibEncoder,
//! };
//! use std::io::Read;
//!
//! let mut encoder = CountingZlibEncoder::new(CompressionLevel::Best);
//! encoder.write(b"highly compressible payload").unwrap();
//! let compressed_len = encoder.finish().unwrap();
//!
//! let compressed = compress_to_vec(b"highly compressible payload", CompressionLevel::Best)
//!     .unwrap();
//! let mut decoder = CountingZlibDecoder::new(&compressed[..]);
//! let mut decoded = Vec::new();
//! decoder.read_to_end(&mut decoded).unwrap();
//! assert_eq!(decoded, b"highly compressible payload");
//! assert_eq!(decoder.bytes_read(), decoded.len() as u64);
//! assert!(compressed_len as usize <= compressed.len());
//! ```
//!
//! Forward compressed bytes into a caller-provided sink while tracking the
//! compressed length:
//!
//! ```
//! use compress::zlib::{CompressionLevel, CountingZlibEncoder};
//! use std::io::Write;
//!
//! let mut encoder = CountingZlibEncoder::with_sink(Vec::new(), CompressionLevel::Default);
//! encoder.write_all(b"payload").unwrap();
//! let (compressed, bytes) = encoder.finish_into_inner().unwrap();
//! assert_eq!(bytes as usize, compressed.len());
//! ```

use std::fmt;
use std::io::{self, IoSlice, IoSliceMut, Read, Write};
use std::num::NonZeroU8;

use thiserror::Error;

use crate::common::{CountingSink, CountingWriter};
use flate2::{
    Compression,
    read::{DeflateDecoder, DeflateDecoder as FlateDecoder},
    write::DeflateEncoder as FlateEncoder,
};

/// Compression levels recognised by the zlib encoder.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompressionLevel {
    /// No compression (level 0) - data is stored without deflation.
    None,
    /// Favour speed over compression ratio.
    Fast,
    /// Use zlib's default balance between speed and ratio.
    Default,
    /// Favour the best possible compression ratio.
    Best,
    /// Use an explicit zlib compression level in the range `1..=9`.
    Precise(NonZeroU8),
}

impl CompressionLevel {
    /// Creates a [`CompressionLevel`] value from an explicit numeric level.
    ///
    /// Level 0 returns [`CompressionLevel::None`] (no compression).
    /// Levels 1-9 return [`CompressionLevel::Precise`].
    ///
    /// # Errors
    ///
    /// Returns [`CompressionLevelError`] when `level` falls outside the inclusive
    /// range `0..=9` accepted by zlib.
    pub fn from_numeric(level: u32) -> Result<Self, CompressionLevelError> {
        if level > 9 {
            return Err(CompressionLevelError::new(level));
        }

        if level == 0 {
            return Ok(Self::None);
        }

        let as_u8 = u8::try_from(level).map_err(|_| CompressionLevelError::new(level))?;
        let precise = NonZeroU8::new(as_u8).ok_or_else(|| CompressionLevelError::new(level))?;
        Ok(Self::Precise(precise))
    }

    /// Constructs a [`CompressionLevel::Precise`] variant from the provided zlib level.
    #[must_use]
    pub const fn precise(level: NonZeroU8) -> Self {
        Self::Precise(level)
    }
}

impl From<CompressionLevel> for Compression {
    fn from(level: CompressionLevel) -> Self {
        match level {
            CompressionLevel::None => Compression::none(),
            CompressionLevel::Fast => Compression::fast(),
            CompressionLevel::Default => Compression::default(),
            CompressionLevel::Best => Compression::best(),
            CompressionLevel::Precise(value) => Compression::new(u32::from(value.get())),
        }
    }
}

/// Error returned when a requested compression level falls outside the
/// permissible zlib range.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Error)]
#[error("compression level {level} is outside the supported range 0-9")]
pub struct CompressionLevelError {
    level: u32,
}

impl CompressionLevelError {
    /// Creates a new error capturing the unsupported compression level.
    const fn new(level: u32) -> Self {
        Self { level }
    }

    /// Returns the invalid compression level that triggered the error.
    #[must_use]
    pub const fn level(&self) -> u32 {
        self.level
    }
}

/// Streaming encoder that records the number of compressed bytes produced.
///
/// The encoder implements [`std::io::Write`], enabling integration with APIs
/// such as [`std::io::copy`], [`write!`](std::write), and
/// [`std::io::Write::write_all`]. By default compressed bytes are discarded
/// after being counted, matching upstream rsync's bandwidth accounting path
/// where the payload itself is forwarded separately. Callers that need to
/// forward the compressed stream can construct the encoder with an explicit
/// sink via [`CountingZlibEncoder::with_sink`] so the counted bytes are written
/// into the provided writer.
pub struct CountingZlibEncoder<W = CountingSink>
where
    W: Write,
{
    inner: FlateEncoder<CountingWriter<W>>,
}

impl CountingZlibEncoder<CountingSink> {
    /// Creates a new encoder that counts the compressed output produced by zlib.
    #[must_use]
    pub fn new(level: CompressionLevel) -> Self {
        Self::with_sink(CountingSink, level)
    }

    /// Completes the stream and returns the total number of compressed bytes generated.
    pub fn finish(self) -> io::Result<u64> {
        let (_sink, bytes) = self.finish_into_inner()?;
        Ok(bytes)
    }
}

impl<W> CountingZlibEncoder<W>
where
    W: Write,
{
    /// Creates a new encoder that forwards compressed bytes into `sink`.
    #[must_use]
    pub fn with_sink(sink: W, level: CompressionLevel) -> Self {
        Self {
            inner: FlateEncoder::new(CountingWriter::new(sink), level.into()),
        }
    }

    /// Appends data to the compression stream.
    pub fn write(&mut self, input: &[u8]) -> io::Result<()> {
        self.inner.write_all(input)
    }

    /// Returns the number of compressed bytes produced so far without finalising the stream.
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

    /// Completes the stream, returning the sink and the total number of compressed bytes produced.
    ///
    /// # Errors
    ///
    /// Propagates any I/O errors reported by the underlying writer or zlib
    /// during stream finalisation.
    pub fn finish_into_inner(self) -> io::Result<(W, u64)> {
        let writer = self.inner.finish()?;
        Ok(writer.into_parts())
    }
}

impl<W> Write for CountingZlibEncoder<W>
where
    W: Write,
{
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.inner.write(buf)
    }

    fn write_vectored(&mut self, bufs: &[IoSlice<'_>]) -> io::Result<usize> {
        self.inner.write_vectored(bufs)
    }

    fn write_all(&mut self, buf: &[u8]) -> io::Result<()> {
        self.inner.write_all(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }

    fn write_fmt(&mut self, fmt: fmt::Arguments<'_>) -> io::Result<()> {
        self.inner.write_fmt(fmt)
    }
}

/// Streaming decoder that records the number of decompressed bytes produced.
pub struct CountingZlibDecoder<R> {
    inner: DeflateDecoder<R>,
    bytes: u64,
}

impl<R> CountingZlibDecoder<R>
where
    R: Read,
{
    /// Creates a new decoder that wraps the provided reader.
    #[must_use]
    pub fn new(reader: R) -> Self {
        Self {
            inner: DeflateDecoder::new(reader),
            bytes: 0,
        }
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
        self.inner.get_mut()
    }

    /// Returns an immutable reference to the wrapped reader.
    #[inline]
    #[must_use]
    pub fn get_ref(&self) -> &R {
        self.inner.get_ref()
    }

    /// Consumes the decoder and returns the wrapped reader.
    #[inline]
    #[must_use]
    pub fn into_inner(self) -> R {
        self.inner.into_inner()
    }
}

impl<R> Read for CountingZlibDecoder<R>
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
    let mut encoder = FlateEncoder::new(Vec::new(), level.into());
    encoder.write_all(input)?;
    encoder.finish()
}

/// Decompresses `input` into a new [`Vec`].
pub fn decompress_to_vec(input: &[u8]) -> io::Result<Vec<u8>> {
    let mut decoder = FlateDecoder::new(input);
    let mut output = Vec::new();
    io::copy(&mut decoder, &mut output)?;
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::Compression;
    use std::io::{Cursor, IoSliceMut, Read};

    #[test]
    fn counting_encoder_tracks_bytes() {
        let mut encoder = CountingZlibEncoder::new(CompressionLevel::Default);
        encoder.write(b"payload").expect("compress payload");
        let compressed = encoder.finish().expect("finish stream");
        assert!(compressed > 0);
    }

    #[test]
    fn counting_encoder_reports_incremental_bytes() {
        let mut encoder = CountingZlibEncoder::new(CompressionLevel::Default);
        assert_eq!(encoder.bytes_written(), 0);
        encoder.write(b"payload").expect("compress payload");
        let after_first = encoder.bytes_written();
        encoder.write(b"more payload").expect("compress payload");
        let after_second = encoder.bytes_written();
        assert!(after_second >= after_first);
        let final_len = encoder.finish().expect("finish stream");
        assert!(final_len >= after_second);
    }

    #[test]
    fn streaming_round_trip_preserves_payload() {
        let mut encoder = CountingZlibEncoder::new(CompressionLevel::Default);
        let input = b"The quick brown fox jumps over the lazy dog".repeat(8);
        for chunk in input.chunks(11) {
            encoder.write(chunk).expect("write chunk");
        }
        let compressed_len = encoder.finish().expect("finish stream");
        assert!(compressed_len > 0);

        let compressed = compress_to_vec(&input, CompressionLevel::Default).expect("compress");
        assert!(compressed.len() as u64 >= compressed_len);
        let decompressed = decompress_to_vec(&compressed).expect("decompress");
        assert_eq!(decompressed, input);
    }

    #[test]
    fn counting_encoder_supports_write_trait() {
        let mut encoder = CountingZlibEncoder::new(CompressionLevel::Default);
        write!(&mut encoder, "payload").expect("write via trait");
        encoder.flush().expect("flush encoder");
        let compressed = encoder.finish().expect("finish stream");
        assert!(compressed > 0);
    }

    #[test]
    fn counting_encoder_supports_vectored_writes() {
        let mut encoder = CountingZlibEncoder::new(CompressionLevel::Default);
        let buffers = [IoSlice::new(b"foo"), IoSlice::new(b"bar")];

        let written = encoder
            .write_vectored(&buffers)
            .expect("vectored write succeeds");
        if written < 6 {
            encoder
                .write_all(&b"foobar"[written..])
                .expect("write remaining data");
        }

        let compressed = encoder.finish().expect("finish stream");
        assert!(compressed > 0);
    }

    #[test]
    fn helper_functions_round_trip() {
        let payload = b"highly compressible payload";
        let compressed = compress_to_vec(payload, CompressionLevel::Best).expect("compress");
        let decoded = decompress_to_vec(&compressed).expect("decompress");
        assert_eq!(decoded, payload);
    }

    #[test]
    fn counting_encoder_forwards_to_sink() {
        let mut encoder = CountingZlibEncoder::with_sink(Vec::new(), CompressionLevel::Default);
        encoder.write(b"payload").expect("compress payload");
        let (sink, bytes) = encoder
            .finish_into_inner()
            .expect("finish compression stream");
        assert!(bytes > 0);
        assert!(!sink.is_empty());
        let decoded = decompress_to_vec(&sink).expect("decompress");
        assert_eq!(decoded, b"payload");
    }

    #[test]
    fn counting_encoder_exposes_sink_references() {
        let mut encoder = CountingZlibEncoder::with_sink(Vec::new(), CompressionLevel::Default);
        assert!(encoder.get_ref().is_empty());

        encoder.get_mut().extend_from_slice(b"prefix");
        assert_eq!(encoder.get_ref(), b"prefix");

        encoder.write_all(b"payload").expect("compress payload");
        let (sink, bytes) = encoder
            .finish_into_inner()
            .expect("finish compression stream");

        assert!(bytes > 0);
        assert!(sink.starts_with(b"prefix"));
        assert_eq!(bytes as usize, sink.len() - b"prefix".len());
    }

    #[test]
    fn counting_decoder_tracks_output_bytes() {
        let payload = b"streaming decoder payload";
        let compressed = compress_to_vec(payload, CompressionLevel::Default).expect("compress");
        let mut decoder = CountingZlibDecoder::new(Cursor::new(compressed));
        let mut output = Vec::new();
        decoder.read_to_end(&mut output).expect("decompress");
        assert_eq!(output, payload);
        assert_eq!(decoder.bytes_read(), payload.len() as u64);
    }

    #[test]
    fn counting_decoder_vectored_reads_update_byte_count() {
        let payload = b"Vectored read payload repeated".repeat(4);
        let compressed = compress_to_vec(&payload, CompressionLevel::Default).expect("compress");
        let mut decoder = CountingZlibDecoder::new(Cursor::new(compressed));
        let mut first = [0u8; 13];
        let mut second = [0u8; 21];
        let mut buffers = [IoSliceMut::new(&mut first), IoSliceMut::new(&mut second)];
        let read = decoder
            .read_vectored(&mut buffers)
            .expect("vectored read succeeds");
        assert!(read > 0);

        let mut collected = Vec::with_capacity(read);
        let first_len = read.min(first.len());
        collected.extend_from_slice(&first[..first_len]);
        if read > first_len {
            let second_len = read - first_len;
            collected.extend_from_slice(&second[..second_len]);
        }

        assert_eq!(collected, payload[..read]);
        assert_eq!(decoder.bytes_read(), read as u64);
    }

    #[test]
    fn counting_decoder_exposes_reader_accessors() {
        let payload = b"reader accessor payload";
        let compressed = compress_to_vec(payload, CompressionLevel::Default).expect("compress");
        let cursor = Cursor::new(compressed);
        let mut decoder = CountingZlibDecoder::new(cursor);

        assert_eq!(decoder.get_ref().position(), 0);
        decoder.get_mut().set_position(2);
        assert_eq!(decoder.get_ref().position(), 2);

        let inner = decoder.into_inner();
        assert_eq!(inner.position(), 2);
    }

    #[test]
    fn precise_level_converts_to_requested_value() {
        let level = NonZeroU8::new(7).expect("non-zero");
        let compression = Compression::from(CompressionLevel::precise(level));
        assert_eq!(compression.level(), u32::from(level.get()));
    }

    #[test]
    fn numeric_level_constructor_accepts_valid_range() {
        for level in 1..=9 {
            let precise = CompressionLevel::from_numeric(level).expect("valid level");
            let expected = NonZeroU8::new(level as u8).expect("range checked");
            assert_eq!(precise, CompressionLevel::Precise(expected));
        }
    }

    #[test]
    fn numeric_level_constructor_rejects_out_of_range() {
        let err = CompressionLevel::from_numeric(10).expect_err("level above 9 rejected");
        assert_eq!(err.level(), 10);
    }

    #[test]
    fn counting_writer_saturating_add_prevents_overflow() {
        let mut writer = CountingWriter::new(CountingSink);
        writer.saturating_add_bytes(usize::MAX);
        writer.saturating_add_bytes(usize::MAX);
        assert_eq!(writer.bytes(), u64::MAX);
    }

    #[test]
    fn zero_byte_roundtrip() {
        // Empty input should compress and decompress to empty output
        let compressed = compress_to_vec(b"", CompressionLevel::Default).expect("compress empty");
        let decompressed = decompress_to_vec(&compressed).expect("decompress empty");
        assert!(decompressed.is_empty());
    }

    #[test]
    fn zero_byte_streaming_roundtrip() {
        // Streaming encoder with no writes should produce valid empty stream
        let encoder = CountingZlibEncoder::with_sink(Vec::new(), CompressionLevel::Default);
        // Don't write anything
        let (compressed, bytes) = encoder.finish_into_inner().expect("finish empty stream");
        assert!(bytes > 0, "deflate stream has framing even when empty");

        let decompressed = decompress_to_vec(&compressed).expect("decompress empty stream");
        assert!(decompressed.is_empty());
    }

    #[test]
    fn compression_level_1_compresses_successfully() {
        let level = CompressionLevel::from_numeric(1).expect("level 1 is valid");
        let payload = b"The quick brown fox jumps over the lazy dog".repeat(10);
        let compressed = compress_to_vec(&payload, level).expect("compress with level 1");
        assert!(!compressed.is_empty());
        let decompressed = decompress_to_vec(&compressed).expect("decompress level 1");
        assert_eq!(decompressed, payload);
    }

    #[test]
    fn compression_level_2_compresses_successfully() {
        let level = CompressionLevel::from_numeric(2).expect("level 2 is valid");
        let payload = b"The quick brown fox jumps over the lazy dog".repeat(10);
        let compressed = compress_to_vec(&payload, level).expect("compress with level 2");
        assert!(!compressed.is_empty());
        let decompressed = decompress_to_vec(&compressed).expect("decompress level 2");
        assert_eq!(decompressed, payload);
    }

    #[test]
    fn compression_level_3_compresses_successfully() {
        let level = CompressionLevel::from_numeric(3).expect("level 3 is valid");
        let payload = b"The quick brown fox jumps over the lazy dog".repeat(10);
        let compressed = compress_to_vec(&payload, level).expect("compress with level 3");
        assert!(!compressed.is_empty());
        let decompressed = decompress_to_vec(&compressed).expect("decompress level 3");
        assert_eq!(decompressed, payload);
    }

    #[test]
    fn compression_level_4_compresses_successfully() {
        let level = CompressionLevel::from_numeric(4).expect("level 4 is valid");
        let payload = b"The quick brown fox jumps over the lazy dog".repeat(10);
        let compressed = compress_to_vec(&payload, level).expect("compress with level 4");
        assert!(!compressed.is_empty());
        let decompressed = decompress_to_vec(&compressed).expect("decompress level 4");
        assert_eq!(decompressed, payload);
    }

    #[test]
    fn compression_level_5_compresses_successfully() {
        let level = CompressionLevel::from_numeric(5).expect("level 5 is valid");
        let payload = b"The quick brown fox jumps over the lazy dog".repeat(10);
        let compressed = compress_to_vec(&payload, level).expect("compress with level 5");
        assert!(!compressed.is_empty());
        let decompressed = decompress_to_vec(&compressed).expect("decompress level 5");
        assert_eq!(decompressed, payload);
    }

    #[test]
    fn compression_level_6_compresses_successfully() {
        let level = CompressionLevel::from_numeric(6).expect("level 6 is valid");
        let payload = b"The quick brown fox jumps over the lazy dog".repeat(10);
        let compressed = compress_to_vec(&payload, level).expect("compress with level 6");
        assert!(!compressed.is_empty());
        let decompressed = decompress_to_vec(&compressed).expect("decompress level 6");
        assert_eq!(decompressed, payload);
    }

    #[test]
    fn compression_level_7_compresses_successfully() {
        let level = CompressionLevel::from_numeric(7).expect("level 7 is valid");
        let payload = b"The quick brown fox jumps over the lazy dog".repeat(10);
        let compressed = compress_to_vec(&payload, level).expect("compress with level 7");
        assert!(!compressed.is_empty());
        let decompressed = decompress_to_vec(&compressed).expect("decompress level 7");
        assert_eq!(decompressed, payload);
    }

    #[test]
    fn compression_level_8_compresses_successfully() {
        let level = CompressionLevel::from_numeric(8).expect("level 8 is valid");
        let payload = b"The quick brown fox jumps over the lazy dog".repeat(10);
        let compressed = compress_to_vec(&payload, level).expect("compress with level 8");
        assert!(!compressed.is_empty());
        let decompressed = decompress_to_vec(&compressed).expect("decompress level 8");
        assert_eq!(decompressed, payload);
    }

    #[test]
    fn compression_level_9_compresses_successfully() {
        let level = CompressionLevel::from_numeric(9).expect("level 9 is valid");
        let payload = b"The quick brown fox jumps over the lazy dog".repeat(10);
        let compressed = compress_to_vec(&payload, level).expect("compress with level 9");
        assert!(!compressed.is_empty());
        let decompressed = decompress_to_vec(&compressed).expect("decompress level 9");
        assert_eq!(decompressed, payload);
    }

    #[test]
    fn higher_compression_levels_produce_smaller_output() {
        // Test with highly compressible data
        let payload = b"AAAAAAAAAA".repeat(100);

        // Collect sizes for all levels
        let mut sizes = Vec::new();
        for level in 1..=9 {
            let compression_level =
                CompressionLevel::from_numeric(level).expect("levels 1-9 are valid");
            let compressed =
                compress_to_vec(&payload, compression_level).expect("compression succeeds");
            sizes.push((level, compressed.len()));
        }

        // Level 9 (best compression) should produce smaller output than level 1 (fast)
        // for highly compressible data
        let level1_size = sizes[0].1;
        let level9_size = sizes[8].1;
        assert!(
            level9_size < level1_size,
            "level 9 ({level9_size} bytes) should be smaller than level 1 ({level1_size} bytes)"
        );

        // Level 5 should be no larger than level 1 (intermediate levels should improve or match)
        let level5_size = sizes[4].1;
        assert!(
            level5_size <= level1_size,
            "level 5 ({level5_size} bytes) should be <= level 1 ({level1_size} bytes)"
        );

        // Level 9 should be no larger than level 5
        assert!(
            level9_size <= level5_size,
            "level 9 ({level9_size} bytes) should be <= level 5 ({level5_size} bytes)"
        );
    }

    #[test]
    fn all_levels_roundtrip_correctly() {
        let payload = b"Test payload with various characters: 123!@# ABC xyz".repeat(20);

        for level in 1..=9 {
            let compression_level =
                CompressionLevel::from_numeric(level).expect("levels 1-9 are valid");
            let compressed =
                compress_to_vec(&payload, compression_level).expect("compression succeeds");
            let decompressed = decompress_to_vec(&compressed).expect("decompression succeeds");

            assert_eq!(
                decompressed, payload,
                "level {level} failed to roundtrip correctly"
            );
        }
    }

    #[test]
    fn all_levels_handle_empty_input() {
        for level in 1..=9 {
            let compression_level =
                CompressionLevel::from_numeric(level).expect("levels 1-9 are valid");
            let compressed = compress_to_vec(b"", compression_level).expect("compress empty input");
            let decompressed = decompress_to_vec(&compressed).expect("decompress empty input");

            assert!(
                decompressed.is_empty(),
                "level {level} failed to handle empty input"
            );
        }
    }

    #[test]
    fn all_levels_handle_single_byte() {
        for level in 1..=9 {
            let compression_level =
                CompressionLevel::from_numeric(level).expect("levels 1-9 are valid");
            let payload = b"X";
            let compressed =
                compress_to_vec(payload, compression_level).expect("compress single byte");
            let decompressed = decompress_to_vec(&compressed).expect("decompress single byte");

            assert_eq!(
                decompressed, payload,
                "level {level} failed to handle single byte"
            );
        }
    }

    #[test]
    fn all_levels_handle_incompressible_data() {
        // Random-looking data that compresses poorly
        let payload: Vec<u8> = (0..256).map(|i| (i * 137 + 73) as u8).collect();

        for level in 1..=9 {
            let compression_level =
                CompressionLevel::from_numeric(level).expect("levels 1-9 are valid");
            let compressed =
                compress_to_vec(&payload, compression_level).expect("compress incompressible data");
            let decompressed =
                decompress_to_vec(&compressed).expect("decompress incompressible data");

            assert_eq!(
                decompressed, payload,
                "level {level} failed with incompressible data"
            );

            // Incompressible data may actually expand slightly due to framing overhead
            // Just verify it didn't balloon unreasonably
            assert!(
                compressed.len() < payload.len() * 2,
                "level {level} produced unreasonably large output for incompressible data"
            );
        }
    }

    #[test]
    fn all_levels_work_with_counting_encoder() {
        let payload = b"Counting encoder test payload".repeat(5);

        for level in 1..=9 {
            let compression_level =
                CompressionLevel::from_numeric(level).expect("levels 1-9 are valid");

            let mut encoder = CountingZlibEncoder::with_sink(Vec::new(), compression_level);
            encoder.write(&payload).expect("write to encoder");
            let (compressed, bytes_written) = encoder.finish_into_inner().expect("finish encoder");

            assert_eq!(
                bytes_written as usize,
                compressed.len(),
                "level {level} counting encoder byte count mismatch"
            );

            let decompressed = decompress_to_vec(&compressed).expect("decompress");
            assert_eq!(
                decompressed, payload,
                "level {level} counting encoder roundtrip failed"
            );
        }
    }
}
