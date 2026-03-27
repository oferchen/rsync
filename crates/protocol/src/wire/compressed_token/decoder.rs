//! Decoder for compressed token wire format.
//!
//! Implements the receiver side of rsync's compressed token protocol,
//! matching upstream `token.c:recv_deflated_token()`.

use std::io::{self, Read};

use flate2::{Decompress, FlushDecompress};

use super::{
    CHUNK_SIZE, CompressedToken, DEFLATED_DATA, END_FLAG, MAX_DATA_COUNT, TOKEN_LONG, TOKEN_REL,
    read_deflated_data_length,
};

/// Decoder state for receiving compressed tokens.
///
/// Manages a persistent inflate stream for decompressing literal data in the
/// rsync protocol. The decoder maintains state across multiple tokens to match
/// the encoder's persistent deflate context.
///
/// # Decompression Strategy
///
/// - Uses Z_SYNC_FLUSH to incrementally decompress data
/// - Adds back the 4-byte sync marker (0x00 0x00 0xFF 0xFF) that the encoder stripped
/// - Maintains a persistent inflate context across the entire file transfer
/// - Buffers decompressed data and returns it in chunks
///
/// # Examples
///
/// ```no_run
/// use protocol::wire::{CompressedTokenDecoder, CompressedToken};
/// use std::io::Cursor;
///
/// let mut decoder = CompressedTokenDecoder::new();
/// # let encoded_data: Vec<u8> = vec![]; // Placeholder
/// let mut cursor = Cursor::new(&encoded_data);
///
/// loop {
///     match decoder.recv_token(&mut cursor)? {
///         CompressedToken::Literal(data) => {
///             // Write literal data to output
///         }
///         CompressedToken::BlockMatch(index) => {
///             // Copy block from basis file
///         }
///         CompressedToken::End => break,
///     }
/// }
/// # Ok::<(), std::io::Error>(())
/// ```
pub struct CompressedTokenDecoder {
    /// Decompression buffer for accumulated output.
    decompress_buf: Vec<u8>,
    /// Current position in decompress buffer.
    decompress_pos: usize,
    /// Persistent inflate decompressor.
    decompressor: Decompress,
    /// Output buffer for decompression (scratch space for inflate output).
    output_buf: Vec<u8>,
    /// Reusable buffer for compressed input data read from the wire.
    /// Upstream: static `cbuf` in token.c (line 493).
    compressed_input_buf: Vec<u8>,
    /// Current token index.
    rx_token: i32,
    /// Remaining tokens in current run.
    rx_run: i32,
    /// Decoder initialized flag.
    pub(super) initialized: bool,
    /// When `true`, [`Self::see_token`] is a no-op (CPRES_ZLIBX mode).
    is_zlibx: bool,
    /// Flag byte saved from a previous read, to be re-processed on the next
    /// call. Used when the peek-ahead loop reads a non-DEFLATED_DATA flag
    /// after accumulating consecutive compressed blocks.
    /// Upstream: `saved_flag` in token.c line 503.
    saved_flag: Option<u8>,
}

impl Default for CompressedTokenDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl CompressedTokenDecoder {
    /// Creates a new decoder.
    ///
    /// Initializes a decoder with a fresh inflate context ready to receive
    /// compressed tokens.
    #[must_use]
    pub fn new() -> Self {
        Self {
            decompress_buf: Vec::new(),
            decompress_pos: 0,
            decompressor: Decompress::new(false), // raw deflate (-15 window)
            output_buf: vec![0u8; CHUNK_SIZE * 2],
            compressed_input_buf: Vec::with_capacity(MAX_DATA_COUNT + 4),
            rx_token: 0,
            rx_run: 0,
            initialized: false,
            is_zlibx: false,
            saved_flag: None,
        }
    }

    /// Resets the decoder for a new file.
    ///
    /// Clears all internal state including the decompression context and buffers,
    /// allowing the decoder to be reused for a new file transfer.
    pub fn reset(&mut self) {
        self.decompress_buf.clear();
        self.decompress_pos = 0;
        self.decompressor.reset(false);
        self.compressed_input_buf.clear();
        self.rx_token = 0;
        self.rx_run = 0;
        self.initialized = false;
        self.saved_flag = None;
    }

