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

    /// Flushes the encoder, emitting all buffered data as a complete LZ4 block.
    ///
    /// This calls `flush()` on the underlying `FrameEncoder`, which writes any
    /// buffered uncompressed data as a compressed LZ4 block to the sink. The
    /// receiver can then decompress all data written so far without waiting for
    /// more input.
    ///
    /// This provides per-token flush semantics analogous to zlib's `Z_SYNC_FLUSH`.
    /// Each flush boundary produces output that, combined with the frame header,
    /// allows incremental decompression up to that point.
    ///
    /// # Upstream Reference
    ///
    /// See `token.c:send_deflated_token()` - upstream rsync flushes the
    /// compressor after each token so the receiver can decompress incrementally.
    pub fn flush(&mut self) -> io::Result<()> {
        self.inner.flush().map_err(io::Error::other)
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

/// Decompresses `input` directly into `output`, avoiding intermediate allocation.
pub fn decompress_into(input: &[u8], output: &mut Vec<u8>) -> io::Result<usize> {
    let initial_len = output.len();
    let mut decoder = FrameDecoder::new(input);
    io::copy(&mut decoder, output)?;
    Ok(output.len() - initial_len)
}

fn frame_info_for_level(level: CompressionLevel) -> FrameInfo {
    let block_size = match level {
        CompressionLevel::None => BlockSize::Max64KB,
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

    // Per-token flush tests - verify that flush produces output enabling
    // incremental decompression, matching the upstream per-token pattern
    // (token.c:send_deflated_token).

    #[test]
    fn flush_emits_compressed_block() {
        let mut encoder = CountingLz4Encoder::with_sink(Vec::new(), CompressionLevel::Default);
        encoder.write(b"token payload data").expect("write");

        // Before flush, the encoder may buffer data internally
        let before_flush = encoder.get_ref().len();

        encoder.flush().expect("flush");

        // After flush, compressed data must appear in the sink
        let after_flush = encoder.get_ref().len();
        assert!(
            after_flush > before_flush,
            "flush must emit compressed data to the sink (before={before_flush}, after={after_flush})"
        );
    }

    #[test]
    fn flush_then_finish_produces_valid_frame() {
        let mut encoder = CountingLz4Encoder::with_sink(Vec::new(), CompressionLevel::Default);

        let token1 = b"first token data for flush test";
        encoder.write(token1).expect("write token 1");
        encoder.flush().expect("flush after token 1");

        let token2 = b"second token after flush";
        encoder.write(token2).expect("write token 2");

        let (compressed, bytes) = encoder.finish_into_inner().expect("finish");
        assert!(bytes > 0, "should have produced compressed output");

        // The entire stream must decompress to both tokens concatenated
        let decompressed = decompress_to_vec(&compressed).expect("decompress");
        let mut expected = Vec::new();
        expected.extend_from_slice(token1);
        expected.extend_from_slice(token2);
        assert_eq!(decompressed, expected);
    }

    #[test]
    fn flush_after_each_token_all_data_recoverable() {
        // Simulates upstream per-token pattern: write data, flush, repeat.
        // Verifies that all tokens are recoverable after stream finalization.
        let tokens: &[&[u8]] = &[
            b"token one: file header metadata",
            b"token two: delta literal run",
            b"token three: block match copy",
            b"token four: final literal",
        ];

        let mut encoder = CountingLz4Encoder::with_sink(Vec::new(), CompressionLevel::Default);

        for token in tokens {
            encoder.write(token).expect("write token");
            encoder.flush().expect("flush after token");
        }

        let (compressed, _bytes) = encoder.finish_into_inner().expect("finish");

        // All tokens must decompress correctly
        let decompressed = decompress_to_vec(&compressed).expect("decompress");
        let expected: Vec<u8> = tokens.iter().flat_map(|t| t.iter().copied()).collect();
        assert_eq!(decompressed, expected);
    }

    #[test]
    fn flush_on_empty_buffer_is_noop() {
        let mut encoder = CountingLz4Encoder::with_sink(Vec::new(), CompressionLevel::Default);

        // Flush with no data written should not error
        encoder.flush().expect("flush on empty");

        let before = encoder.get_ref().len();

        // Another flush should also be fine
        encoder.flush().expect("second flush on empty");
        let after = encoder.get_ref().len();

        // No data should have been emitted (or at most frame header bytes)
        assert_eq!(
            before, after,
            "empty flush should not produce additional output"
        );
    }

    #[test]
    fn flush_produces_incrementally_decompressible_output() {
        // Key test: after writing token1 and flushing, the compressed output
        // so far (once finalized into a complete frame) must contain token1's
        // data. This validates that flush materializes buffered data rather
        // than holding it in the encoder's internal state.
        let mut encoder = CountingLz4Encoder::with_sink(Vec::new(), CompressionLevel::Default);

        let token1 = b"incremental decompression test - token one data payload";
        encoder.write(token1).expect("write token 1");
        encoder.flush().expect("flush after token 1");

        // Capture sink state after flush - this has compressed blocks
        let after_token1_len = encoder.get_ref().len();
        assert!(
            after_token1_len > 0,
            "flush must produce output in the sink"
        );

        let token2 = b"incremental decompression test - token two";
        encoder.write(token2).expect("write token 2");

        let (compressed, _) = encoder.finish_into_inner().expect("finish");
        let decompressed = decompress_to_vec(&compressed).expect("decompress");

        let mut expected = Vec::new();
        expected.extend_from_slice(token1);
        expected.extend_from_slice(token2);
        assert_eq!(decompressed, expected);
    }

    #[test]
    fn flush_bytes_written_increases() {
        let mut encoder = CountingLz4Encoder::with_sink(Vec::new(), CompressionLevel::Default);

        encoder.write(b"data for byte counting").expect("write");
        let before = encoder.bytes_written();

        encoder.flush().expect("flush");
        let after = encoder.bytes_written();

        assert!(
            after > before,
            "bytes_written must increase after flush (before={before}, after={after})"
        );
    }

    #[test]
    fn multiple_flush_cycles_with_varying_sizes() {
        // Test flush with different token sizes to exercise block boundaries
        let mut encoder = CountingLz4Encoder::with_sink(Vec::new(), CompressionLevel::Fast);
        let mut expected = Vec::new();

        // Small token
        let small = b"x";
        encoder.write(small).expect("write small");
        encoder.flush().expect("flush small");
        expected.extend_from_slice(small);

        // Medium token
        let medium = vec![b'M'; 4096];
        encoder.write(&medium).expect("write medium");
        encoder.flush().expect("flush medium");
        expected.extend_from_slice(&medium);

        // Large token (exceeds typical block size)
        let large = vec![b'L'; 65536];
        encoder.write(&large).expect("write large");
        encoder.flush().expect("flush large");
        expected.extend_from_slice(&large);

        // Empty write + flush
        encoder.write(b"").expect("write empty");
        encoder.flush().expect("flush empty");

        let (compressed, _) = encoder.finish_into_inner().expect("finish");
        let decompressed = decompress_to_vec(&compressed).expect("decompress");
        assert_eq!(decompressed, expected);
    }

    #[test]
    fn counting_sink_encoder_flush() {
        // Verify flush works with the CountingSink (discard) variant
        let mut encoder = CountingLz4Encoder::new(CompressionLevel::Default);
        encoder
            .write(b"data for counting sink flush")
            .expect("write");
        encoder.flush().expect("flush with counting sink");

        let bytes = encoder.finish().expect("finish");
        assert!(bytes > 0, "counting sink should report compressed bytes");
    }
}
