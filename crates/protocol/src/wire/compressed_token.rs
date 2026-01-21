//! Compressed token wire format for file reconstruction.
//!
//! This module implements the compressed token format used by rsync when
//! compression is enabled (-z flag). It wraps literal data in DEFLATED_DATA
//! headers and encodes block match tokens efficiently.
//!
//! ## Wire Format (from upstream token.c)
//!
//! Flag bytes in compressed stream:
//! - `END_FLAG (0x00)` - end of file marker
//! - `TOKEN_LONG (0x20)` - followed by 32-bit token number
//! - `TOKENRUN_LONG (0x21)` - followed by 32-bit token + 16-bit run count
//! - `DEFLATED_DATA (0x40)` - + 6-bit high len, then low len byte
//! - `TOKEN_REL (0x80)` - + 6-bit relative token number
//! - `TOKENRUN_REL (0xC0)` - + 6-bit relative token + 16-bit run count
//!
//! ## DEFLATED_DATA Format
//!
//! ```text
//! Byte 0: 0x40 | (len >> 8)   // DEFLATED_DATA flag + upper 6 bits of length
//! Byte 1: len & 0xFF         // lower 8 bits of length
//! Bytes 2..: compressed data (raw deflate, no zlib header)
//! ```
//!
//! Maximum data count is 16383 (14 bits).
//!
//! ## Compression Model
//!
//! Upstream rsync maintains a single deflate stream per file transfer, using
//! `Z_SYNC_FLUSH` to produce incrementally decompressible output. This differs
//! from compressing each chunk independently with `Z_FINISH`.
//!
//! ## References
//!
//! - `token.c` lines 321-329: flag byte definitions
//! - `token.c:send_deflated_token()` lines 357-485
//! - `token.c:recv_deflated_token()` lines 500-630

use std::io::{self, Read, Write};

use compress::zlib::CompressionLevel;
use flate2::{Compress, Compression, Decompress, FlushCompress, FlushDecompress};

/// End of file marker.
pub const END_FLAG: u8 = 0x00;

/// Followed by 32-bit token number.
pub const TOKEN_LONG: u8 = 0x20;

/// Followed by 32-bit token + 16-bit run count.
pub const TOKENRUN_LONG: u8 = 0x21;

/// Compressed data follows: + 6-bit high len, then low len byte.
pub const DEFLATED_DATA: u8 = 0x40;

/// Relative token: + 6-bit relative token number.
pub const TOKEN_REL: u8 = 0x80;

/// Relative token run: + 6-bit relative token + 16-bit run count.
pub const TOKENRUN_REL: u8 = 0xC0;

/// Maximum compressed data count (14 bits).
pub const MAX_DATA_COUNT: usize = 16383;

/// Chunk size for compression input.
pub const CHUNK_SIZE: usize = 32 * 1024;

/// Encoder state for sending compressed tokens.
///
/// Uses a persistent deflate stream with Z_SYNC_FLUSH, stripping the trailing
/// 4-byte sync marker (0x00 0x00 0xFF 0xFF) from output. The receiver adds
/// the marker back before inflating.
///
/// This matches upstream rsync's token.c:send_deflated_token() behavior.
pub struct CompressedTokenEncoder {
    /// Accumulated literal data to compress.
    literal_buf: Vec<u8>,
    /// Persistent deflate compressor.
    compressor: Compress,
    /// Output buffer for compression.
    compress_buf: Vec<u8>,
    /// Last token sent (for run encoding).
    last_token: i32,
    /// Start of current token run.
    run_start: i32,
    /// End of last token run.
    last_run_end: i32,
}

impl CompressedTokenEncoder {
    /// Creates a new encoder with the specified compression level.
    #[must_use]
    pub fn new(level: CompressionLevel) -> Self {
        let compression = match level {
            CompressionLevel::Fast => Compression::fast(),
            CompressionLevel::Default => Compression::default(),
            CompressionLevel::Best => Compression::best(),
            CompressionLevel::Precise(n) => Compression::new(u32::from(n.get())),
        };
        Self {
            literal_buf: Vec::new(),
            compressor: Compress::new(compression, false), // false = raw deflate (-15 window)
            compress_buf: vec![0u8; CHUNK_SIZE * 2],
            last_token: -1,
            run_start: 0,
            last_run_end: 0,
        }
    }

