//! Zstd per-token codec for compressed token wire format.
//!
//! Implements the zstd-specific encoder and decoder used by CPRES_ZSTD mode.
//! Unlike zlib, zstd does not use sync marker stripping/restoration, and
//! `see_token` is always a noop (no dictionary synchronization needed).
//!
//! ## Flush boundary alignment
//!
//! Upstream rsync uses `ZSTD_e_flush` at each token boundary (block match or
//! end-of-file). Literal data fed between token boundaries is compressed with
//! `ZSTD_e_continue` (no flush point). When a token arrives, the encoder
//! flushes, producing a decompressible boundary that the receiver can
//! decompress before processing the token.
//!
//! Compressed output is accumulated in a `MAX_DATA_COUNT`-sized buffer and
//! only written as a DEFLATED_DATA block when the buffer is full or a flush
//! completes. This matches upstream's single-buffer output pattern.
//!
//! - upstream: token.c:send_zstd_token() lines 678-776
//! - upstream: token.c:recv_zstd_token() lines 780-870

use std::io::{self, Read, Write};

use zstd::stream::raw::{Decoder as ZstdRawDecoder, Encoder as ZstdRawEncoder, Operation};

use super::{
    CHUNK_SIZE, CompressedToken, DEFLATED_DATA, END_FLAG, MAX_DATA_COUNT, TOKEN_LONG, TOKEN_REL,
    TOKENRUN_LONG, TOKENRUN_REL, read_deflated_data_length, write_deflated_data_header,
};

/// Zstd encoder state for sending compressed tokens.
///
/// Maintains a persistent `ZSTD_CCtx` across tokens for a file transfer.
/// Uses `ZSTD_e_flush` at token boundaries to produce decompressible output.
/// No sync marker stripping (unlike zlib).
///
/// Compressed output is accumulated in a `MAX_DATA_COUNT`-sized buffer.
/// A DEFLATED_DATA block is written only when the buffer is full (during
/// `ZSTD_e_continue`) or after each `ZSTD_e_flush` call, matching upstream's
/// output pattern in token.c:send_zstd_token().
///
/// Reference: upstream token.c:send_zstd_token()
pub(super) struct ZstdTokenEncoder {
    /// Persistent zstd compression context.
    encoder: ZstdRawEncoder<'static>,
    /// Output buffer for compression results.
    /// Sized to `MAX_DATA_COUNT` to match upstream's `obuf` (token.c line 695).
    output_buf: Vec<u8>,
    /// Current write position in `output_buf`.
    /// upstream: zstd_out_buff.pos
    output_pos: usize,
    /// Accumulated literal data pending compression.
    literal_buf: Vec<u8>,
    /// Last token sent (for run encoding).
    last_token: i32,
    /// Start of current token run.
    run_start: i32,
    /// End of last token run.
    last_run_end: i32,
    /// Whether data has been fed but not yet flushed.
    /// upstream: token.c line 680 flush_pending
    flush_pending: bool,
}

impl ZstdTokenEncoder {
    /// Creates a new zstd encoder with the specified compression level.
    pub(super) fn new(level: i32) -> io::Result<Self> {
        let encoder = ZstdRawEncoder::new(level)?;
        Ok(Self {
            encoder,
            output_buf: vec![0u8; MAX_DATA_COUNT],
            output_pos: 0,
            literal_buf: Vec::new(),
            last_token: -1,
            run_start: 0,
            last_run_end: 0,
            flush_pending: false,
        })
    }

    pub(super) fn reset(&mut self) -> io::Result<()> {
        self.encoder.reinit()?;
        self.literal_buf.clear();
        self.output_pos = 0;
        self.last_token = -1;
        self.run_start = 0;
        self.last_run_end = 0;
        self.flush_pending = false;
        Ok(())
    }

    pub(super) fn send_literal<W: Write>(
        &mut self,
        _writer: &mut W,
        data: &[u8],
    ) -> io::Result<()> {
        self.literal_buf.extend_from_slice(data);
        Ok(())
    }

