//! LZ4 per-token codec for compressed token wire format.
//!
//! Implements the LZ4-specific encoder and decoder used by CPRES_LZ4 mode.
//! Unlike zlib/zstd, LZ4 compresses each chunk independently with no
//! persistent compression state. `see_token` is always a noop.
//!
//! ## Flush boundary alignment
//!
//! Upstream rsync compresses literal data at token boundaries, not eagerly.
//! When `send_compressed_token()` is called with literal bytes (`nb != 0`),
//! the previous token run is written first, then the literals are compressed
//! and emitted as DEFLATED_DATA blocks. This ensures the wire ordering is
//! `[prev_token_run] [DEFLATED_DATA...] [next_token_run] ...`, matching the
//! receiver's state machine expectation.
//!
//! Literal data is buffered in `send_literal()` and compressed eagerly when
//! the buffer reaches `MAX_DATA_COUNT` bytes. Any remaining tail is flushed
//! at the next token boundary (`send_block_match()` or `finish()`). This
//! matches upstream's per-call compression pattern where each invocation of
//! `send_compressed_token(f, token, buf, offset, nb)` compresses its literal
//! payload in `MIN(nb, MAX_DATA_COUNT)`-sized input chunks immediately.
//!
//! - upstream: token.c:send_compressed_token() (SUPPORT_LZ4 variant) lines 881-954
//! - upstream: token.c:recv_compressed_token() (SUPPORT_LZ4 variant) lines 956-1027

use std::io::{self, Read, Write};

use lz4_flex::block;

use super::{
    CHUNK_SIZE, CompressedToken, DEFLATED_DATA, END_FLAG, MAX_DATA_COUNT, TOKEN_LONG, TOKEN_REL,
    TOKENRUN_LONG, TOKENRUN_REL, read_deflated_data_length, write_deflated_data_header,
};

/// LZ4 encoder state for sending compressed tokens.
///
/// Each literal chunk is compressed independently via `LZ4_compress_default`.
/// No persistent compression context is maintained across chunks.
///
/// Literal data is compressed eagerly in `MAX_DATA_COUNT`-sized chunks as it
/// accumulates. Any remaining tail is flushed at token boundaries (block match
/// or finish). This matches upstream's wire framing where each call to
/// `send_compressed_token` compresses its literals immediately.
///
/// Reference: upstream token.c:send_compressed_token() (SUPPORT_LZ4)
pub(super) struct Lz4TokenEncoder {
    /// Output buffer for compressed data. Sized to hold compressed output.
    /// upstream: size = MAX(LZ4_compressBound(CHUNK_SIZE), MAX_DATA_COUNT+2)
    output_buf: Vec<u8>,
    /// Accumulated literal data pending compression at next token boundary.
    literal_buf: Vec<u8>,
    /// Last token sent (for run encoding).
    last_token: i32,
    /// Start of current token run.
    run_start: i32,
    /// End of last token run.
    last_run_end: i32,
}

impl Lz4TokenEncoder {
    pub(super) fn new() -> Self {
        let size = lz4_flex::block::get_maximum_output_size(CHUNK_SIZE).max(MAX_DATA_COUNT + 2);
        Self {
            output_buf: vec![0u8; size],
            literal_buf: Vec::new(),
            last_token: -1,
            run_start: 0,
            last_run_end: 0,
        }
    }

    pub(super) fn reset(&mut self) {
        self.literal_buf.clear();
        self.last_token = -1;
        self.run_start = 0;
        self.last_run_end = 0;
    }

    /// Buffers literal data and eagerly compresses full chunks.
    ///
    /// Upstream rsync compresses literal data within each call to
    /// `send_compressed_token()` in `MAX_DATA_COUNT`-sized input chunks
    /// (token.c lines 923-947). To match this wire framing, we compress
    /// and emit each `MAX_DATA_COUNT` input chunk as soon as it accumulates
    /// rather than deferring all compression to token boundaries.
    ///
    /// Without eager flushing, whole-file transfers would buffer all literal
    /// data in memory and only emit compressed output at `finish()`, causing
    /// the receiver to stall waiting for DEFLATED_DATA blocks that upstream
    /// would have sent incrementally.
    ///
    /// upstream: token.c line 927 - `available_in = MIN(nb, MAX_DATA_COUNT)`
    pub(super) fn send_literal<W: Write>(&mut self, writer: &mut W, data: &[u8]) -> io::Result<()> {
        self.literal_buf.extend_from_slice(data);
        // upstream: token.c lines 923-947 - compress in MAX_DATA_COUNT chunks
        while self.literal_buf.len() >= MAX_DATA_COUNT {
            self.compress_one_chunk(writer)?;
        }
        Ok(())
    }

