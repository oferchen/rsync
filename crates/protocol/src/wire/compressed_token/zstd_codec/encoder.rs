//! Zstd token encoder for the compressed token wire format.
//!
//! Implements the zstd-specific encoder used by CPRES_ZSTD mode. Maintains a
//! single persistent compression context across the entire transfer session,
//! flushing at each token boundary with `ZSTD_e_flush`.
//!
//! - upstream: token.c:send_zstd_token() lines 678-776

use std::io::{self, Write};

use zstd::stream::raw::{Encoder as ZstdRawEncoder, Operation};

use super::super::{
    END_FLAG, MAX_DATA_COUNT, TOKEN_LONG, TOKEN_REL, TOKENRUN_LONG, TOKENRUN_REL,
    write_deflated_data_header,
};

/// Zstd encoder state for sending compressed tokens.
///
/// Maintains a single persistent `ZSTD_CCtx` across the **entire transfer
/// session** (all files). Upstream rsync never resets or reinitializes the
/// zstd context between files - the session is one continuous zstd stream
/// with `ZSTD_e_flush` sync points at token boundaries.
///
/// Between files, only the token run-encoding state (last_token, run_start,
/// last_run_end, flush_pending) is reset, matching upstream token.c:700-703.
/// The compression context preserves cross-file dictionary/history.
///
/// Compressed output is accumulated in a `MAX_DATA_COUNT`-sized buffer.
/// A DEFLATED_DATA block is written only when the buffer is full (during
/// `ZSTD_e_continue`) or after each `ZSTD_e_flush` call, matching upstream's
/// output pattern in token.c:send_zstd_token().
///
/// upstream: token.c:send_zstd_token() - CCtx created once (line 688),
/// never reset between files (line 700-703 only resets run state)
pub(in crate::wire::compressed_token) struct ZstdTokenEncoder {
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
    ///
    /// `workers` plumbs `--compress-threads=N` through to zstd's
    /// `ZSTD_c_nbWorkers`. `None` keeps the encoder single-threaded,
    /// matching upstream's `do_compression_threads = 0` default.
    ///
    /// upstream: token.c:701 - `ZSTD_CCtx_setParameter(zstd_cctx, ZSTD_c_nbWorkers, do_compression_threads)`
    pub(in crate::wire::compressed_token) fn new(
        level: i32,
        workers: Option<std::num::NonZeroU8>,
    ) -> io::Result<Self> {
        let mut encoder = ZstdRawEncoder::new(level)?;
        if let Some(n) = workers {
            // Silently ignore failures - zstd rejects ZSTD_c_nbWorkers
            // when built without multi-thread support (the `zstdmt` Cargo
            // feature). The encoder still works in single-threaded mode.
            let _ =
                encoder.set_parameter(zstd::stream::raw::CParameter::NbWorkers(u32::from(n.get())));
        }
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

    /// Resets token run-encoding state for a new file.
    ///
    /// Only resets the run-encoding variables (last_token, run_start,
    /// last_run_end, flush_pending) and pending literal data. The zstd
    /// compression context is NOT reinitialized - upstream rsync uses a
    /// single continuous stream across all files in the session.
    ///
    /// upstream: token.c:700-703 - only resets last_run_end, run_start,
    /// flush_pending when last_token == -1 (new file boundary)
    pub(in crate::wire::compressed_token) fn reset(&mut self) {
        self.literal_buf.clear();
        self.output_pos = 0;
        self.last_token = -1;
        self.run_start = 0;
        self.last_run_end = 0;
        self.flush_pending = false;
    }

    pub(in crate::wire::compressed_token) fn send_literal<W: Write>(
        &mut self,
        _writer: &mut W,
        data: &[u8],
    ) -> io::Result<()> {
        self.literal_buf.extend_from_slice(data);
        Ok(())
    }

    pub(in crate::wire::compressed_token) fn send_block_match<W: Write>(
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

    /// Signals end of the current file's token stream.
    ///
    /// Flushes pending literals and run-encoding, writes the END_FLAG byte,
    /// then resets only the run-encoding state for the next file. The zstd
    /// compression context is preserved - upstream rsync maintains one
    /// continuous stream across all files.
    ///
    /// upstream: token.c:772-775 - writes END_FLAG, does NOT reset CCtx
    pub(in crate::wire::compressed_token) fn finish<W: Write>(
        &mut self,
        writer: &mut W,
    ) -> io::Result<()> {
        if self.last_token >= 0 {
            self.write_token_run(writer)?;
        }
        self.compress_and_flush(writer)?;
        writer.write_all(&[END_FLAG])?;
        // upstream: token.c:700-703 - only run state resets between files
        self.last_token = -1;
        self.run_start = 0;
        self.last_run_end = 0;
        self.flush_pending = false;
        Ok(())
    }

    /// Noop for zstd - no dictionary synchronization needed.
    /// upstream: token.c:1102-1104 (see_token for CPRES_ZSTD is empty)
    pub(in crate::wire::compressed_token) fn see_token(&mut self, _data: &[u8]) -> io::Result<()> {
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