    /// Resets the encoder for a new file.
    pub fn reset(&mut self) {
        self.literal_buf.clear();
        self.compressor.reset();
        self.last_token = -1;
        self.run_start = 0;
        self.last_run_end = 0;
    }

    /// Sends literal data with compression.
    ///
    /// Accumulates data and compresses when the buffer is full.
    pub fn send_literal<W: Write>(&mut self, writer: &mut W, data: &[u8]) -> io::Result<()> {
        self.literal_buf.extend_from_slice(data);

        // Flush if buffer is large enough
        while self.literal_buf.len() >= CHUNK_SIZE {
            self.flush_chunk(writer)?;
        }

        Ok(())
    }

    /// Sends a block match token.
    ///
    /// Flushes any pending compressed data and writes the token.
    pub fn send_block_match<W: Write>(
        &mut self,
        writer: &mut W,
        block_index: u32,
    ) -> io::Result<()> {
        let token = block_index as i32;

        // Flush pending literal data
        self.flush_all_literals(writer)?;

        // Write token using run-length encoding
        if self.last_token == -1 || self.last_token == -2 {
            self.run_start = token;
        } else if token != self.last_token + 1 || token >= self.run_start + 65536 {
            // Output previous run
            self.write_token_run(writer)?;
            self.run_start = token;
        }

        self.last_token = token;
        Ok(())
    }

    /// Signals end of file and flushes all pending data.
    pub fn finish<W: Write>(&mut self, writer: &mut W) -> io::Result<()> {
        // Flush any pending literal data
        self.flush_all_literals(writer)?;

        // Output final token run if any
        if self.last_token >= 0 {
            self.write_token_run(writer)?;
        }

        // Write end marker
        writer.write_all(&[END_FLAG])?;

        self.reset();
        Ok(())
    }

    /// Flushes one chunk of literal data using Z_SYNC_FLUSH.
    ///
    /// Uses the persistent deflate stream and strips the trailing 4-byte
    /// sync marker (0x00 0x00 0xFF 0xFF) from output. The receiver adds
    /// the marker back before inflating.
    ///
    /// Reference: token.c:send_deflated_token() lines 449-457
    fn flush_chunk<W: Write>(&mut self, writer: &mut W) -> io::Result<()> {
        if self.literal_buf.is_empty() {
            return Ok(());
        }

        let chunk_len = self.literal_buf.len().min(CHUNK_SIZE);
        let chunk: Vec<u8> = self.literal_buf.drain(..chunk_len).collect();

        let mut compressed = Vec::new();

        // Feed all input with Z_NO_FLUSH
        let mut input = &chunk[..];
        while !input.is_empty() {
            let before_in = self.compressor.total_in();
            let before_out = self.compressor.total_out();

            self.compressor
                .compress(input, &mut self.compress_buf, FlushCompress::None)
                .map_err(|e| io::Error::other(e.to_string()))?;

            let consumed = (self.compressor.total_in() - before_in) as usize;
            let produced = (self.compressor.total_out() - before_out) as usize;

            compressed.extend_from_slice(&self.compress_buf[..produced]);
            input = &input[consumed..];

            if consumed == 0 && produced < self.compress_buf.len() {
                break;
            }
        }

        // Flush with Z_SYNC_FLUSH
        loop {
            let before_out = self.compressor.total_out();

            let status = self
                .compressor
                .compress(&[], &mut self.compress_buf, FlushCompress::Sync)
                .map_err(|e| io::Error::other(e.to_string()))?;

            let produced = (self.compressor.total_out() - before_out) as usize;
            if produced > 0 {
                compressed.extend_from_slice(&self.compress_buf[..produced]);
            }

            // Stop when sync flush is complete
            if status == flate2::Status::Ok {
                break;
            }
        }

        // Strip the trailing 4-byte sync marker (0x00 0x00 0xFF 0xFF)
        // The receiver will add it back before inflating
        if compressed.len() >= 4 {
            let len = compressed.len();
            if compressed[len - 4..] == [0x00, 0x00, 0xFF, 0xFF] {
                compressed.truncate(len - 4);
            }
        }

        // Write in MAX_DATA_COUNT pieces with DEFLATED_DATA headers
        let mut offset = 0;
        while offset < compressed.len() {
            let piece_len = (compressed.len() - offset).min(MAX_DATA_COUNT);
            write_deflated_data_header(writer, piece_len)?;
            writer.write_all(&compressed[offset..offset + piece_len])?;
            offset += piece_len;
        }

        Ok(())
    }