    pub(super) fn send_block_match<W: Write>(
        &mut self,
        writer: &mut W,
        block_index: u32,
    ) -> io::Result<()> {
        let token = block_index as i32;
        let has_literals = !self.literal_buf.is_empty();

        // upstream: token.c lines 700-723 - same run encoding as zlib
        if self.last_token == -1 || self.last_token == -2 {
            self.compress_and_flush(writer)?;
            self.run_start = token;
        } else if has_literals || token != self.last_token + 1 || token >= self.run_start + 65536 {
            self.write_token_run(writer)?;
            self.compress_and_flush(writer)?;
            self.run_start = token;
        }

        self.last_token = token;
        Ok(())
    }

    pub(super) fn finish<W: Write>(&mut self, writer: &mut W) -> io::Result<()> {
        if self.last_token >= 0 {
            self.write_token_run(writer)?;
        }
        self.compress_and_flush(writer)?;
        writer.write_all(&[END_FLAG])?;
        self.reset()?;
        Ok(())
    }

    /// Noop for zstd - no dictionary synchronization needed.
    /// upstream: token.c:1102-1104 (see_token for CPRES_ZSTD is empty)
    pub(super) fn see_token(&mut self, _data: &[u8]) -> io::Result<()> {
        Ok(())
    }

    /// Compresses pending literals and flushes the zstd encoder.
    ///
    /// Mirrors upstream token.c lines 727-769. Feeds accumulated literals to
    /// the zstd encoder with `ZSTD_e_continue`, then performs `ZSTD_e_flush`
    /// to produce a decompressible boundary. Output is accumulated in a single
    /// `MAX_DATA_COUNT` buffer and written as DEFLATED_DATA blocks only when
    /// the buffer fills or on flush.
    ///
    /// upstream: token.c lines 727-769 (nb || flush_pending block)
    fn compress_and_flush<W: Write>(&mut self, writer: &mut W) -> io::Result<()> {
        if self.literal_buf.is_empty() && !self.flush_pending {
            return Ok(());
        }

        let input = std::mem::take(&mut self.literal_buf);

        // upstream: token.c lines 733-768
        // Feed input with ZSTD_e_continue, accumulating output.
        // Write DEFLATED_DATA only when the output buffer fills.
        let mut input_pos = 0;
        while input_pos < input.len() {
            // upstream: token.c lines 734-737 - reset buffer when exhausted
            if self.output_pos == MAX_DATA_COUNT {
                self.write_output_buffer(writer)?;
            }

            let mut in_buf = zstd::stream::raw::InBuffer::around(&input[input_pos..]);
            let mut out_buf =
                zstd::stream::raw::OutBuffer::around(&mut self.output_buf[self.output_pos..]);

            self.encoder.run(&mut in_buf, &mut out_buf)?;
            input_pos += in_buf.pos();
            self.output_pos += out_buf.pos();

            // upstream: token.c line 755 - write when buffer is full
            if self.output_pos == MAX_DATA_COUNT {
                self.write_output_buffer(writer)?;
            }
        }

        // upstream: token.c lines 740-743 - ZSTD_e_flush
        // Flush produces a decompressible boundary. After each flush call,
        // write whatever is in the output buffer (even if not full).
        loop {
            let mut out_buf =
                zstd::stream::raw::OutBuffer::around(&mut self.output_buf[self.output_pos..]);

            let remaining = self.encoder.flush(&mut out_buf)?;
            self.output_pos += out_buf.pos();

            // upstream: token.c line 755 - write when buffer full OR flushing
            if self.output_pos > 0 {
                self.write_output_buffer(writer)?;
            }

            if remaining == 0 {
                break;
            }
        }

        self.flush_pending = false;
        Ok(())
    }

    /// Writes the accumulated output buffer as a single DEFLATED_DATA block.
    ///
    /// Upstream writes the entire output buffer as one DEFLATED_DATA block
    /// (token.c lines 756-760), then resets the buffer for the next chunk.
    /// The DEFLATED_DATA header uses 14-bit length encoding, so the maximum
    /// block size is `MAX_DATA_COUNT` (16383 bytes).
    fn write_output_buffer<W: Write>(&mut self, writer: &mut W) -> io::Result<()> {
        debug_assert!(self.output_pos <= MAX_DATA_COUNT);
        // upstream: token.c lines 758-760
        write_deflated_data_header(writer, self.output_pos)?;
        writer.write_all(&self.output_buf[..self.output_pos])?;
        self.output_pos = 0;
        Ok(())
    }