    /// Emits a block match token, flushing pending literals first.
    ///
    /// Writes the previous token run, then compresses and emits any buffered
    /// literal data, matching upstream's ordering: `[prev_token_run]
    /// [DEFLATED_DATA...]` within each `send_compressed_token()` call.
    ///
    /// upstream: token.c lines 889-948
    pub(super) fn send_block_match<W: Write>(
        &mut self,
        writer: &mut W,
        block_index: u32,
    ) -> io::Result<()> {
        let token = block_index as i32;
        let has_literals = !self.literal_buf.is_empty();

        // upstream: token.c lines 889-915 - same run encoding as zlib/zstd
        if self.last_token == -1 || self.last_token == -2 {
            // upstream: token.c lines 919-948 - compress and emit literals
            self.compress_and_emit(writer)?;
            self.run_start = token;
        } else if has_literals || token != self.last_token + 1 || token >= self.run_start + 65536 {
            self.write_token_run(writer)?;
            // upstream: token.c lines 919-948 - compress and emit literals
            self.compress_and_emit(writer)?;
            self.run_start = token;
        }

        self.last_token = token;
        Ok(())
    }

    /// Finishes the current file's compressed token stream.
    ///
    /// Writes the final token run (if any), flushes pending literals, and
    /// emits END_FLAG.
    ///
    /// upstream: token.c lines 950-953
    pub(super) fn finish<W: Write>(&mut self, writer: &mut W) -> io::Result<()> {
        if self.last_token >= 0 {
            self.write_token_run(writer)?;
        }
        self.compress_and_emit(writer)?;
        writer.write_all(&[END_FLAG])?;
        self.reset();
        Ok(())
    }

    /// Noop for LZ4 - no dictionary synchronization needed.
    pub(super) fn see_token(&mut self, _data: &[u8]) -> io::Result<()> {
        Ok(())
    }

    /// Compresses and emits all remaining buffered literal data.
    ///
    /// Called at token boundaries (`send_block_match` / `finish`) to flush
    /// any leftover data that did not reach a full `MAX_DATA_COUNT` chunk
    /// during `send_literal`. Most data will already have been emitted
    /// eagerly; this handles the tail.
    ///
    /// upstream: token.c lines 919-948 - LZ4_compress_default with halving retry
    fn compress_and_emit<W: Write>(&mut self, writer: &mut W) -> io::Result<()> {
        while !self.literal_buf.is_empty() {
            self.compress_one_chunk(writer)?;
        }
        Ok(())
    }

