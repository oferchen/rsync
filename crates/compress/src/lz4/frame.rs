#![allow(clippy::module_name_repetitions)]

//! LZ4 frame format compression.
//!
//! This module provides streaming LZ4 compression using the standard LZ4 frame
//! format. The frame format includes magic bytes, checksums, and supports
//! streaming across multiple blocks.
//!
//! **Note**: This format is NOT compatible with upstream rsync's wire protocol.
//! For wire protocol compatibility, use the [`super::raw`] module instead.

use std::io::{self, BufReader, IoSliceMut, Read, Write};

use crate::common::{CountingSink, CountingWriter};
use crate::zlib::CompressionLevel;
use lz4_flex::frame::{BlockMode, BlockSize, FrameDecoder, FrameEncoder, FrameInfo};

/// Streaming encoder that records the number of compressed bytes produced.
pub struct CountingLz4Encoder<W = CountingSink>
where
    W: Write,
{
    inner: FrameEncoder<CountingWriter<W>>,
}

impl CountingLz4Encoder<CountingSink> {
    /// Creates a new encoder that discards compressed output while tracking its length.
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

impl<W> CountingLz4Encoder<W>
where
    W: Write,
{
    /// Creates a new encoder that writes compressed bytes into `sink`.
    #[must_use]
    pub fn with_sink(sink: W, level: CompressionLevel) -> Self {
        let writer = CountingWriter::new(sink);
        let frame_info = frame_info_for_level(level);
        let encoder = FrameEncoder::with_frame_info(frame_info, writer);
        Self { inner: encoder }
    }

    /// Appends data to the compression stream.
    pub fn write(&mut self, input: &[u8]) -> io::Result<()> {
        self.inner.write_all(input).map_err(io::Error::other)
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
pub struct CountingLz4Decoder<R>
where
    R: Read,
{
    inner: FrameDecoder<BufReader<R>>,
    bytes: u64,
}

impl<R> CountingLz4Decoder<R>
where
    R: Read,
{
    /// Creates a new decoder that wraps the provided reader.
    #[must_use]
    pub fn new(reader: R) -> Self {
        Self {
            inner: FrameDecoder::new(BufReader::new(reader)),
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
        self.inner.into_inner().into_inner()
    }
}

impl<R> Read for CountingLz4Decoder<R>
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

/// Compresses `input` into a new [`Vec`] using LZ4 frame format.
pub fn compress_to_vec(input: &[u8], level: CompressionLevel) -> io::Result<Vec<u8>> {
    let frame_info = frame_info_for_level(level);
    let mut encoder = FrameEncoder::with_frame_info(frame_info, Vec::new());
    encoder.write_all(input).map_err(io::Error::other)?;
    encoder.finish().map_err(io::Error::other)
}

/// Decompresses `input` from LZ4 frame format into a new [`Vec`].
pub fn decompress_to_vec(input: &[u8]) -> io::Result<Vec<u8>> {
    let mut decoder = FrameDecoder::new(input);
    let mut output = Vec::new();
    io::copy(&mut decoder, &mut output)?;
    Ok(output)
}

fn frame_info_for_level(level: CompressionLevel) -> FrameInfo {
    let block_size = match level {
        CompressionLevel::Fast => BlockSize::Max64KB,
        CompressionLevel::Default => BlockSize::Max256KB,
        CompressionLevel::Best => BlockSize::Max4MB,
        CompressionLevel::Precise(value) => match value.get() {
            1..=3 => BlockSize::Max64KB,
            4..=6 => BlockSize::Max256KB,
            7..=8 => BlockSize::Max1MB,
            _ => BlockSize::Max4MB,
        },
    };

    FrameInfo::new()
        .block_mode(BlockMode::Linked)
        .block_size(block_size)
        .content_checksum(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn counting_encoder_tracks_bytes() {
        let mut encoder = CountingLz4Encoder::new(CompressionLevel::Default);
        encoder.write(b"payload").expect("compress payload");
        let compressed = encoder.finish().expect("finish stream");
        assert!(compressed > 0);
    }

    #[test]
    fn encoder_with_sink_forwards_bytes() {
        let mut encoder = CountingLz4Encoder::with_sink(Vec::new(), CompressionLevel::Default);
        encoder.write(b"payload").expect("compress payload");
        let (compressed, bytes) = encoder.finish_into_inner().expect("finish stream");
        assert_eq!(bytes as usize, compressed.len());
    }

    #[test]
    fn decoder_tracks_bytes() {
        let payload = b"highly compressible payload";
        let compressed = compress_to_vec(payload, CompressionLevel::Best).expect("compress");
        let mut decoder = CountingLz4Decoder::new(&compressed[..]);
        let mut decoded = Vec::new();
        decoder
            .read_to_end(&mut decoded)
            .expect("decompress payload");
        assert_eq!(decoded, payload);
        assert_eq!(decoder.bytes_read(), payload.len() as u64);
    }

    #[test]
    fn decompress_round_trip_matches_input() {
        let payload = b"block oriented data";
        let compressed = compress_to_vec(payload, CompressionLevel::Fast).expect("compress");
        let restored = decompress_to_vec(&compressed).expect("decompress");
        assert_eq!(restored, payload);
    }

    #[test]
    fn decompress_detects_corrupted_frame() {
        let payload = b"test data for checksum verification";
        let mut compressed = compress_to_vec(payload, CompressionLevel::Default).expect("compress");

        // Corrupt a byte in the middle of the frame (after the header)
        // LZ4 frames have: magic (4) + frame descriptor (2-15) + blocks + checksum (4)
        if compressed.len() > 10 {
            compressed[8] ^= 0xFF;
        }

        // Decompression should fail due to corruption
        let result = decompress_to_vec(&compressed);
        assert!(result.is_err(), "corrupted frame should fail decompression");
    }

    #[test]
    fn decompress_detects_truncated_checksum() {
        let payload = b"sufficient data for a complete frame with checksum";
        let compressed = compress_to_vec(payload, CompressionLevel::Default).expect("compress");

        // Truncate only the last byte of the checksum
        // The frame has content_checksum(true), so it needs the 4-byte checksum at the end
        if compressed.len() > 5 {
            let truncated = &compressed[..compressed.len() - 1];
            let result = decompress_to_vec(truncated);
            // Either error or wrong data - truncated checksums should fail validation
            if let Ok(decoded) = result {
                assert_ne!(decoded, payload, "truncated checksum should not match");
            }
        }
    }

    #[test]
    fn decompress_invalid_magic_returns_error() {
        // LZ4 frame magic is 0x184D2204
        let invalid_frame = [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];
        let result = decompress_to_vec(&invalid_frame);
        assert!(result.is_err(), "invalid magic should fail");
    }

    #[test]
    fn compress_empty_input_produces_valid_frame() {
        let compressed = compress_to_vec(&[], CompressionLevel::Default).expect("compress empty");
        // Valid LZ4 frame for empty input still has magic + descriptor + end marker
        assert!(
            !compressed.is_empty(),
            "empty input should produce frame header"
        );
        let restored = decompress_to_vec(&compressed).expect("decompress");
        assert!(restored.is_empty(), "empty input should round-trip");
    }
}
