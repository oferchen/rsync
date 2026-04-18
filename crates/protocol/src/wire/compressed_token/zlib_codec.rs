//! Zlib/zlibx per-token codec for compressed token wire format.
//!
//! Implements the zlib-specific encoder and decoder used by CPRES_ZLIB and
//! CPRES_ZLIBX modes. These are the original rsync compression codecs.
//!
//! - upstream: token.c:send_deflated_token() (CPRES_ZLIB/CPRES_ZLIBX)
//! - upstream: token.c:recv_deflated_token() (CPRES_ZLIB/CPRES_ZLIBX)

use std::io::{self, Read, Write};

use compress::zlib::CompressionLevel;
use flate2::{Compress, Compression, Decompress, FlushCompress, FlushDecompress};

use super::{
    CHUNK_SIZE, CompressedToken, DEFLATED_DATA, END_FLAG, MAX_DATA_COUNT, TOKEN_LONG, TOKEN_REL,
    TOKENRUN_LONG, TOKENRUN_REL, read_deflated_data_length, write_deflated_data_pieces,
};

/// Zlib encoder state for sending compressed tokens.
///
/// Manages a persistent deflate stream for compressing literal data.
/// Uses Z_SYNC_FLUSH with trailing sync marker stripping.
///
/// Reference: upstream token.c:send_deflated_token()
pub(super) struct ZlibTokenEncoder {
    literal_buf: Vec<u8>,
    compressor: Compress,
    compress_buf: Vec<u8>,
    flush_buf: Vec<u8>,
    last_token: i32,
    run_start: i32,
    last_run_end: i32,
    protocol_version: u32,
    is_zlibx: bool,
    needs_flush: bool,
}

impl ZlibTokenEncoder {
    /// Creates a new zlib encoder with the specified compression level and protocol version.
    pub(super) fn new(level: CompressionLevel, protocol_version: u32) -> Self {
        let compression = match level {
            CompressionLevel::None => Compression::new(0),
            CompressionLevel::Fast => Compression::fast(),
            CompressionLevel::Default => Compression::default(),
            CompressionLevel::Best => Compression::best(),
            CompressionLevel::Precise(n) => Compression::new(u32::from(n.get())),
        };
        Self {
            literal_buf: Vec::new(),
            compressor: Compress::new(compression, false),
            compress_buf: vec![0u8; CHUNK_SIZE * 2],
            flush_buf: Vec::with_capacity(CHUNK_SIZE * 2),
            last_token: -1,
            run_start: 0,
            last_run_end: 0,
            protocol_version,
            is_zlibx: false,
            needs_flush: false,
        }
    }

    pub(super) fn reset(&mut self) {
        self.literal_buf.clear();
        self.compressor.reset();
        self.flush_buf.clear();
        self.last_token = -1;
        self.run_start = 0;
        self.last_run_end = 0;
        self.needs_flush = false;
    }

    pub(super) fn send_literal<W: Write>(&mut self, writer: &mut W, data: &[u8]) -> io::Result<()> {
        self.literal_buf.extend_from_slice(data);
        while self.literal_buf.len() >= CHUNK_SIZE {
            self.compress_chunk_no_flush(writer)?;
        }
        Ok(())
    }

    pub(super) fn send_block_match<W: Write>(
        &mut self,
        writer: &mut W,
        block_index: u32,
    ) -> io::Result<()> {
        let token = block_index as i32;
        let has_literals = !self.literal_buf.is_empty();

        if self.last_token == -1 || self.last_token == -2 {
            self.flush_all_literals(writer)?;
            self.run_start = token;
        } else if has_literals || token != self.last_token + 1 || token >= self.run_start + 65536 {
            self.write_token_run(writer)?;
            self.flush_all_literals(writer)?;
            self.run_start = token;
        }

        self.last_token = token;
        Ok(())
    }

    pub(super) fn finish<W: Write>(&mut self, writer: &mut W) -> io::Result<()> {
        if self.last_token >= 0 {
            self.write_token_run(writer)?;
        }
        self.flush_all_literals(writer)?;
        writer.write_all(&[END_FLAG])?;
        self.reset();
        Ok(())
    }