    /// Flushes all pending literal data.
    fn flush_all_literals<W: Write>(&mut self, writer: &mut W) -> io::Result<()> {
        while !self.literal_buf.is_empty() {
            self.flush_chunk(writer)?;
        }
        Ok(())
    }

    /// Writes the current token run.
    fn write_token_run<W: Write>(&mut self, writer: &mut W) -> io::Result<()> {
        let r = self.run_start - self.last_run_end;
        let n = self.last_token - self.run_start;

        if (0..=63).contains(&r) {
            // Use relative encoding
            let flag = if n == 0 { TOKEN_REL } else { TOKENRUN_REL };
            writer.write_all(&[flag + r as u8])?;
        } else {
            // Use absolute encoding
            let flag = if n == 0 { TOKEN_LONG } else { TOKENRUN_LONG };
            writer.write_all(&[flag])?;
            writer.write_all(&(self.run_start).to_le_bytes())?;
        }

        if n != 0 {
            writer.write_all(&[(n & 0xFF) as u8])?;
            writer.write_all(&[((n >> 8) & 0xFF) as u8])?;
        }

        // Update to where the decoder's rx_token will be after emitting these tokens.
        // A run from run_start to last_token emits (last_token - run_start + 1) tokens,
        // and the decoder advances rx_token after each, ending at last_token + 1.
        self.last_run_end = self.last_token + 1;
        Ok(())
    }
}

impl Default for CompressedTokenEncoder {
    fn default() -> Self {
        Self::new(CompressionLevel::Default)
    }
}

/// Decoder state for receiving compressed tokens.
///
/// Uses a persistent inflate stream that matches the encoder's persistent
/// deflate stream.
pub struct CompressedTokenDecoder {
    /// Decompression buffer.
    decompress_buf: Vec<u8>,
    /// Current position in decompress buffer.
    decompress_pos: usize,
    /// Persistent inflate decompressor.
    decompressor: Decompress,
    /// Output buffer for decompression.
    output_buf: Vec<u8>,
    /// Current token index.
    rx_token: i32,
    /// Remaining tokens in current run.
    rx_run: i32,
    /// Decoder initialized flag.
    initialized: bool,
}

impl Default for CompressedTokenDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl CompressedTokenDecoder {
    /// Creates a new decoder.
    #[must_use]
    pub fn new() -> Self {
        Self {
            decompress_buf: Vec::new(),
            decompress_pos: 0,
            decompressor: Decompress::new(false), // false = raw deflate (-15 window)
            output_buf: vec![0u8; CHUNK_SIZE * 2],
            rx_token: 0,
            rx_run: 0,
            initialized: false,
        }
    }

    /// Resets the decoder for a new file.
    pub fn reset(&mut self) {
        self.decompress_buf.clear();
        self.decompress_pos = 0;
        self.decompressor.reset(false);
        self.rx_token = 0;
        self.rx_run = 0;
        self.initialized = false;
    }