    /// Receives the next token from the stream.
    ///
    /// Reads and decodes the next token from the compressed stream. Automatically
    /// decompresses literal data and returns it in chunks. Buffers decompressed
    /// data internally to handle partial reads efficiently.
    ///
    /// # Returns
    ///
    /// - `Ok(CompressedToken::Literal(data))` - Literal data to write to output
    /// - `Ok(CompressedToken::BlockMatch(index))` - Copy from block index in basis file
    /// - `Ok(CompressedToken::End)` - End of file marker
    ///
    /// Receives the next compressed token from the stream.
    ///
    /// Mirrors upstream `recv_deflated_token()` (token.c lines 500-625).
    ///
    /// Consecutive DEFLATED_DATA blocks are accumulated into a single buffer.
    /// The sync marker (`0x00 0x00 0xFF 0xFF`) stripped by the encoder is
    /// appended only when the next flag is not DEFLATED_DATA, matching
    /// upstream's state transition from `r_inflated` to `r_idle`.
    pub fn recv_token<R: Read>(&mut self, reader: &mut R) -> io::Result<CompressedToken> {
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

        // upstream: token.c:618-622 r_running - emit pending run tokens.
        // Upstream increments rx_token BEFORE returning, so run tokens
        // are run_start+1, run_start+2, etc. (run_start itself was the
        // initial token returned without increment).
        if self.rx_run > 0 {
            self.rx_run -= 1;
            self.rx_token += 1;
            return Ok(CompressedToken::BlockMatch(self.rx_token as u32));
        }

        // Read next flag (or re-process saved flag)
        let flag = if let Some(f) = self.saved_flag.take() {
            f
        } else {
            let mut buf = [0u8; 1];
            reader.read_exact(&mut buf)?;
            buf[0]
        };

        if (flag & 0xC0) == DEFLATED_DATA {
            // Accumulate all consecutive DEFLATED_DATA blocks, then append
            // the sync marker once at the boundary - matching upstream's
            // pattern where the sync marker is only restored when
            // transitioning from compressed data to a non-compressed flag.
            self.compressed_input_buf.clear();
            let len = read_deflated_data_length(reader, flag)?;
            let start = self.compressed_input_buf.len();
            self.compressed_input_buf.resize(start + len, 0);
            reader.read_exact(&mut self.compressed_input_buf[start..start + len])?;

            // Peek ahead: read more DEFLATED_DATA blocks if consecutive
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
                    // Save the non-DEFLATED_DATA flag for the next call
                    self.saved_flag = Some(next_flag);
                    break;
                }
            }

            // Append the sync marker stripped by the encoder
            self.compressed_input_buf
                .extend_from_slice(&[0x00, 0x00, 0xFF, 0xFF]);

            // Decompress the complete segment
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

        // upstream: token.c:588-599 - parse token encoding.
        // rx_token is NOT incremented after the initial return; only r_running
        // increments before returning. This keeps rx_token at the last returned
        // value, matching the encoder's last_run_end = last_token convention.
        if flag & TOKEN_REL != 0 {
            let rel = (flag & 0x3F) as i32;
            self.rx_token += rel;

            // upstream: bit 6 distinguishes TOKEN_REL from TOKENRUN_REL
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

    /// Feeds block data into the decompressor's history.
    ///
    /// This is called after receiving a block match token to keep the decompressor's
    /// dictionary synchronized with the sender's compressor. The sender must call
    /// [`super::CompressedTokenEncoder::see_token`] with the same data.
    ///
    /// Uses a fake deflate stored-block header to feed raw data through inflate
    /// without actual decompression, matching upstream rsync's `see_deflate_token()`.
    ///
    /// # Errors
    ///
    /// Returns an error if the decompression operation fails.
    ///
    /// Reference: upstream token.c lines 631-670.
    pub fn see_token(&mut self, data: &[u8]) -> io::Result<()> {
        // CPRES_ZLIBX: block-match tokens never update the inflate dictionary.
        if self.is_zlibx {
            return Ok(());
        }
        let mut remaining = data;

        while !remaining.is_empty() {
            // Break up long sections at 0xFFFF boundary (matching upstream)
            let chunk_len = remaining.len().min(0xFFFF);
            let chunk = &remaining[..chunk_len];

            // Create a fake stored-block header
            // Format: [0x00, len_lo, len_hi, ~len_lo, ~len_hi]
            // 0x00 = stored block (not final)
            let len_lo = (chunk_len & 0xFF) as u8;
            let len_hi = ((chunk_len >> 8) & 0xFF) as u8;
            let header = [0x00, len_lo, len_hi, !len_lo, !len_hi];

            // Feed the stored-block header
            self.decompressor
                .decompress(&header, &mut self.output_buf, FlushDecompress::Sync)
                .map_err(|e| io::Error::other(e.to_string()))?;

            // Feed the actual data
            self.decompressor
                .decompress(chunk, &mut self.output_buf, FlushDecompress::Sync)
                .map_err(|e| io::Error::other(e.to_string()))?;

            remaining = &remaining[chunk_len..];
        }

        Ok(())
    }

    /// Configures zlibx mode for this decoder.
    ///
    /// When `true`, [`Self::see_token`] becomes a no-op. Must be set to the
    /// same value as the paired encoder's flag.
    pub fn set_zlibx(&mut self, zlibx: bool) {
        self.is_zlibx = zlibx;
    }
}