    /// Feeds block data into the compressor's dictionary.
    ///
    /// Only active in CPRES_ZLIB mode (noop for zlibx).
    /// Reference: upstream token.c lines 463-484.
    pub(super) fn see_token(&mut self, data: &[u8]) -> io::Result<()> {
        if self.is_zlibx {
            return Ok(());
        }
        let mut toklen = data.len();
        let mut offset = 0usize;

        while toklen > 0 {
            let chunk_len = toklen.min(0xFFFF);
            let chunk = &data[offset..offset + chunk_len];
            toklen -= chunk_len;

            self.compressor
                .compress(chunk, &mut self.compress_buf, FlushCompress::Sync)
                .map_err(|e| io::Error::other(e.to_string()))?;

            if self.protocol_version >= 31 {
                offset += chunk_len;
            }
        }
        Ok(())
    }

    pub(super) fn set_zlibx(&mut self, zlibx: bool) {
        self.is_zlibx = zlibx;
    }

    fn compress_chunk_no_flush<W: Write>(&mut self, writer: &mut W) -> io::Result<()> {
        if self.literal_buf.is_empty() {
            return Ok(());
        }

        let chunk_len = self.literal_buf.len().min(CHUNK_SIZE);
        self.needs_flush = true;

        let mut consumed_total = 0;
        while consumed_total < chunk_len {
            let input = &self.literal_buf[consumed_total..chunk_len];
            let before_in = self.compressor.total_in();
            let before_out = self.compressor.total_out();

            self.compressor
                .compress(input, &mut self.compress_buf, FlushCompress::None)
                .map_err(|e| io::Error::other(e.to_string()))?;

            let consumed = (self.compressor.total_in() - before_in) as usize;
            let produced = (self.compressor.total_out() - before_out) as usize;

            if produced > 0 {
                write_deflated_data_pieces(writer, &self.compress_buf[..produced])?;
            }

            consumed_total += consumed;
            if consumed == 0 && produced < self.compress_buf.len() {
                break;
            }
        }

        self.literal_buf.drain(..chunk_len);
        Ok(())
    }

    fn sync_flush<W: Write>(&mut self, writer: &mut W) -> io::Result<()> {
        self.flush_buf.clear();

        loop {
            let before_out = self.compressor.total_out();
            let status = self
                .compressor
                .compress(&[], &mut self.compress_buf, FlushCompress::Sync)
                .map_err(|e| io::Error::other(e.to_string()))?;

            let produced = (self.compressor.total_out() - before_out) as usize;
            if produced > 0 {
                self.flush_buf
                    .extend_from_slice(&self.compress_buf[..produced]);
            }

            if status == flate2::Status::Ok || produced == 0 {
                break;
            }
        }

        // upstream: strips trailing sync marker
        if self.flush_buf.len() >= 4 {
            let len = self.flush_buf.len();
            if self.flush_buf[len - 4..] == [0x00, 0x00, 0xFF, 0xFF] {
                self.flush_buf.truncate(len - 4);
            }
        }

        if !self.flush_buf.is_empty() {
            write_deflated_data_pieces(writer, &self.flush_buf)?;
        }

        self.needs_flush = false;
        Ok(())
    }