    /// Receives the next token from the stream.
    ///
    /// Returns:
    /// - `Ok(CompressedToken::Literal(data))` - literal data to write
    /// - `Ok(CompressedToken::BlockMatch(index))` - copy from block index
    /// - `Ok(CompressedToken::End)` - end of file
    pub fn recv_token<R: Read>(&mut self, reader: &mut R) -> io::Result<CompressedToken> {
        // Initialize on first call
        if !self.initialized {
            self.initialized = true;
        }

        // Return buffered decompressed data if available
        if self.decompress_pos < self.decompress_buf.len() {
            let remaining = &self.decompress_buf[self.decompress_pos..];
            let chunk_len = remaining.len().min(CHUNK_SIZE);
            let data = remaining[..chunk_len].to_vec();
            self.decompress_pos += chunk_len;
            return Ok(CompressedToken::Literal(data));
        }

        // Check for pending token run
        if self.rx_run > 0 {
            self.rx_run -= 1;
            let token = self.rx_token;
            self.rx_token += 1;
            return Ok(CompressedToken::BlockMatch(token as u32));
        }

        // Read next flag byte
        let mut flag_buf = [0u8; 1];
        reader.read_exact(&mut flag_buf)?;
        let flag = flag_buf[0];

        // Check for DEFLATED_DATA
        if (flag & 0xC0) == DEFLATED_DATA {
            let len = read_deflated_data_length(reader, flag)?;
            let mut compressed = vec![0u8; len];
            reader.read_exact(&mut compressed)?;

            // Add the sync marker back that the encoder stripped
            // (0x00 0x00 0xFF 0xFF)
            compressed.extend_from_slice(&[0x00, 0x00, 0xFF, 0xFF]);

            // Decompress using persistent decompressor with sync flush
            self.decompress_buf.clear();
            let mut input = &compressed[..];

            loop {
                let before_in = self.decompressor.total_in();
                let before_out = self.decompressor.total_out();

                self.decompressor
                    .decompress(input, &mut self.output_buf, FlushDecompress::Sync)
                    .map_err(|e| io::Error::other(e.to_string()))?;

                let consumed = (self.decompressor.total_in() - before_in) as usize;
                let produced = (self.decompressor.total_out() - before_out) as usize;

                if produced > 0 {
                    self.decompress_buf
                        .extend_from_slice(&self.output_buf[..produced]);
                }

                if consumed > 0 {
                    input = &input[consumed..];
                }

                // Done when all input consumed and no more output
                if input.is_empty() && produced == 0 {
                    break;
                }
            }

            self.decompress_pos = 0;

            // Return first chunk
            if !self.decompress_buf.is_empty() {
                let chunk_len = self.decompress_buf.len().min(CHUNK_SIZE);
                let data = self.decompress_buf[..chunk_len].to_vec();
                self.decompress_pos = chunk_len;
                return Ok(CompressedToken::Literal(data));
            }

            // Empty compressed data - read next token
            return self.recv_token(reader);
        }

        // Check for END_FLAG
        if flag == END_FLAG {
            return Ok(CompressedToken::End);
        }

        // Parse token encoding
        match flag & 0xE0 {
            0x20 => {
                // TOKEN_LONG or TOKENRUN_LONG
                let mut buf = [0u8; 4];
                reader.read_exact(&mut buf)?;
                self.rx_token = i32::from_le_bytes(buf);

                if flag == TOKENRUN_LONG {
                    let mut run_buf = [0u8; 2];
                    reader.read_exact(&mut run_buf)?;
                    self.rx_run = u16::from_le_bytes(run_buf) as i32;
                }

                let token = self.rx_token;
                self.rx_token += 1;
                // rx_run will be decremented in subsequent calls at the top of recv_token
                Ok(CompressedToken::BlockMatch(token as u32))
            }
            0x80 | 0xC0 => {
                // TOKEN_REL or TOKENRUN_REL
                let rel = (flag & 0x3F) as i32;
                self.rx_token += rel;

                if (flag & 0xE0) == 0xC0 {
                    // TOKENRUN_REL
                    let mut run_buf = [0u8; 2];
                    reader.read_exact(&mut run_buf)?;
                    self.rx_run = u16::from_le_bytes(run_buf) as i32;
                }

                let token = self.rx_token;
                self.rx_token += 1;
                // rx_run will be decremented in subsequent calls at the top of recv_token
                Ok(CompressedToken::BlockMatch(token as u32))
            }
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("invalid compressed token flag: 0x{flag:02X}"),
            )),
        }
    }
}

/// A token received from a compressed stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompressedToken {
    /// Literal data to write to output.
    Literal(Vec<u8>),
    /// Copy from block index in basis file.
    BlockMatch(u32),
    /// End of file marker.
    End,
}

/// Writes a DEFLATED_DATA header.
#[inline]
fn write_deflated_data_header<W: Write>(writer: &mut W, len: usize) -> io::Result<()> {
    debug_assert!(len <= MAX_DATA_COUNT);
    let header = [DEFLATED_DATA | ((len >> 8) as u8), (len & 0xFF) as u8];
    writer.write_all(&header)
}