    fn write_token_run<W: Write>(&mut self, writer: &mut W) -> io::Result<()> {
        let r = self.run_start - self.last_run_end;
        let n = self.last_token - self.run_start;

        if (0..=63).contains(&r) {
            let flag = if n == 0 { TOKEN_REL } else { TOKENRUN_REL };
            writer.write_all(&[flag + r as u8])?;
        } else {
            let flag = if n == 0 { TOKEN_LONG } else { TOKENRUN_LONG };
            writer.write_all(&[flag])?;
            writer.write_all(&(self.run_start).to_le_bytes())?;
        }

        if n != 0 {
            writer.write_all(&[(n & 0xFF) as u8])?;
            writer.write_all(&[((n >> 8) & 0xFF) as u8])?;
        }

        self.last_run_end = self.last_token;
        Ok(())
    }
}

/// Zstd decoder state for receiving compressed tokens.
///
/// Maintains a persistent `ZSTD_DCtx` across tokens for a file transfer.
/// Each DEFLATED_DATA block is fed to `ZSTD_decompressStream`. No sync
/// marker restoration (unlike zlib).
///
/// The decoder processes one DEFLATED_DATA block at a time, matching
/// upstream's state machine (r_idle -> r_inflating -> r_idle). When all
/// compressed input is consumed and the output buffer is not full, the
/// decoder returns to idle to read the next wire flag.
///
/// Reference: upstream token.c:recv_zstd_token()
pub(super) struct ZstdTokenDecoder {
    /// Persistent zstd decompression context.
    decoder: ZstdRawDecoder<'static>,
    /// Buffer for decompressed output.
    decompress_buf: Vec<u8>,
    /// Current position in decompress buffer.
    decompress_pos: usize,
    /// Scratch buffer for decompression output.
    /// upstream: out_buffer_size = ZSTD_DStreamOutSize() * 2
    output_buf: Vec<u8>,
    /// Reusable buffer for compressed input data read from the wire.
    compressed_input_buf: Vec<u8>,
    /// Current token index.
    rx_token: i32,
    /// Remaining tokens in current run.
    rx_run: i32,
    pub(super) initialized: bool,
}

impl ZstdTokenDecoder {
    pub(super) fn new() -> io::Result<Self> {
        let decoder = ZstdRawDecoder::new()?;
        // upstream: token.c line 795 - out_buffer_size = ZSTD_DStreamOutSize() * 2
        let out_size = zstd::zstd_safe::DCtx::out_size() * 2;
        Ok(Self {
            decoder,
            decompress_buf: Vec::new(),
            decompress_pos: 0,
            output_buf: vec![0u8; out_size],
            compressed_input_buf: Vec::with_capacity(MAX_DATA_COUNT),
            rx_token: 0,
            rx_run: 0,
            initialized: false,
        })
    }

    pub(super) fn reset(&mut self) -> io::Result<()> {
        self.decoder.reinit()?;
        self.decompress_buf.clear();
        self.decompress_pos = 0;
        self.compressed_input_buf.clear();
        self.rx_token = 0;
        self.rx_run = 0;
        self.initialized = false;
        Ok(())
    }