    /// Compresses one `MAX_DATA_COUNT`-capped chunk from the front of `literal_buf`.
    ///
    /// Each chunk is compressed independently via `LZ4_compress_default`.
    /// If the compressed output exceeds `MAX_DATA_COUNT`, the input is halved
    /// and retried, matching upstream's retry loop.
    ///
    /// upstream: token.c lines 923-946 - compress, retry with halved input
    fn compress_one_chunk<W: Write>(&mut self, writer: &mut W) -> io::Result<()> {
        let total = self.literal_buf.len();
        if total == 0 {
            return Ok(());
        }

        let mut available_in = total.min(MAX_DATA_COUNT);

        loop {
            let input = &self.literal_buf[..available_in];
            match block::compress_into(input, &mut self.output_buf) {
                Ok(compressed_len) if compressed_len <= MAX_DATA_COUNT => {
                    // upstream: token.c lines 938-941
                    write_deflated_data_header(writer, compressed_len)?;
                    writer.write_all(&self.output_buf[..compressed_len])?;
                    self.literal_buf.drain(..available_in);
                    return Ok(());
                }
                Ok(_) => {
                    // Compressed output too large, halve input and retry
                    // upstream: token.c line 930
                    available_in /= 2;
                    if available_in == 0 {
                        return Err(io::Error::other(
                            "LZ4 compression failed: output exceeds limit",
                        ));
                    }
                }
                Err(e) => {
                    return Err(io::Error::other(e.to_string()));
                }
            }
        }
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

/// LZ4 decoder state for receiving compressed tokens.
///
/// Each DEFLATED_DATA block is decompressed independently via
/// `LZ4_decompress_safe`. No persistent decompression context.
///
/// Reference: upstream token.c:recv_compressed_token() (SUPPORT_LZ4)
pub(super) struct Lz4TokenDecoder {
    /// Buffer for decompressed output.
    decompress_buf: Vec<u8>,
    /// Current position in decompress buffer.
    decompress_pos: usize,
    /// Reusable buffer for compressed input data read from the wire.
    compressed_input_buf: Vec<u8>,
    /// Current token index.
    rx_token: i32,
    /// Remaining tokens in current run.
    rx_run: i32,
    pub(super) initialized: bool,
}

impl Lz4TokenDecoder {
    pub(super) fn new() -> Self {
        let size = lz4_flex::block::get_maximum_output_size(CHUNK_SIZE).max(MAX_DATA_COUNT + 2);
        Self {
            decompress_buf: vec![0u8; size],
            decompress_pos: 0,
            compressed_input_buf: Vec::with_capacity(MAX_DATA_COUNT),
            rx_token: 0,
            rx_run: 0,
            initialized: false,
        }
    }

    pub(super) fn reset(&mut self) {
        self.decompress_pos = 0;
        self.compressed_input_buf.clear();
        self.rx_token = 0;
        self.rx_run = 0;
        self.initialized = false;
    }

    /// Receives the next token from an LZ4-compressed stream.
    ///
    /// Each DEFLATED_DATA block is decompressed independently via
    /// `LZ4_decompress_safe`. No persistent state between blocks.
    ///
    /// upstream: token.c:recv_compressed_token() lines 965-1026 (SUPPORT_LZ4)
    pub(super) fn recv_token<R: Read>(&mut self, reader: &mut R) -> io::Result<CompressedToken> {
        if !self.initialized {
            self.initialized = true;
        }

        // Emit pending run tokens
        if self.rx_run > 0 {
            self.rx_run -= 1;
            self.rx_token += 1;
            return Ok(CompressedToken::BlockMatch(self.rx_token as u32));
        }

        // Read next flag
        let mut flag_buf = [0u8; 1];
        reader.read_exact(&mut flag_buf)?;
        let flag = flag_buf[0];

        if (flag & 0xC0) == DEFLATED_DATA {
            // upstream: token.c lines 979-984
            let len = read_deflated_data_length(reader, flag)?;
            self.compressed_input_buf.clear();
            self.compressed_input_buf.resize(len, 0);
            reader.read_exact(&mut self.compressed_input_buf)?;

            // upstream: token.c line 1008 - LZ4_decompress_safe
            let decompressed_len =
                block::decompress_into(&self.compressed_input_buf, &mut self.decompress_buf)
                    .map_err(|e| {
                        io::Error::new(
                            io::ErrorKind::InvalidData,
                            format!("LZ4 decompression failed: {e}"),
                        )
                    })?;

            if decompressed_len > 0 {
                let data = self.decompress_buf[..decompressed_len].to_vec();
                return Ok(CompressedToken::Literal(data));
            }

            // No output produced, try next token
            return self.recv_token(reader);
        }

        if flag == END_FLAG {
            return Ok(CompressedToken::End);
        }

        // Token parsing - identical to zlib/zstd
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

    /// Noop for LZ4 - no dictionary synchronization needed.
    pub(super) fn see_token(&mut self, _data: &[u8]) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn lz4_roundtrip_literal_only() {
        let mut encoder = Lz4TokenEncoder::new();
        let mut encoded = Vec::new();

        let data = b"Hello, LZ4 compressed token world!";
        encoder.send_literal(&mut encoded, data).unwrap();
        encoder.finish(&mut encoded).unwrap();

        let mut decoder = Lz4TokenDecoder::new();
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
    fn lz4_roundtrip_block_matches() {
        let mut encoder = Lz4TokenEncoder::new();
        let mut encoded = Vec::new();

        encoder.send_literal(&mut encoded, b"prefix").unwrap();
        encoder.send_block_match(&mut encoded, 0).unwrap();
        encoder.send_literal(&mut encoded, b"middle").unwrap();
        encoder.send_block_match(&mut encoded, 5).unwrap();
        encoder.finish(&mut encoded).unwrap();

        let mut decoder = Lz4TokenDecoder::new();
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
    fn lz4_roundtrip_large_literal() {
        let mut encoder = Lz4TokenEncoder::new();
        let mut encoded = Vec::new();

        let data: Vec<u8> = (0..100_000).map(|i| (i % 256) as u8).collect();
        encoder.send_literal(&mut encoded, &data).unwrap();
        encoder.finish(&mut encoded).unwrap();

        let mut decoder = Lz4TokenDecoder::new();
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
    fn lz4_see_token_is_noop() {
        let mut encoder = Lz4TokenEncoder::new();
        encoder.see_token(b"anything").unwrap();

        let mut decoder = Lz4TokenDecoder::new();
        decoder.see_token(b"anything").unwrap();
    }

    #[test]
    fn lz4_consecutive_block_matches_use_run_encoding() {
        let mut encoder = Lz4TokenEncoder::new();
        let mut encoded = Vec::new();

        encoder.send_block_match(&mut encoded, 0).unwrap();
        encoder.send_block_match(&mut encoded, 1).unwrap();
        encoder.send_block_match(&mut encoded, 2).unwrap();
        encoder.finish(&mut encoded).unwrap();

        let mut decoder = Lz4TokenDecoder::new();
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

    /// Verifies that the wire format uses correct token boundary ordering.
    ///
    /// Upstream writes: [DEFLATED_DATA...] [TOKEN_RUN] for each literal+match
    /// pair. Literals must be emitted as DEFLATED_DATA blocks BEFORE the next
    /// token run, not eagerly before the previous token run completes.
    ///
    /// upstream: token.c lines 889-948 - token run then literal compression
    #[test]
    fn lz4_wire_format_ordering() {
        let mut encoder = Lz4TokenEncoder::new();
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

        // Expected: DEFLATED_DATA("first chunk"), TOKEN(block 0),
        //           DEFLATED_DATA("second chunk"), END
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

    /// Verifies that large literals split into multiple DEFLATED_DATA blocks,
    /// each capped at MAX_DATA_COUNT.
    ///
    /// upstream: token.c lines 927-946 - compress in MAX_DATA_COUNT chunks
    #[test]
    fn lz4_large_literal_splits_into_max_data_count_blocks() {
        let mut encoder = Lz4TokenEncoder::new();
        let mut encoded = Vec::new();

        // Use incompressible data to ensure compressed output exceeds
        // MAX_DATA_COUNT and triggers multiple DEFLATED_DATA blocks.
        let mut data = Vec::with_capacity(500_000);
        let mut state: u64 = 0xDEAD_BEEF_CAFE_BABE;
        for _ in 0..500_000 {
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

        // LZ4 block compression on random data typically expands slightly,
        // so 500KB should produce many blocks.
        assert!(
            block_sizes.len() > 1,
            "500KB of xorshift64 data should produce multiple DEFLATED_DATA blocks, got {} block(s) totaling {} bytes",
            block_sizes.len(),
            block_sizes.iter().sum::<usize>(),
        );

        // Verify roundtrip
        let mut decoder = Lz4TokenDecoder::new();
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

    /// Verifies that interleaved literal + block match sequences produce
    /// correct flush boundaries with literals emitted at each token boundary.
    #[test]
    fn lz4_interleaved_literal_block_flush_boundaries() {
        let mut encoder = Lz4TokenEncoder::new();
        let mut encoded = Vec::new();

        // Pattern: lit, match, lit, match, lit, match, end
        for i in 0..3 {
            let data = format!("segment {i} with enough data to be meaningful");
            encoder.send_literal(&mut encoded, data.as_bytes()).unwrap();
            encoder.send_block_match(&mut encoded, i).unwrap();
        }
        encoder.finish(&mut encoded).unwrap();

        // Decode and verify
        let mut decoder = Lz4TokenDecoder::new();
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

    /// Verifies that a block match with no preceding literals produces
    /// no DEFLATED_DATA blocks before the token.
    #[test]
    fn lz4_block_match_without_literals_no_deflated_data() {
        let mut encoder = Lz4TokenEncoder::new();
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
        let mut decoder = Lz4TokenDecoder::new();
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

    /// Verifies multiple file reset and re-encode works correctly.
    #[test]
    fn lz4_reset_between_files() {
        let mut encoder = Lz4TokenEncoder::new();

        for i in 0..3 {
            let mut encoded = Vec::new();
            let data = format!("file {i} content with some data to compress");
            encoder.send_literal(&mut encoded, data.as_bytes()).unwrap();
            encoder.send_block_match(&mut encoded, i as u32).unwrap();
            encoder.finish(&mut encoded).unwrap();

            let mut decoder = Lz4TokenDecoder::new();
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

    /// Golden byte test for the DEFLATED_DATA header format.
    ///
    /// Verifies the 2-byte header encoding: first byte is
    /// `DEFLATED_DATA | (len >> 8)`, second byte is `len & 0xFF`.
    /// This must match upstream's obuf[0]/obuf[1] encoding at
    /// token.c lines 938-939.
    #[test]
    fn lz4_deflated_data_header_matches_upstream() {
        let mut encoder = Lz4TokenEncoder::new();
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

    /// Verifies that empty literal data (only block matches) roundtrips.
    #[test]
    fn lz4_only_block_matches_roundtrip() {
        let mut encoder = Lz4TokenEncoder::new();
        let mut encoded = Vec::new();

        encoder.send_block_match(&mut encoded, 10).unwrap();
        encoder.send_block_match(&mut encoded, 20).unwrap();
        encoder.send_block_match(&mut encoded, 30).unwrap();
        encoder.finish(&mut encoded).unwrap();

        let mut decoder = Lz4TokenDecoder::new();
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

    /// Verifies flush boundary placement matches upstream framing.
    ///
    /// For small literals that fit in a single DEFLATED_DATA block, the
    /// entire compressed output should appear as a single DEFLATED_DATA
    /// block before the token, not multiple smaller blocks.
    ///
    /// upstream: token.c lines 937-941 - write obuf as single block
    #[test]
    fn lz4_flush_produces_single_deflated_data_block_for_small_input() {
        let mut encoder = Lz4TokenEncoder::new();
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
                let pos = cursor.position() as usize;
                cursor.set_position((pos + len) as u64);
            } else {
                break;
            }
        }

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

    /// Verifies that send_literal eagerly emits DEFLATED_DATA blocks when
    /// the buffer exceeds MAX_DATA_COUNT, matching upstream's per-call
    /// compression behavior.
    ///
    /// upstream: token.c line 927 - `available_in = MIN(nb, MAX_DATA_COUNT)`
    /// Upstream compresses literals within each send_compressed_token() call.
    /// Without eager flushing, the receiver would stall waiting for blocks.
    #[test]
    fn lz4_send_literal_eagerly_emits_deflated_data() {
        let mut encoder = Lz4TokenEncoder::new();
        let mut encoded = Vec::new();

        // Feed data exceeding MAX_DATA_COUNT so eager flush triggers
        let data = vec![b'A'; MAX_DATA_COUNT + 100];
        encoder.send_literal(&mut encoded, &data).unwrap();

        // Before finish(), output should already contain DEFLATED_DATA
        // blocks from the eager flush during send_literal.
        assert!(
            !encoded.is_empty(),
            "send_literal must eagerly emit DEFLATED_DATA for data exceeding MAX_DATA_COUNT"
        );

        // Verify at least one valid DEFLATED_DATA header was written
        assert_eq!(
            encoded[0] & 0xC0,
            DEFLATED_DATA,
            "first byte should have DEFLATED_DATA flag"
        );

        // Complete the stream and verify roundtrip
        encoder.finish(&mut encoded).unwrap();

        let mut decoder = Lz4TokenDecoder::new();
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

    /// Verifies that large whole-file literals produce incremental
    /// DEFLATED_DATA blocks during send_literal, not deferred to finish.
    ///
    /// This simulates a whole-file transfer where send_literal is called
    /// multiple times with large chunks. The receiver must see DEFLATED_DATA
    /// blocks incrementally for interop with upstream rsync.
    #[test]
    fn lz4_incremental_literal_flush_for_whole_file_transfer() {
        let mut encoder = Lz4TokenEncoder::new();
        let mut encoded = Vec::new();

        // Simulate whole-file transfer: multiple large literal writes
        let chunk = vec![b'X'; 65536]; // 64KB per write
        for _ in 0..4 {
            encoder.send_literal(&mut encoded, &chunk).unwrap();
        }

        // After 256KB of literals, output should already contain many
        // DEFLATED_DATA blocks (not waiting for finish).
        let pre_finish_len = encoded.len();
        assert!(
            pre_finish_len > 0,
            "256KB of literals must produce DEFLATED_DATA output during send_literal"
        );

        encoder.finish(&mut encoded).unwrap();

        // Verify roundtrip
        let expected: Vec<u8> = vec![b'X'; 65536 * 4];
        let mut decoder = Lz4TokenDecoder::new();
        let mut cursor = Cursor::new(&encoded);
        let mut result = Vec::new();

        loop {
            match decoder.recv_token(&mut cursor).unwrap() {
                CompressedToken::Literal(d) => result.extend_from_slice(&d),
                CompressedToken::End => break,
                CompressedToken::BlockMatch(_) => panic!("unexpected block match"),
            }
        }

        assert_eq!(result, expected);
    }
}
