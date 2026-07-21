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

#[cfg(feature = "tokio-transfer")]
use super::step::drive_async;
use super::step::{DeflateSink, TokenDecodeCore, drive_sync};
use super::{
    CHUNK_SIZE, CompressedToken, END_FLAG, MAX_DATA_COUNT, TOKEN_LONG, TOKEN_REL, TOKENRUN_LONG,
    TOKENRUN_REL, write_deflated_data_pieces,
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
            // zlib tokens never carry a signed (zstd-only) level; clamp
            // defensively into zlib's 0..=9 range so flate2 cannot panic.
            CompressionLevel::PreciseSigned(v) => Compression::new(v.clamp(0, 9) as u32),
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
    ///
    /// # Overflow handling
    ///
    /// `Z_SYNC_FLUSH` can emit a stored-block header + payload + sync trailer
    /// that exceeds `compress_buf`. Upstream issue #951 (rsync 3.4.3) addressed
    /// the symmetric bug in `send_deflated_token()`: a single matched-block
    /// insert larger than the fixed `obuf` aborted with "deflate on token
    /// returned 0 (N bytes left)". The fix is to loop until the compressor
    /// has consumed the chunk *and* has no pending output buffered. The
    /// discarded output stays inside the deflate dictionary so the receiver's
    /// matching `see_token()` (see [`ZlibTokenDecoder::see_token`]) stays in
    /// lockstep without needing the bytes on the wire.
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

            // Feed the chunk through the compressor with Sync flush, looping
            // until the input is fully consumed AND the compressor has no
            // more pending output. A single Sync flush of a ~64 KiB
            // incompressible insert produces stored-block output > compress_buf;
            // the first compress() call consumes the input and fills the
            // output buffer, leaving residual output trapped inside the
            // deflate state. We must call compress() again with empty input
            // to drain the residue. Output bytes are discarded - only the
            // dictionary update side-effect matters.
            let mut input = chunk;
            loop {
                let before_in = self.compressor.total_in();
                let before_out = self.compressor.total_out();

                self.compressor
                    .compress(input, &mut self.compress_buf, FlushCompress::Sync)
                    .map_err(|e| io::Error::other(e.to_string()))?;

                let consumed = (self.compressor.total_in() - before_in) as usize;
                let produced = (self.compressor.total_out() - before_out) as usize;

                input = &input[consumed..];

                if input.is_empty() && produced < self.compress_buf.len() {
                    // Input fully consumed and last call only partially filled
                    // the output buffer: the Sync flush is complete. Includes
                    // the produced == 0 case (no residue left to drain).
                    break;
                }
            }

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

/// Zlib decompression sink: the algorithm-specific half of the sans-io decoder.
///
/// Owns the persistent inflate stream and a fixed CHUNK_SIZE output scratch, and
/// holds only the current DEFLATED_DATA block being inflated. Consecutive blocks
/// of a run form one deflate stream, fed block by block and inflated
/// incrementally: O(CHUNK_SIZE) memory for a multi-GB literal, matching upstream.
///
/// Reference: upstream token.c:recv_deflated_token() r_inflating/r_inflated
struct ZlibDeflate {
    decompressor: Decompress,
    output_buf: Vec<u8>,
    /// The current block payload being inflated.
    pending: Vec<u8>,
    /// Read cursor into `pending`.
    pending_pos: usize,
}

impl ZlibDeflate {
    fn new() -> Self {
        Self {
            decompressor: Decompress::new(false),
            output_buf: vec![0u8; CHUNK_SIZE],
            pending: Vec::with_capacity(MAX_DATA_COUNT),
            pending_pos: 0,
        }
    }

    /// Feeds one DEFLATED_DATA block into the persistent inflate stream.
    ///
    /// The bytes must persist across the multiple `stream_step` calls that drain
    /// this block, so they are copied into `pending` (bounded by MAX_DATA_COUNT).
    fn feed(&mut self, payload: &[u8]) {
        self.pending.clear();
        self.pending.extend_from_slice(payload);
        self.pending_pos = 0;
    }
}

impl DeflateSink for ZlibDeflate {
    fn accumulates(&self) -> bool {
        true
    }

    fn begin_block(&mut self, payload: &[u8]) {
        self.feed(payload);
    }

    fn push_block(&mut self, payload: &[u8]) -> io::Result<()> {
        self.feed(payload);
        Ok(())
    }

    fn stream_step(&mut self, output: &mut Vec<u8>) -> io::Result<bool> {
        // upstream: token.c recv_deflated_token() r_inflating - inflate the block
        // with Z_NO_FLUSH into a fixed dbuf, emitting each CHUNK_SIZE increment.
        loop {
            let before_in = self.decompressor.total_in();
            let before_out = self.decompressor.total_out();

            self.decompressor
                .decompress(
                    &self.pending[self.pending_pos..],
                    &mut self.output_buf,
                    FlushDecompress::None,
                )
                .map_err(|e| io::Error::other(e.to_string()))?;

            let consumed = (self.decompressor.total_in() - before_in) as usize;
            let produced = (self.decompressor.total_out() - before_out) as usize;
            self.pending_pos += consumed;

            if produced > 0 {
                // At most CHUNK_SIZE (output_buf length); the block may hold
                // more, so report not-yet-exhausted and let the core re-enter.
                output.extend_from_slice(&self.output_buf[..produced]);
                return Ok(false);
            }
            if self.pending_pos >= self.pending.len() {
                // Block fully consumed, decoder drained.
                return Ok(true);
            }
            if consumed == 0 {
                // No forward progress with input still pending: treat the block
                // as done to avoid a spin. A valid deflate stream never hits
                // this - the sender only splits at MAX_DATA_COUNT boundaries.
                return Ok(true);
            }
        }
    }

    fn finish_run(&mut self, output: &mut Vec<u8>) -> io::Result<()> {
        // upstream: token.c recv_deflated_token() r_inflated - restore the
        // Z_SYNC_FLUSH trailer the sender stripped and feed it through the
        // persistent inflate stream, draining any residual output.
        let marker = [0x00u8, 0x00, 0xFF, 0xFF];
        let mut input = &marker[..];

        loop {
            let before_in = self.decompressor.total_in();
            let before_out = self.decompressor.total_out();

            self.decompressor
                .decompress(input, &mut self.output_buf, FlushDecompress::Sync)
                .map_err(|e| io::Error::other(e.to_string()))?;

            let consumed = (self.decompressor.total_in() - before_in) as usize;
            let produced = (self.decompressor.total_out() - before_out) as usize;

            if consumed > 0 {
                input = &input[consumed..];
            }
            if produced > 0 {
                output.extend_from_slice(&self.output_buf[..produced]);
            }

            // Stop as soon as an inflate call makes no forward progress:
            // neither consuming the sync marker nor producing residual output.
            // This is zlib's Z_BUF_ERROR ("no pending output"), which upstream
            // treats as end-of-drain rather than a reason to keep flushing.
            // Without this an adversarial stream whose inflate state can neither
            // advance on nor consume the marker spins here forever (a DoS).
            // upstream: token.c:615-646 recv_deflated_token() r_inflated - a
            // single Z_SYNC_FLUSH per residual chunk, Z_BUF_ERROR ends the drain.
            if consumed == 0 && produced == 0 {
                break;
            }
        }

        Ok(())
    }
}

/// Zlib decoder state for receiving compressed tokens.
///
/// Manages a persistent inflate stream for decompressing literal data.
/// Restores the sync marker stripped by the encoder.
///
/// The decode/decompress logic lives in a sans-io state machine
/// ([`TokenDecodeCore`] + [`ZlibDeflate`]); [`recv_token`](Self::recv_token) is
/// a thin blocking driver over it, and [`recv_token_async`](Self::recv_token_async)
/// is the `.await` counterpart. Both share the exact same state machine, so
/// they stay byte-identical.
///
/// Reference: upstream token.c:recv_deflated_token()
pub(super) struct ZlibTokenDecoder {
    core: TokenDecodeCore,
    deflate: ZlibDeflate,
    is_zlibx: bool,
}

impl Default for ZlibTokenDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl ZlibTokenDecoder {
    pub(super) fn new() -> Self {
        Self {
            core: TokenDecodeCore::new(true),
            deflate: ZlibDeflate::new(),
            is_zlibx: false,
        }
    }

    pub(super) fn reset(&mut self) {
        self.core.reset();
        self.core.initialized = false;
        self.deflate.decompressor.reset(false);
        self.deflate.pending.clear();
        self.deflate.pending_pos = 0;
    }

    pub(super) fn recv_token<R: Read>(&mut self, reader: &mut R) -> io::Result<CompressedToken> {
        drive_sync(&mut self.core, &mut self.deflate, reader)
    }

    /// Async counterpart to [`recv_token`](Self::recv_token), backed by the same
    /// sans-io state machine. Only the byte fetch differs (`.await` vs blocking).
    #[cfg(feature = "tokio-transfer")]
    pub(super) async fn recv_token_async<R>(
        &mut self,
        reader: &mut R,
    ) -> io::Result<CompressedToken>
    where
        R: tokio::io::AsyncRead + Unpin + ?Sized,
    {
        drive_async(&mut self.core, &mut self.deflate, reader).await
    }

    pub(super) fn initialized(&self) -> bool {
        self.core.initialized
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
                let before_in = self.deflate.decompressor.total_in();
                let before_out = self.deflate.decompressor.total_out();

                self.deflate
                    .decompressor
                    .decompress(input, &mut self.deflate.output_buf, FlushDecompress::Sync)
                    .map_err(|e| io::Error::other(e.to_string()))?;

                let consumed = (self.deflate.decompressor.total_in() - before_in) as usize;
                if consumed > 0 {
                    input = &input[consumed..];
                }
                let produced = (self.deflate.decompressor.total_out() - before_out) as usize;

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