    /// Receives the next token from a zstd-compressed stream.
    ///
    /// Mirrors upstream's state machine in recv_zstd_token() (token.c lines
    /// 805-877). Processes one DEFLATED_DATA block at a time: reads compressed
    /// data, decompresses via `ZSTD_decompressStream`, and returns available
    /// output. If all input is consumed and the output buffer is not full,
    /// transitions back to idle to read the next wire flag.
    ///
    /// upstream: token.c:recv_zstd_token() lines 805-877
    pub(super) fn recv_token<R: Read>(&mut self, reader: &mut R) -> io::Result<CompressedToken> {
        if !self.initialized {
            self.initialized = true;
        }

        if self.decompress_pos < self.decompress_buf.len() {
            let remaining = &self.decompress_buf[self.decompress_pos..];
            let chunk_len = remaining.len().min(CHUNK_SIZE);
            let data = remaining[..chunk_len].to_vec();
            self.decompress_pos += chunk_len;
            return Ok(CompressedToken::Literal(data));
        }

        // Emit pending run tokens
        // upstream: token.c lines 871-876 (r_running state)
        if self.rx_run > 0 {
            self.rx_run -= 1;
            self.rx_token += 1;
            return Ok(CompressedToken::BlockMatch(self.rx_token as u32));
        }

        // Read next flag byte
        // upstream: token.c lines 812-813 (r_idle state)
        let mut flag_buf = [0u8; 1];
        reader.read_exact(&mut flag_buf)?;
        let flag = flag_buf[0];

        if (flag & 0xC0) == DEFLATED_DATA {
            // upstream: token.c lines 814-822
            let len = read_deflated_data_length(reader, flag)?;
            self.compressed_input_buf.clear();
            self.compressed_input_buf.resize(len, 0);
            reader.read_exact(&mut self.compressed_input_buf)?;

            // Decompress the block
            // upstream: token.c lines 846-863 (r_inflating state)
            self.decompress_buf.clear();
            let mut input_pos = 0;

            while input_pos < self.compressed_input_buf.len() {
                let mut in_buf =
                    zstd::stream::raw::InBuffer::around(&self.compressed_input_buf[input_pos..]);
                let mut out_buf = zstd::stream::raw::OutBuffer::around(&mut self.output_buf);

                self.decoder.run(&mut in_buf, &mut out_buf)?;
                input_pos += in_buf.pos();
                let produced = out_buf.pos();

                if produced > 0 {
                    self.decompress_buf
                        .extend_from_slice(&self.output_buf[..produced]);
                }

                // upstream: token.c lines 862-863
                // If input is fully consumed and output buffer not full,
                // transition back to idle (read next flag).
                if input_pos >= self.compressed_input_buf.len() && produced < self.output_buf.len()
                {
                    break;
                }
            }

            self.decompress_pos = 0;

            if !self.decompress_buf.is_empty() {
                let chunk_len = self.decompress_buf.len().min(CHUNK_SIZE);
                let data = self.decompress_buf[..chunk_len].to_vec();
                self.decompress_pos = chunk_len;
                return Ok(CompressedToken::Literal(data));
            }

            // No output produced - read next flag
            return self.recv_token(reader);
        }

        if flag == END_FLAG {
            // upstream: token.c lines 825-828
            return Ok(CompressedToken::End);
        }

        // Token parsing - same encoding for all algorithms
        // upstream: token.c lines 831-841
        if flag & TOKEN_REL != 0 {
            let rel = (flag & 0x3F) as i32;
            self.rx_token += rel;

            if (flag >> 6) & 1 != 0 {
                let mut run_buf = [0u8; 2];
                reader.read_exact(&mut run_buf)?;
                self.rx_run = u16::from_le_bytes(run_buf) as i32;
            }

            Ok(CompressedToken::BlockMatch(self.rx_token as u32))
        } else if flag & 0xE0 == TOKEN_LONG {
            let mut buf = [0u8; 4];
            reader.read_exact(&mut buf)?;
            self.rx_token = i32::from_le_bytes(buf);

            if flag & 1 != 0 {
                let mut run_buf = [0u8; 2];
                reader.read_exact(&mut run_buf)?;
                self.rx_run = u16::from_le_bytes(run_buf) as i32;
            }

            Ok(CompressedToken::BlockMatch(self.rx_token as u32))
        } else {
            Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("invalid compressed token flag: 0x{flag:02X}"),
            ))
        }
    }

    /// Noop for zstd - no dictionary synchronization needed.
    pub(super) fn see_token(&mut self, _data: &[u8]) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn zstd_roundtrip_literal_only() {
        let mut encoder = ZstdTokenEncoder::new(3).unwrap();
        let mut encoded = Vec::new();

        let data = b"Hello, zstd compressed token world!";
        encoder.send_literal(&mut encoded, data).unwrap();
        encoder.finish(&mut encoded).unwrap();

        let mut decoder = ZstdTokenDecoder::new().unwrap();
        let mut cursor = Cursor::new(&encoded);
        let mut result = Vec::new();

        loop {
            match decoder.recv_token(&mut cursor).unwrap() {
                CompressedToken::Literal(d) => result.extend_from_slice(&d),
                CompressedToken::End => break,
                CompressedToken::BlockMatch(_) => panic!("unexpected block match"),
            }
        }

        assert_eq!(result, data);
    }

    #[test]
    fn zstd_roundtrip_block_matches() {
        let mut encoder = ZstdTokenEncoder::new(3).unwrap();
        let mut encoded = Vec::new();

        encoder.send_literal(&mut encoded, b"prefix").unwrap();
        encoder.send_block_match(&mut encoded, 0).unwrap();
        encoder.send_literal(&mut encoded, b"middle").unwrap();
        encoder.send_block_match(&mut encoded, 5).unwrap();
        encoder.finish(&mut encoded).unwrap();

        let mut decoder = ZstdTokenDecoder::new().unwrap();
        let mut cursor = Cursor::new(&encoded);
        let mut literals = Vec::new();
        let mut blocks = Vec::new();

        loop {
            match decoder.recv_token(&mut cursor).unwrap() {
                CompressedToken::Literal(d) => literals.extend_from_slice(&d),
                CompressedToken::BlockMatch(idx) => blocks.push(idx),
                CompressedToken::End => break,
            }
        }

        assert_eq!(literals, b"prefixmiddle");
        assert_eq!(blocks, vec![0, 5]);
    }

    #[test]
    fn zstd_roundtrip_large_literal() {
        let mut encoder = ZstdTokenEncoder::new(3).unwrap();
        let mut encoded = Vec::new();

        // Large literal exceeding CHUNK_SIZE
        let data: Vec<u8> = (0..100_000).map(|i| (i % 256) as u8).collect();
        encoder.send_literal(&mut encoded, &data).unwrap();
        encoder.finish(&mut encoded).unwrap();

        let mut decoder = ZstdTokenDecoder::new().unwrap();
        let mut cursor = Cursor::new(&encoded);
        let mut result = Vec::new();

        loop {
            match decoder.recv_token(&mut cursor).unwrap() {
                CompressedToken::Literal(d) => result.extend_from_slice(&d),
                CompressedToken::End => break,
                CompressedToken::BlockMatch(_) => panic!("unexpected block match"),
            }
        }

        assert_eq!(result, data);
    }

    #[test]
    fn zstd_see_token_is_noop() {
        let mut encoder = ZstdTokenEncoder::new(3).unwrap();
        encoder.see_token(b"anything").unwrap();

        let mut decoder = ZstdTokenDecoder::new().unwrap();
        decoder.see_token(b"anything").unwrap();
    }

    #[test]
    fn zstd_consecutive_block_matches_use_run_encoding() {
        let mut encoder = ZstdTokenEncoder::new(3).unwrap();
        let mut encoded = Vec::new();

        // Consecutive blocks should use run encoding
        encoder.send_block_match(&mut encoded, 0).unwrap();
        encoder.send_block_match(&mut encoded, 1).unwrap();
        encoder.send_block_match(&mut encoded, 2).unwrap();
        encoder.finish(&mut encoded).unwrap();

        let mut decoder = ZstdTokenDecoder::new().unwrap();
        let mut cursor = Cursor::new(&encoded);
        let mut blocks = Vec::new();

        loop {
            match decoder.recv_token(&mut cursor).unwrap() {
                CompressedToken::Literal(_) => {}
                CompressedToken::BlockMatch(idx) => blocks.push(idx),
                CompressedToken::End => break,
            }
        }

        assert_eq!(blocks, vec![0, 1, 2]);
    }

    /// Verifies flush boundary placement matches upstream framing.
    ///
    /// Upstream writes one DEFLATED_DATA block per output buffer fill
    /// (during continue) or per flush call. For small literals that fit in
    /// a single buffer, the entire compressed+flushed output should appear
    /// as a single DEFLATED_DATA block, not multiple smaller blocks.
    ///
    /// upstream: token.c lines 755-763
    #[test]
    fn zstd_flush_produces_single_deflated_data_block_for_small_input() {
        let mut encoder = ZstdTokenEncoder::new(3).unwrap();
        let mut encoded = Vec::new();

        let data = b"small literal data for flush test";
        encoder.send_literal(&mut encoded, data).unwrap();
        encoder.send_block_match(&mut encoded, 0).unwrap();
        encoder.finish(&mut encoded).unwrap();

        // Count DEFLATED_DATA blocks before the first token byte
        let mut cursor = Cursor::new(&encoded);
        let mut deflated_count = 0;
        let mut total_compressed_len = 0;

        loop {
            let mut flag_buf = [0u8; 1];
            cursor.read_exact(&mut flag_buf).unwrap();
            let flag = flag_buf[0];

            if (flag & 0xC0) == DEFLATED_DATA {
                deflated_count += 1;
                let len = read_deflated_data_length(&mut cursor, flag).unwrap();
                total_compressed_len += len;
                // Skip past compressed data
                let pos = cursor.position() as usize;
                cursor.set_position((pos + len) as u64);
            } else {
                // Hit a token or end flag - stop counting
                break;
            }
        }

        // Small input should produce exactly one DEFLATED_DATA block
        // (all compressed data fits in one MAX_DATA_COUNT buffer)
        assert_eq!(
            deflated_count, 1,
            "small literal should produce exactly one DEFLATED_DATA block, got {deflated_count}"
        );
        assert!(
            total_compressed_len > 0,
            "compressed data should not be empty"
        );
        assert!(
            total_compressed_len <= MAX_DATA_COUNT,
            "single block should not exceed MAX_DATA_COUNT"
        );
    }

    /// Verifies that the wire format uses DEFLATED_DATA framing correctly.
    ///
    /// The encoder must produce: [DEFLATED_DATA blocks...] [TOKEN byte] pattern
    /// for each literal+token pair, matching upstream's output ordering.
    #[test]
    fn zstd_wire_format_ordering() {
        let mut encoder = ZstdTokenEncoder::new(3).unwrap();
        let mut encoded = Vec::new();

        // Literal followed by block match, then another literal + finish
        encoder.send_literal(&mut encoded, b"first chunk").unwrap();
        encoder.send_block_match(&mut encoded, 0).unwrap();
        encoder.send_literal(&mut encoded, b"second chunk").unwrap();
        encoder.finish(&mut encoded).unwrap();

        // Parse wire format to verify ordering
        let mut cursor = Cursor::new(&encoded);
        let mut sequence = Vec::new();

        loop {
            let mut flag_buf = [0u8; 1];
            if cursor.read_exact(&mut flag_buf).is_err() {
                break;
            }
            let flag = flag_buf[0];

            if (flag & 0xC0) == DEFLATED_DATA {
                let len = read_deflated_data_length(&mut cursor, flag).unwrap();
                let pos = cursor.position() as usize;
                cursor.set_position((pos + len) as u64);
                sequence.push("DEFLATED_DATA");
            } else if flag == END_FLAG {
                sequence.push("END");
                break;
            } else if flag & TOKEN_REL != 0 {
                if (flag >> 6) & 1 != 0 {
                    let mut run_buf = [0u8; 2];
                    cursor.read_exact(&mut run_buf).unwrap();
                }
                sequence.push("TOKEN");
            } else if flag & 0xE0 == TOKEN_LONG {
                let mut buf = [0u8; 4];
                cursor.read_exact(&mut buf).unwrap();
                if flag & 1 != 0 {
                    let mut run_buf = [0u8; 2];
                    cursor.read_exact(&mut run_buf).unwrap();
                }
                sequence.push("TOKEN");
            }
        }

        // Expected: DEFLATED_DATA(s) for "first chunk", TOKEN(block 0),
        //           DEFLATED_DATA(s) for "second chunk", END
        assert!(
            sequence.len() >= 4,
            "expected at least 4 wire elements, got {sequence:?}"
        );
        assert_eq!(sequence[0], "DEFLATED_DATA");
        assert_eq!(
            sequence.iter().filter(|s| **s == "TOKEN").count(),
            1,
            "expected exactly one TOKEN"
        );
        assert_eq!(*sequence.last().unwrap(), "END");
    }

    /// Verifies that large literals produce multiple DEFLATED_DATA blocks
    /// each capped at MAX_DATA_COUNT, matching upstream's buffer-full write
    /// pattern.
    ///
    /// upstream: token.c line 755 - write when zstd_out_buff.pos == zstd_out_buff.size
    #[test]
    fn zstd_large_literal_splits_into_max_data_count_blocks() {
        let mut encoder = ZstdTokenEncoder::new(1).unwrap();
        let mut encoded = Vec::new();

        // Use a large dataset so that even with zstd level 1 compression,
        // the compressed output exceeds MAX_DATA_COUNT (16383 bytes) and
        // triggers multiple DEFLATED_DATA blocks on the wire.
        let mut data = Vec::with_capacity(500_000);
        let mut state: u64 = 0xDEAD_BEEF_CAFE_BABE;
        for _ in 0..500_000 {
            // xorshift64 - produces uniformly distributed bytes that
            // defeat zstd's dictionary and entropy coder.
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            data.push((state & 0xFF) as u8);
        }
        encoder.send_literal(&mut encoded, &data).unwrap();
        encoder.finish(&mut encoded).unwrap();

        // Parse and verify all DEFLATED_DATA blocks respect MAX_DATA_COUNT
        let mut cursor = Cursor::new(&encoded);
        let mut block_sizes = Vec::new();

        loop {
            let mut flag_buf = [0u8; 1];
            if cursor.read_exact(&mut flag_buf).is_err() {
                break;
            }
            let flag = flag_buf[0];

            if (flag & 0xC0) == DEFLATED_DATA {
                let len = read_deflated_data_length(&mut cursor, flag).unwrap();
                block_sizes.push(len);
                let pos = cursor.position() as usize;
                cursor.set_position((pos + len) as u64);
            } else if flag == END_FLAG {
                break;
            }
        }

        assert!(
            !block_sizes.is_empty(),
            "should produce at least one DEFLATED_DATA block"
        );
        for (i, &size) in block_sizes.iter().enumerate() {
            assert!(
                size <= MAX_DATA_COUNT,
                "block {i} size {size} exceeds MAX_DATA_COUNT ({MAX_DATA_COUNT})"
            );
            assert!(size > 0, "block {i} should not be empty");
        }

        // With incompressible data, multiple blocks are produced.
        // Blocks from the continue phase are exactly MAX_DATA_COUNT (buffer-full
        // writes). The final block(s) from the flush phase may be smaller.
        assert!(
            block_sizes.len() > 1,
            "500KB of xorshift64 data should produce multiple DEFLATED_DATA blocks, got {} block(s) totaling {} bytes",
            block_sizes.len(),
            block_sizes.iter().sum::<usize>(),
        );

        // Verify roundtrip
        let mut decoder = ZstdTokenDecoder::new().unwrap();
        let mut read_cursor = Cursor::new(&encoded);
        let mut result = Vec::new();

        loop {
            match decoder.recv_token(&mut read_cursor).unwrap() {
                CompressedToken::Literal(d) => result.extend_from_slice(&d),
                CompressedToken::End => break,
                CompressedToken::BlockMatch(_) => panic!("unexpected block match"),
            }
        }

        assert_eq!(result, data);
    }

    /// Verifies multiple file reset and re-encode works correctly.
    #[test]
    fn zstd_reset_between_files() {
        let mut encoder = ZstdTokenEncoder::new(3).unwrap();

        for i in 0..3 {
            let mut encoded = Vec::new();
            let data = format!("file {i} content with some data to compress");
            encoder.send_literal(&mut encoded, data.as_bytes()).unwrap();
            encoder.send_block_match(&mut encoded, i as u32).unwrap();
            encoder.finish(&mut encoded).unwrap();

            let mut decoder = ZstdTokenDecoder::new().unwrap();
            let mut cursor = Cursor::new(&encoded);
            let mut literals = Vec::new();
            let mut blocks = Vec::new();

            loop {
                match decoder.recv_token(&mut cursor).unwrap() {
                    CompressedToken::Literal(d) => literals.extend_from_slice(&d),
                    CompressedToken::BlockMatch(idx) => blocks.push(idx),
                    CompressedToken::End => break,
                }
            }

            assert_eq!(literals, data.as_bytes());
            assert_eq!(blocks, vec![i as u32]);
        }
    }

    /// Verifies that a block match with no preceding literals produces
    /// no DEFLATED_DATA blocks before the token.
    #[test]
    fn zstd_block_match_without_literals_no_deflated_data() {
        let mut encoder = ZstdTokenEncoder::new(3).unwrap();
        let mut encoded = Vec::new();

        encoder.send_block_match(&mut encoded, 42).unwrap();
        encoder.finish(&mut encoded).unwrap();

        // First byte should be a TOKEN, not DEFLATED_DATA
        assert_ne!(
            encoded[0] & 0xC0,
            DEFLATED_DATA,
            "block match without literals should not produce DEFLATED_DATA"
        );

        // Verify roundtrip
        let mut decoder = ZstdTokenDecoder::new().unwrap();
        let mut cursor = Cursor::new(&encoded);
        let mut blocks = Vec::new();

        loop {
            match decoder.recv_token(&mut cursor).unwrap() {
                CompressedToken::Literal(_) => {}
                CompressedToken::BlockMatch(idx) => blocks.push(idx),
                CompressedToken::End => break,
            }
        }

        assert_eq!(blocks, vec![42]);
    }

    /// Golden byte test for the DEFLATED_DATA header format.
    ///
    /// Verifies the 2-byte header encoding: first byte is
    /// `DEFLATED_DATA | (len >> 8)`, second byte is `len & 0xFF`.
    /// This must match upstream's obuf[0]/obuf[1] encoding at
    /// token.c lines 758-759.
    #[test]
    fn zstd_deflated_data_header_matches_upstream() {
        let mut encoder = ZstdTokenEncoder::new(3).unwrap();
        let mut encoded = Vec::new();

        encoder.send_literal(&mut encoded, b"test").unwrap();
        encoder.finish(&mut encoded).unwrap();

        // First two bytes should be the DEFLATED_DATA header
        let flag = encoded[0];
        assert_eq!(
            flag & 0xC0,
            DEFLATED_DATA,
            "first byte should have DEFLATED_DATA flag"
        );

        // Decode the length from the header
        let high = (flag & 0x3F) as usize;
        let low = encoded[1] as usize;
        let len = (high << 8) | low;

        // The compressed data should follow immediately
        assert!(
            encoded.len() >= 2 + len,
            "encoded data too short for declared length"
        );

        // After the DEFLATED_DATA block, the next byte should be END_FLAG
        assert_eq!(
            encoded[2 + len],
            END_FLAG,
            "END_FLAG should follow the single DEFLATED_DATA block"
        );
    }

    /// Verifies that interleaved literal + block match sequences produce
    /// correct flush boundaries with one flush per token boundary.
    #[test]
    fn zstd_interleaved_literal_block_flush_boundaries() {
        let mut encoder = ZstdTokenEncoder::new(3).unwrap();
        let mut encoded = Vec::new();

        // Pattern: lit, match, lit, match, lit, match, end
        for i in 0..3 {
            let data = format!("segment {i} with enough data to be meaningful");
            encoder.send_literal(&mut encoded, data.as_bytes()).unwrap();
            encoder.send_block_match(&mut encoded, i).unwrap();
        }
        encoder.finish(&mut encoded).unwrap();

        // Decode and verify
        let mut decoder = ZstdTokenDecoder::new().unwrap();
        let mut cursor = Cursor::new(&encoded);
        let mut literals = Vec::new();
        let mut blocks = Vec::new();

        loop {
            match decoder.recv_token(&mut cursor).unwrap() {
                CompressedToken::Literal(d) => literals.extend_from_slice(&d),
                CompressedToken::BlockMatch(idx) => blocks.push(idx),
                CompressedToken::End => break,
            }
        }

        let expected_literals: Vec<u8> = (0..3)
            .flat_map(|i| format!("segment {i} with enough data to be meaningful").into_bytes())
            .collect();
        assert_eq!(literals, expected_literals);
        assert_eq!(blocks, vec![0, 1, 2]);
    }

    /// Verifies that empty literal data (only block matches) roundtrips.
    #[test]
    fn zstd_only_block_matches_roundtrip() {
        let mut encoder = ZstdTokenEncoder::new(3).unwrap();
        let mut encoded = Vec::new();

        encoder.send_block_match(&mut encoded, 10).unwrap();
        encoder.send_block_match(&mut encoded, 20).unwrap();
        encoder.send_block_match(&mut encoded, 30).unwrap();
        encoder.finish(&mut encoded).unwrap();

        let mut decoder = ZstdTokenDecoder::new().unwrap();
        let mut cursor = Cursor::new(&encoded);
        let mut blocks = Vec::new();

        loop {
            match decoder.recv_token(&mut cursor).unwrap() {
                CompressedToken::Literal(_) => {}
                CompressedToken::BlockMatch(idx) => blocks.push(idx),
                CompressedToken::End => break,
            }
        }

        assert_eq!(blocks, vec![10, 20, 30]);
    }
}