/// Reads the length from a DEFLATED_DATA header.
#[inline]
fn read_deflated_data_length<R: Read>(reader: &mut R, first_byte: u8) -> io::Result<usize> {
    let high = (first_byte & 0x3F) as usize;
    let mut low_buf = [0u8; 1];
    reader.read_exact(&mut low_buf)?;
    Ok((high << 8) | (low_buf[0] as usize))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn deflated_data_header_roundtrip() {
        for len in [0, 1, 100, 1000, MAX_DATA_COUNT] {
            let mut buf = Vec::new();
            write_deflated_data_header(&mut buf, len).unwrap();
            assert_eq!(buf.len(), 2);

            let first_byte = buf[0];
            let mut cursor = Cursor::new(&buf[1..]);
            let decoded_len = read_deflated_data_length(&mut cursor, first_byte).unwrap();
            assert_eq!(decoded_len, len);
        }
    }

    #[test]
    fn deflated_data_header_format() {
        let mut buf = Vec::new();
        write_deflated_data_header(&mut buf, 0x1234).unwrap();

        // 0x1234 = 4660
        // high 6 bits: 0x12 = 18
        // low 8 bits: 0x34 = 52
        assert_eq!(buf[0], DEFLATED_DATA | 0x12);
        assert_eq!(buf[1], 0x34);
    }

    #[test]
    fn encode_decode_literal_roundtrip() {
        let data = b"Hello, compressed world! This is a test of the compression system.";

        let mut encoded = Vec::new();
        let mut encoder = CompressedTokenEncoder::new(CompressionLevel::Default);
        encoder.send_literal(&mut encoded, data).unwrap();
        encoder.finish(&mut encoded).unwrap();

        let mut cursor = Cursor::new(&encoded);
        let mut decoder = CompressedTokenDecoder::new();

        let mut decoded = Vec::new();
        loop {
            match decoder.recv_token(&mut cursor).unwrap() {
                CompressedToken::Literal(chunk) => decoded.extend_from_slice(&chunk),
                CompressedToken::End => break,
                CompressedToken::BlockMatch(_) => panic!("unexpected block match"),
            }
        }

        assert_eq!(decoded, data);
    }

    #[test]
    fn encode_decode_block_match() {
        let mut encoded = Vec::new();
        let mut encoder = CompressedTokenEncoder::new(CompressionLevel::Default);
        encoder.send_block_match(&mut encoded, 0).unwrap();
        encoder.send_block_match(&mut encoded, 1).unwrap();
        encoder.send_block_match(&mut encoded, 2).unwrap();
        encoder.finish(&mut encoded).unwrap();

        let mut cursor = Cursor::new(&encoded);
        let mut decoder = CompressedTokenDecoder::new();

        let mut blocks = Vec::new();
        loop {
            match decoder.recv_token(&mut cursor).unwrap() {
                CompressedToken::BlockMatch(idx) => blocks.push(idx),
                CompressedToken::End => break,
                CompressedToken::Literal(_) => {}
            }
        }

        assert_eq!(blocks, vec![0, 1, 2]);
    }

    #[test]
    fn encode_decode_mixed() {
        let literal1 = b"first literal data";
        let literal2 = b"second literal";

        let mut encoded = Vec::new();
        let mut encoder = CompressedTokenEncoder::new(CompressionLevel::Default);
        encoder.send_literal(&mut encoded, literal1).unwrap();
        encoder.send_block_match(&mut encoded, 5).unwrap();
        encoder.send_literal(&mut encoded, literal2).unwrap();
        encoder.send_block_match(&mut encoded, 10).unwrap();
        encoder.finish(&mut encoded).unwrap();

        let mut cursor = Cursor::new(&encoded);
        let mut decoder = CompressedTokenDecoder::new();

        let mut literals = Vec::new();
        let mut blocks = Vec::new();

        loop {
            match decoder.recv_token(&mut cursor).unwrap() {
                CompressedToken::Literal(data) => literals.push(data),
                CompressedToken::BlockMatch(idx) => blocks.push(idx),
                CompressedToken::End => break,
            }
        }

        let combined: Vec<u8> = literals.into_iter().flatten().collect();
        let expected: Vec<u8> = [literal1.as_slice(), literal2.as_slice()].concat();
        assert_eq!(combined, expected);
        assert_eq!(blocks, vec![5, 10]);
    }

    #[test]
    fn max_data_count_fits_in_14_bits() {
        // 0x3FFF = 16383 = 2^14 - 1 (14 bits)
        assert_eq!(MAX_DATA_COUNT, 16383);
    }

    #[test]
    fn flag_constants_match_upstream() {
        assert_eq!(END_FLAG, 0x00);
        assert_eq!(TOKEN_LONG, 0x20);
        assert_eq!(TOKENRUN_LONG, 0x21);
        assert_eq!(DEFLATED_DATA, 0x40);
        assert_eq!(TOKEN_REL, 0x80);
        assert_eq!(TOKENRUN_REL, 0xC0);
    }
}