    fn flush_all_literals<W: Write>(&mut self, writer: &mut W) -> io::Result<()> {
        while !self.literal_buf.is_empty() {
            self.compress_chunk_no_flush(writer)?;
        }
        if self.needs_flush {
            self.sync_flush(writer)?;
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

/// Zlib decoder state for receiving compressed tokens.
///
/// Manages a persistent inflate stream for decompressing literal data.
/// Restores the sync marker stripped by the encoder.
///
/// Reference: upstream token.c:recv_deflated_token()
pub(super) struct ZlibTokenDecoder {
    decompress_buf: Vec<u8>,
    decompress_pos: usize,
    decompressor: Decompress,
    output_buf: Vec<u8>,
    compressed_input_buf: Vec<u8>,
    rx_token: i32,
    rx_run: i32,
    pub(super) initialized: bool,
    is_zlibx: bool,
    saved_flag: Option<u8>,
}

impl Default for ZlibTokenDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl ZlibTokenDecoder {
    pub(super) fn new() -> Self {
        Self {
            decompress_buf: Vec::new(),
            decompress_pos: 0,
            decompressor: Decompress::new(false),
            output_buf: vec![0u8; CHUNK_SIZE * 2],
            compressed_input_buf: Vec::with_capacity(MAX_DATA_COUNT + 4),
            rx_token: 0,
            rx_run: 0,
            initialized: false,
            is_zlibx: false,
            saved_flag: None,
        }
    }

    pub(super) fn reset(&mut self) {
        self.decompress_buf.clear();
        self.decompress_pos = 0;
        self.decompressor.reset(false);
        self.compressed_input_buf.clear();
        self.rx_token = 0;
        self.rx_run = 0;
        self.initialized = false;
        self.saved_flag = None;
    }

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
        if self.rx_run > 0 {
            self.rx_run -= 1;
            self.rx_token += 1;
            return Ok(CompressedToken::BlockMatch(self.rx_token as u32));
        }

        let flag = if let Some(f) = self.saved_flag.take() {
            f
        } else {
            let mut buf = [0u8; 1];
            reader.read_exact(&mut buf)?;
            buf[0]
        };

        if (flag & 0xC0) == DEFLATED_DATA {
            self.compressed_input_buf.clear();
            let len = read_deflated_data_length(reader, flag)?;
            let start = self.compressed_input_buf.len();
            self.compressed_input_buf.resize(start + len, 0);
            reader.read_exact(&mut self.compressed_input_buf[start..start + len])?;

            // Accumulate consecutive DEFLATED_DATA blocks
            loop {
                let mut peek = [0u8; 1];
                reader.read_exact(&mut peek)?;
                let next_flag = peek[0];

                if (next_flag & 0xC0) == DEFLATED_DATA {
                    let next_len = read_deflated_data_length(reader, next_flag)?;
                    let s = self.compressed_input_buf.len();
                    self.compressed_input_buf.resize(s + next_len, 0);
                    reader.read_exact(&mut self.compressed_input_buf[s..s + next_len])?;
                } else {
                    self.saved_flag = Some(next_flag);
                    break;
                }
            }

            // Restore sync marker stripped by encoder
            self.compressed_input_buf
                .extend_from_slice(&[0x00, 0x00, 0xFF, 0xFF]);

            self.decompress_buf.clear();
            let mut input = &self.compressed_input_buf[..];

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

                if input.is_empty() || (consumed == 0 && produced == 0) {
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

            return self.recv_token(reader);
        }

        if flag == END_FLAG {
            return Ok(CompressedToken::End);
        }

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

    /// Feeds block data into the decompressor's dictionary.
    ///
    /// Uses fake deflate stored-block headers to feed raw data through inflate,
    /// concatenated into a single buffer per chunk. This ensures the inflate
    /// engine sees the complete stored block (header + payload) atomically,
    /// avoiding partial-block state issues between separate decompress calls.
    ///
    /// upstream: token.c:see_deflate_token() lines 631-670 - feeds header then
    /// data in separate inflate() calls within the same do/while loop, relying
    /// on zlib's stateful stream. With flate2/miniz_oxide, a single call with
    /// the concatenated input is more robust.
    pub(super) fn see_token(&mut self, data: &[u8]) -> io::Result<()> {
        if self.is_zlibx {
            return Ok(());
        }
        let mut remaining = data;
        let mut combined = Vec::new();

        while !remaining.is_empty() {
            let chunk_len = remaining.len().min(0xFFFF);
            let chunk = &remaining[..chunk_len];

            let len_lo = (chunk_len & 0xFF) as u8;
            let len_hi = ((chunk_len >> 8) & 0xFF) as u8;

            // Build a single buffer with stored-block header + payload.
            // upstream: token.c:see_deflate_token() - hdr[0]=0x00 (stored block,
            // not final), hdr[1..2]=len LE, hdr[3..4]=~len LE.
            combined.clear();
            combined.reserve(5 + chunk_len);
            combined.extend_from_slice(&[0x00, len_lo, len_hi, !len_lo, !len_hi]);
            combined.extend_from_slice(chunk);

            // Feed the complete stored block in one call so inflate processes
            // header + payload together without intermediate flush boundaries.
            let mut input = &combined[..];
            loop {
                let before_in = self.decompressor.total_in();
                let before_out = self.decompressor.total_out();

                self.decompressor
                    .decompress(input, &mut self.output_buf, FlushDecompress::Sync)
                    .map_err(|e| io::Error::other(e.to_string()))?;

                let consumed = (self.decompressor.total_in() - before_in) as usize;
                if consumed > 0 {
                    input = &input[consumed..];
                }
                let produced = (self.decompressor.total_out() - before_out) as usize;

                if input.is_empty() || (consumed == 0 && produced == 0) {
                    break;
                }
            }

            remaining = &remaining[chunk_len..];
        }
        Ok(())
    }

    pub(super) fn set_zlibx(&mut self, zlibx: bool) {
        self.is_zlibx = zlibx;
    }
}
