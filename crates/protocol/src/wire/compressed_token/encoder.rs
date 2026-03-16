//! Encoder for compressed token wire format.
//!
//! Implements the sender side of rsync's compressed token protocol,
//! matching upstream `token.c:send_deflated_token()`.

use std::io::{self, Write};

use compress::zlib::CompressionLevel;
use flate2::{Compress, Compression, FlushCompress};

use super::{
    CHUNK_SIZE, END_FLAG, TOKEN_LONG, TOKEN_REL, TOKENRUN_LONG, TOKENRUN_REL,
    write_deflated_data_pieces,
};

/// Encoder state for sending compressed tokens.
///
/// Manages a persistent deflate stream for compressing literal data in the
/// rsync protocol. The encoder maintains state across multiple tokens to
/// achieve better compression ratios.
///
/// # Compression Strategy
///
/// - Uses Z_SYNC_FLUSH to produce incrementally decompressible output
/// - Strips the trailing 4-byte sync marker (0x00 0x00 0xFF 0xFF) from each chunk
/// - The receiver adds the marker back before inflating
/// - Maintains a persistent deflate context across the entire file transfer
///
/// # Protocol Compatibility
///
/// This implementation matches upstream rsync's `token.c:send_deflated_token()`
/// behavior. Protocol version affects the `see_token` method's behavior:
/// - Protocol >= 31: Properly advances through data (recommended)
/// - Protocol < 31: Has a data-duplicating bug in dictionary synchronization
///
/// # Examples
///
/// ```
/// use protocol::wire::CompressedTokenEncoder;
/// use compress::zlib::CompressionLevel;
///
/// let mut encoder = CompressedTokenEncoder::new(CompressionLevel::Default, 31);
/// let mut output = Vec::new();
///
/// // Send literal data
/// encoder.send_literal(&mut output, b"hello world").unwrap();
///
/// // Send block match
/// encoder.send_block_match(&mut output, 0).unwrap();
///
/// // Finish the stream
/// encoder.finish(&mut output).unwrap();
/// ```
pub struct CompressedTokenEncoder {
    /// Accumulated literal data to compress.
    literal_buf: Vec<u8>,
    /// Persistent deflate compressor.
    compressor: Compress,
    /// Output buffer for compression (scratch space for deflate output).
    compress_buf: Vec<u8>,
    /// Reusable buffer for accumulating compressed output before writing.
    /// Replaces per-block `Vec::new()` allocation in `flush_chunk`.
    /// Upstream: static `obuf` in token.c.
    flush_buf: Vec<u8>,
    /// Last token sent (for run encoding).
    last_token: i32,
    /// Start of current token run.
    run_start: i32,
    /// End of last token run.
    last_run_end: i32,
    /// Protocol version for compatibility.
    /// Protocol versions < 31 have a data-duplicating bug in `see_token`.
    protocol_version: u32,
    /// When `true`, [`Self::see_token`] is a no-op (CPRES_ZLIBX mode).
    ///
    /// In zlibx mode block-match tokens do not update the compressor
    /// dictionary; only literal bytes flow through the deflate context.
    is_zlibx: bool,
    /// Tracks whether data has been fed to the deflate compressor since the
    /// last sync flush. Needed because `compress_chunk_no_flush` may drain
    /// `literal_buf` (e.g. from `send_literal` at chunk boundaries) while
    /// buffering compressed output inside the deflate context - a subsequent
    /// `flush_all_literals` must still issue `sync_flush` even though
    /// `literal_buf` is empty.
    needs_flush: bool,
}

