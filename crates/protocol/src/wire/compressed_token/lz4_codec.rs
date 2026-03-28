//! LZ4 per-token codec for compressed token wire format.
//!
//! Implements the LZ4-specific encoder and decoder used by CPRES_LZ4 mode.
//! Unlike zlib/zstd, LZ4 compresses each chunk independently with no
//! persistent compression state. `see_token` is always a noop.
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
/// Reference: upstream token.c:send_compressed_token() (SUPPORT_LZ4)
pub(super) struct Lz4TokenEncoder {
    /// Output buffer for compressed data. Sized to hold compressed output
    /// plus the 2-byte DEFLATED_DATA header.
    /// upstream: size = MAX(LZ4_compressBound(CHUNK_SIZE), MAX_DATA_COUNT+2)
    output_buf: Vec<u8>,
    /// Last token sent (for run encoding).
    last_token: i32,
    /// Start of current token run.
    run_start: i32,
    /// End of last token run.
    last_run_end: i32,
    /// Whether a flush is pending (data fed but not yet flushed).
    flush_pending: bool,
}

impl Lz4TokenEncoder {
    pub(super) fn new() -> Self {
        let size = lz4_flex::block::get_maximum_output_size(CHUNK_SIZE).max(MAX_DATA_COUNT + 2);
        Self {
            output_buf: vec![0u8; size],
            last_token: -1,
            run_start: 0,
            last_run_end: 0,
            flush_pending: false,
        }
    }

    pub(super) fn reset(&mut self) {
        self.last_token = -1;
        self.run_start = 0;
        self.last_run_end = 0;
        self.flush_pending = false;
    }

    /// Sends literal data, compressing each chunk independently.
    ///
    /// upstream: token.c lines 919-948 - compress with LZ4_compress_default,
    /// retry with halved input if output exceeds MAX_DATA_COUNT.
    pub(super) fn send_literal<W: Write>(&mut self, writer: &mut W, data: &[u8]) -> io::Result<()> {
        let mut offset = 0;
        while offset < data.len() {
            let mut available_in = (data.len() - offset).min(MAX_DATA_COUNT);

            // upstream: LZ4_compress_default, retry with halved input if output too large
            loop {
                let input = &data[offset..offset + available_in];
                match block::compress_into(input, &mut self.output_buf) {
                    Ok(compressed_len) if compressed_len <= MAX_DATA_COUNT => {
                        write_deflated_data_header(writer, compressed_len)?;
                        writer.write_all(&self.output_buf[..compressed_len])?;
                        offset += available_in;
                        break;
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
            self.flush_pending = true;
        }
        Ok(())
    }

    pub(super) fn send_block_match<W: Write>(
        &mut self,
        writer: &mut W,
        block_index: u32,
    ) -> io::Result<()> {
        let token = block_index as i32;
        let has_literals = self.flush_pending;

        // upstream: token.c lines 889-914 - same run encoding as zlib/zstd
        if self.last_token == -1 || self.last_token == -2 {
            self.run_start = token;
        } else if has_literals || token != self.last_token + 1 || token >= self.run_start + 65536 {
            self.write_token_run(writer)?;
            self.run_start = token;
        }
        self.flush_pending = false;

        self.last_token = token;
        Ok(())
    }

    pub(super) fn finish<W: Write>(&mut self, writer: &mut W) -> io::Result<()> {
        if self.last_token >= 0 {
            self.write_token_run(writer)?;
        }
        writer.write_all(&[END_FLAG])?;
        self.reset();
        Ok(())
    }

    /// Noop for LZ4 - no dictionary synchronization needed.
    pub(super) fn see_token(&mut self, _data: &[u8]) -> io::Result<()> {
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
}
