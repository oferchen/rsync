//! Zstd per-token codec for compressed token wire format.
//!
//! Implements the zstd-specific encoder and decoder used by CPRES_ZSTD mode.
//! Unlike zlib, zstd does not use sync marker stripping/restoration, and
//! `see_token` is always a noop (no dictionary synchronization needed).
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
/// Reference: upstream token.c:send_zstd_token()
pub(super) struct ZstdTokenEncoder {
    /// Persistent zstd compression context.
    encoder: ZstdRawEncoder<'static>,
    /// Output buffer for compression results.
    output_buf: Vec<u8>,
    /// Accumulated literal data pending compression.
    literal_buf: Vec<u8>,
    /// Last token sent (for run encoding).
    last_token: i32,
    /// Start of current token run.
    run_start: i32,
    /// End of last token run.
    last_run_end: i32,
    /// Whether a flush is pending (data fed but not yet flushed).
    flush_pending: bool,
}

impl ZstdTokenEncoder {
    /// Creates a new zstd encoder with the specified compression level.
    pub(super) fn new(level: i32) -> io::Result<Self> {
        let encoder = ZstdRawEncoder::new(level)?;
        Ok(Self {
            encoder,
            output_buf: vec![0u8; MAX_DATA_COUNT],
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
            self.flush_literals(writer)?;
            self.run_start = token;
        } else if has_literals || token != self.last_token + 1 || token >= self.run_start + 65536 {
            self.write_token_run(writer)?;
            self.flush_literals(writer)?;
            self.run_start = token;
        }

        self.last_token = token;
        Ok(())
    }

    pub(super) fn finish<W: Write>(&mut self, writer: &mut W) -> io::Result<()> {
        if self.last_token >= 0 {
            self.write_token_run(writer)?;
        }
        self.flush_literals(writer)?;
        writer.write_all(&[END_FLAG])?;
        self.reset()?;
        Ok(())
    }

    /// Noop for zstd - no dictionary synchronization needed.
    /// upstream: token.c:1102-1104 (see_token for CPRES_ZSTD is empty)
    pub(super) fn see_token(&mut self, _data: &[u8]) -> io::Result<()> {
        Ok(())
    }

    /// Compresses and flushes all pending literal data.
    ///
    /// Feeds accumulated literals to the zstd encoder, then performs
    /// ZSTD_e_flush to produce decompressible output. Writes results
    /// as DEFLATED_DATA blocks on the wire.
    ///
    /// upstream: token.c lines 727-770
    fn flush_literals<W: Write>(&mut self, writer: &mut W) -> io::Result<()> {
        if self.literal_buf.is_empty() && !self.flush_pending {
            return Ok(());
        }

        let input = std::mem::take(&mut self.literal_buf);
        let mut input_pos = 0;

        // Feed input with ZSTD_e_continue, then flush
        while input_pos < input.len() {
            let mut in_buf = zstd::stream::raw::InBuffer::around(&input[input_pos..]);
            let mut out_buf = zstd::stream::raw::OutBuffer::around(&mut self.output_buf);

            self.encoder.run(&mut in_buf, &mut out_buf)?;
            input_pos += in_buf.pos();

            let produced = out_buf.pos();
            if produced > 0 {
                let data = self.output_buf[..produced].to_vec();
                self.write_deflated_output(writer, &data)?;
            }
        }

        // ZSTD_e_flush to produce decompressible boundary
        // upstream: token.c line 741-743
        loop {
            let mut out_buf = zstd::stream::raw::OutBuffer::around(&mut self.output_buf);

            let remaining = self.encoder.flush(&mut out_buf)?;
            let produced = out_buf.pos();

            if produced > 0 {
                let data = self.output_buf[..produced].to_vec();
                self.write_deflated_output(writer, &data)?;
            }

            if remaining == 0 {
                break;
            }
        }

        self.flush_pending = false;
        Ok(())
    }

    /// Writes compressed data as DEFLATED_DATA blocks.
    fn write_deflated_output<W: Write>(&self, writer: &mut W, data: &[u8]) -> io::Result<()> {
        let mut offset = 0;
        while offset < data.len() {
            let piece_len = (data.len() - offset).min(MAX_DATA_COUNT);
            write_deflated_data_header(writer, piece_len)?;
            writer.write_all(&data[offset..offset + piece_len])?;
            offset += piece_len;
        }
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
/// Each DEFLATED_DATA block is fed to ZSTD_decompressStream. No sync
/// marker restoration (unlike zlib).
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
        // upstream: out_buffer_size = ZSTD_DStreamOutSize() * 2
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
    /// Unlike zlib, there is no sync marker stripping/restoration. Each
    /// DEFLATED_DATA block is decompressed via ZSTD_decompressStream.
    /// The persistent DCtx maintains state across blocks.
    ///
    /// upstream: token.c:recv_zstd_token() lines 805-870
    pub(super) fn recv_token<R: Read>(&mut self, reader: &mut R) -> io::Result<CompressedToken> {
        if !self.initialized {
            self.initialized = true;
        }

        // Return buffered decompressed data
        if self.decompress_pos < self.decompress_buf.len() {
            let remaining = &self.decompress_buf[self.decompress_pos..];
            let chunk_len = remaining.len().min(CHUNK_SIZE);
            let data = remaining[..chunk_len].to_vec();
            self.decompress_pos += chunk_len;
            return Ok(CompressedToken::Literal(data));
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
            // upstream: token.c lines 814-822
            let len = read_deflated_data_length(reader, flag)?;
            self.compressed_input_buf.clear();
            self.compressed_input_buf.resize(len, 0);
            reader.read_exact(&mut self.compressed_input_buf)?;

            // Decompress the block
            self.decompress_buf.clear();
            let mut input_pos = 0;

            // upstream: token.c lines 846-863
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

                // upstream: if input consumed and output not full, go idle
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

            // No output produced, try next token
            return self.recv_token(reader);
        }

        if flag == END_FLAG {
            return Ok(CompressedToken::End);
        }

        // Token parsing - identical to zlib
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
}