impl CompressedTokenEncoder {
    /// Creates a new encoder with the specified compression level and protocol version.
    ///
    /// # Arguments
    ///
    /// * `level` - Compression level (Fast, Default, Best, or Precise(1-9))
    /// * `protocol_version` - rsync protocol version (affects `see_token` behavior)
    ///
    /// # Protocol Version Behavior
    ///
    /// The protocol version affects `see_token` behavior:
    /// - Protocol >= 31: Properly advances through data (recommended)
    /// - Protocol < 31: Has a data-duplicating bug where the same data chunk is fed
    ///   multiple times to the compressor dictionary
    ///
    /// # Examples
    ///
    /// ```
    /// use protocol::wire::CompressedTokenEncoder;
    /// use compress::zlib::CompressionLevel;
    ///
    /// // Recommended: protocol 31 with default compression
    /// let encoder = CompressedTokenEncoder::new(CompressionLevel::Default, 31);
    ///
    /// // Fast compression for better performance
    /// let fast_encoder = CompressedTokenEncoder::new(CompressionLevel::Fast, 31);
    /// ```
    #[must_use]
    pub fn new(level: CompressionLevel, protocol_version: u32) -> Self {
        let compression = match level {
            CompressionLevel::None => Compression::new(0),
            CompressionLevel::Fast => Compression::fast(),
            CompressionLevel::Default => Compression::default(),
            CompressionLevel::Best => Compression::best(),
            CompressionLevel::Precise(n) => Compression::new(u32::from(n.get())),
        };
        Self {
            literal_buf: Vec::new(),
            compressor: Compress::new(compression, false), // false = raw deflate (-15 window)
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

    /// Resets the encoder for a new file.
    ///
    /// Clears all internal state including the compression context, allowing
    /// the encoder to be reused for a new file transfer. This is more efficient
    /// than creating a new encoder.
    pub fn reset(&mut self) {
        self.literal_buf.clear();
        self.compressor.reset();
        self.flush_buf.clear();
        self.last_token = -1;
        self.run_start = 0;
        self.last_run_end = 0;
        self.needs_flush = false;
    }

    /// Sends literal data with compression.
    ///
    /// Accumulates data in an internal buffer and compresses it when the buffer
    /// reaches CHUNK_SIZE (32 KiB). Data smaller than the chunk size is buffered
    /// until more data arrives or [`finish`](Self::finish) is called.
    ///
    /// # Arguments
    ///
    /// * `writer` - The output stream to write compressed data to
    /// * `data` - The literal data to send (will be buffered and compressed)
    ///
    /// # Errors
    ///
    /// Returns an error if compression or writing fails.
    #[must_use]
    pub fn send_literal<W: Write>(&mut self, writer: &mut W, data: &[u8]) -> io::Result<()> {
        self.literal_buf.extend_from_slice(data);

        // upstream: compress full chunks with Z_NO_FLUSH as data accumulates
        while self.literal_buf.len() >= CHUNK_SIZE {
            self.compress_chunk_no_flush(writer)?;
        }

        Ok(())
    }

    /// Sends a block match token.
    ///
    /// Flushes any pending compressed literal data and writes a token indicating
    /// that the receiver should copy data from the specified block in the basis file.
    /// Uses run-length encoding to efficiently represent consecutive block matches.
    ///
    /// # Arguments
    ///
    /// * `writer` - The output stream to write to
    /// * `block_index` - The 0-based index of the block to copy from the basis file
    ///
    /// # Errors
    ///
    /// Returns an error if flushing or writing fails.
    pub fn send_block_match<W: Write>(
        &mut self,
        writer: &mut W,
        block_index: u32,
    ) -> io::Result<()> {
        let token = block_index as i32;
        let has_literals = !self.literal_buf.is_empty();

        // upstream token.c lines 363-400: write previous token run BEFORE
        // flushing literals, so DEFLATED_DATA always follows a token on
        // the wire (never two DEFLATED_DATA groups from separate flushes).
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

    /// Signals end of file and flushes all pending data.
    ///
    /// Flushes any remaining literal data, outputs the final token run if any,
    /// and writes the END_FLAG to signal completion. Also resets the encoder
    /// state for potential reuse.
    ///
    /// # Arguments
    ///
    /// * `writer` - The output stream to write to
    ///
    /// # Errors
    ///
    /// Returns an error if flushing or writing the end marker fails.
    #[must_use]
    pub fn finish<W: Write>(&mut self, writer: &mut W) -> io::Result<()> {
        // upstream token.c lines 460-462: output token run BEFORE literal
        // data, matching the wire ordering where DEFLATED_DATA always
        // follows a token (never precedes it without a separator).
        if self.last_token >= 0 {
            self.write_token_run(writer)?;
        }

        self.flush_all_literals(writer)?;

        writer.write_all(&[END_FLAG])?;

        self.reset();
        Ok(())
    }

    /// Compresses one chunk of literal data with `Z_NO_FLUSH`.
    ///
    /// Upstream token.c feeds `CHUNK_SIZE` blocks to deflate with `Z_NO_FLUSH`,
    /// writing any produced output as `DEFLATED_DATA` blocks. The sync flush
    /// only happens in [`Self::sync_flush`] when a token or end marker follows.
    ///
    /// Reference: upstream token.c lines 409-418 (input feeding loop).
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

            // upstream: writes DEFLATED_DATA when output buffer fills
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

    /// Performs `Z_SYNC_FLUSH` and writes remaining compressed output.
    ///
    /// Upstream token.c sets `flush = Z_SYNC_FLUSH` when all literal input has
    /// been consumed (`nb == 0`) and the token is not a continuation (`-2`).
    /// The trailing 4-byte sync marker (`0x00 0x00 0xFF 0xFF`) is stripped
    /// from the final output; the receiver's inflate state machine re-inserts
    /// it when transitioning from compressed data to a non-DEFLATED_DATA flag.
    ///
    /// Reference: upstream token.c lines 433-454.
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

            // Z_SYNC_FLUSH is complete when deflate returns Ok, or when
            // no more output is produced (BufError with 0 output).
            if status == flate2::Status::Ok || produced == 0 {
                break;
            }
        }

        // upstream: strips the trailing sync marker from the last DEFLATED_DATA
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

    /// Flushes all pending literal data with a final `Z_SYNC_FLUSH`.
    ///
    /// Compresses remaining data with `Z_NO_FLUSH`, then performs a sync
    /// flush to produce a decompressible boundary. This matches upstream's
    /// pattern of only sync-flushing at token/end boundaries.
    fn flush_all_literals<W: Write>(&mut self, writer: &mut W) -> io::Result<()> {
        while !self.literal_buf.is_empty() {
            self.compress_chunk_no_flush(writer)?;
        }
        // Only sync flush if data was actually fed to the compressor.
        // `needs_flush` tracks this across calls - `send_literal` may drain
        // `literal_buf` via `compress_chunk_no_flush` at chunk boundaries,
        // leaving `literal_buf` empty here while deflate still holds output.
        if self.needs_flush {
            self.sync_flush(writer)?;
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

    /// Feeds block data into the compressor's history without producing output.
    ///
    /// This is called after sending a block match token to keep the compressor's
    /// dictionary synchronized with what the receiver sees. The receiver must call
    /// [`super::CompressedTokenDecoder::see_token`] with the same data.
    ///
    /// Only needed for CPRES_ZLIB mode (not zlibx/zstd/lz4).
    ///
    /// **Protocol version behavior:**
    /// - Protocol >= 31: Properly advances through data after each chunk
    /// - Protocol < 31: Has a data-duplicating bug where `offset` is not advanced,
    ///   causing the first chunk of data to be fed repeatedly for each 0xFFFF-sized
    ///   iteration. The loop still terminates because `toklen` is decremented.
    ///
    /// # Errors
    ///
    /// Returns an error if the compression operation fails.
    ///
    /// Reference: upstream token.c lines 463-484 (`send_deflated_token`).
    #[must_use]
    pub fn see_token(&mut self, data: &[u8]) -> io::Result<()> {
        // CPRES_ZLIBX: block-match tokens never update the deflate dictionary.
        if self.is_zlibx {
            return Ok(());
        }
        let mut toklen = data.len();
        let mut offset = 0usize;

        while toklen > 0 {
            // Break up long sections at 0xFFFF boundary (matching upstream)
            let chunk_len = toklen.min(0xFFFF);
            let chunk = &data[offset..offset + chunk_len];

            // Decrement toklen (this always happens, even with the bug)
            toklen -= chunk_len;

            // Feed data through deflate with Z_SYNC_FLUSH (Z_INSERT_ONLY equivalent)
            // This updates the compressor's dictionary without producing real output
            self.compressor
                .compress(chunk, &mut self.compress_buf, FlushCompress::Sync)
                .map_err(|e| io::Error::other(e.to_string()))?;

            // Protocol >= 31 fixes a data-duplicating bug by advancing the offset.
            // Protocol < 31 does NOT advance offset, feeding the same first chunk
            // of data repeatedly while toklen decrements normally.
            // Reference: token.c lines 473-474
            if self.protocol_version >= 31 {
                offset += chunk_len;
            }
            // In protocol < 31, offset stays at 0, so we keep feeding data[0..chunk_len]
        }

        Ok(())
    }

    /// Configures zlibx mode for this encoder.
    ///
    /// When `true`, [`Self::see_token`] becomes a no-op, matching upstream
    /// rsync's CPRES_ZLIBX behaviour. The flag persists across [`Self::reset`]
    /// calls because the compression algorithm is fixed for the session.
    pub fn set_zlibx(&mut self, zlibx: bool) {
        self.is_zlibx = zlibx;
    }
}

impl Default for CompressedTokenEncoder {
    fn default() -> Self {
        Self::new(CompressionLevel::Default, 31)
    }
}
