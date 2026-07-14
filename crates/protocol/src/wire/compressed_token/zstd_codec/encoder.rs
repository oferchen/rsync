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
    CHUNK_SIZE, END_FLAG, MAX_DATA_COUNT, TOKEN_LONG, TOKEN_REL, TOKENRUN_LONG, TOKENRUN_REL,
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
/// last_run_end, needs_flush) is reset, matching upstream token.c:756-778.
/// The compression context preserves cross-file dictionary/history.
///
/// Literal data is streamed into the zstd context in `CHUNK_SIZE` (32 KiB)
/// units as it arrives, so the staging buffer never holds more than one chunk
/// regardless of how large a literal run is. This mirrors upstream, which feeds
/// each `nb`-byte literal span straight through the compressor with a bounded
/// output buffer rather than materialising the whole run.
///
/// Compressed output is accumulated in a `MAX_DATA_COUNT`-sized buffer.
/// A DEFLATED_DATA block is written only when the buffer is full (during
/// `ZSTD_e_continue`) or after each `ZSTD_e_flush` call, matching upstream's
/// output pattern in token.c:send_zstd_token().
///
/// upstream: token.c:send_zstd_token() - CCtx created once (line 740),
/// never reset between files (line 756-778 only resets run state); literals fed
/// through a bounded output buffer (line 783-823)
pub(in crate::wire::compressed_token) struct ZstdTokenEncoder {
    /// Persistent zstd compression context.
    encoder: ZstdRawEncoder<'static>,
    /// Output buffer for compression results.
    /// Sized to `MAX_DATA_COUNT` to match upstream's `obuf` (token.c line 746).
    output_buf: Vec<u8>,
    /// Current write position in `output_buf`.
    /// upstream: zstd_out_buff.pos
    output_pos: usize,
    /// Bounded staging buffer for literal data awaiting compression.
    ///
    /// Never grows past `CHUNK_SIZE`: [`send_literal`](Self::send_literal)
    /// drains it a chunk at a time into the zstd stream, leaving at most the
    /// sub-chunk tail behind for the terminating flush.
    literal_buf: Vec<u8>,
    /// Last token sent (for run encoding).
    last_token: i32,
    /// Start of current token run.
    run_start: i32,
    /// End of last token run.
    last_run_end: i32,
    /// Whether literal data has been fed to the zstd context but not yet
    /// flushed. Drives the terminating `ZSTD_e_flush` at each token boundary
    /// even when the staging buffer already drained to the wire.
    needs_flush: bool,
    /// Whether the current literal region's preceding token run has already
    /// been written to the wire.
    ///
    /// A literal run breaks the token run, so upstream writes that run
    /// (token.c:762-779) before compressing the literal span (token.c:783).
    /// Because oc streams literals eagerly, the run must be emitted when the
    /// first literal of a region arrives - before any DEFLATED_DATA block - so
    /// the wire order stays `[token run][literal data]`. This flag ensures the
    /// terminating `send_block_match`/`finish` does not write it a second time.
    literal_pending_run_written: bool,
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
            needs_flush: false,
            literal_pending_run_written: false,
        })
    }

    /// Resets token run-encoding state for a new file.
    ///
    /// Only resets the run-encoding variables (last_token, run_start,
    /// last_run_end, needs_flush, literal_pending_run_written) and pending
    /// literal data. The zstd compression context is NOT reinitialized -
    /// upstream rsync uses a single continuous stream across all files in the
    /// session.
    ///
    /// upstream: token.c:756-778 - only resets last_run_end, run_start,
    /// flush_pending when last_token == -1 (new file boundary)
    pub(in crate::wire::compressed_token) fn reset(&mut self) {
        self.literal_buf.clear();
        self.output_pos = 0;
        self.last_token = -1;
        self.run_start = 0;
        self.last_run_end = 0;
        self.needs_flush = false;
        self.literal_pending_run_written = false;
    }

    /// Streams literal data into the zstd context in `CHUNK_SIZE` units.
    ///
    /// Only the sub-chunk tail is buffered between calls, so a large literal run
    /// is compressed incrementally in constant memory rather than materialising
    /// the whole span. The wire bytes are unchanged: `ZSTD_e_continue` is
    /// chunking-invariant, so feeding a run in CHUNK_SIZE pieces produces the
    /// same compressed stream as feeding it in one shot, and the terminating
    /// flush still happens only at the token boundary.
    ///
    /// A literal region breaks any open token run, so upstream emits that run
    /// before compressing the literal span (token.c:762-783). Because output is
    /// streamed eagerly here, the pending run is written when the first literal
    /// of the region arrives - before any DEFLATED_DATA block - preserving the
    /// `[token run][literal data]` wire order.
    ///
    /// upstream: token.c:783-823 (the `if (nb || flush_pending)` block)
    pub(in crate::wire::compressed_token) fn send_literal<W: Write>(
        &mut self,
        writer: &mut W,
        data: &[u8],
    ) -> io::Result<()> {
        if data.is_empty() {
            return Ok(());
        }

        // upstream: token.c:762-779 - a literal run breaks the open token run,
        // which must be written before the literal data (token.c:783).
        if self.last_token >= 0 && !self.literal_pending_run_written {
            self.write_token_run(writer)?;
            self.literal_pending_run_written = true;
        }

        self.literal_buf.extend_from_slice(data);
        while self.literal_buf.len() >= CHUNK_SIZE {
            self.compress_chunk_continue(writer, CHUNK_SIZE)?;
        }
        Ok(())
    }

    pub(in crate::wire::compressed_token) fn send_block_match<W: Write>(
        &mut self,
        writer: &mut W,
        block_index: u32,
    ) -> io::Result<()> {
        let token = block_index as i32;

        // upstream: token.c lines 756-779 - same run encoding as zlib.
        // A literal region's run was already written by `send_literal`
        // (`literal_pending_run_written`); the remaining branches mirror the
        // "output previous run" cases that are not triggered by literals.
        if self.last_token == -1 || self.last_token == -2 {
            self.compress_and_flush(writer)?;
            self.run_start = token;
        } else if self.literal_pending_run_written {
            self.compress_and_flush(writer)?;
            self.literal_pending_run_written = false;
            self.run_start = token;
        } else if token != self.last_token + 1 || token >= self.run_start + 65536 {
            self.write_token_run(writer)?;
            self.compress_and_flush(writer)?;
            self.run_start = token;
        }

        self.last_token = token;
        Ok(())
    }

    /// Returns the number of literal bytes currently staged awaiting
    /// compression. Used by tests to assert the staging buffer stays bounded.
    #[cfg(test)]
    pub(in crate::wire::compressed_token) fn staging_len(&self) -> usize {
        self.literal_buf.len()
    }

    /// Signals end of the current file's token stream.
    ///
    /// Flushes pending literals and run-encoding, writes the END_FLAG byte,
    /// then resets only the run-encoding state for the next file. The zstd
    /// compression context is preserved - upstream rsync maintains one
    /// continuous stream across all files.
    ///
    /// upstream: token.c:828-831 - writes END_FLAG, does NOT reset CCtx
    pub(in crate::wire::compressed_token) fn finish<W: Write>(
        &mut self,
        writer: &mut W,
    ) -> io::Result<()> {
        // A trailing literal region already emitted its run in `send_literal`
        // (`literal_pending_run_written`); only write the run here when it is
        // still open (a trailing block-match run).
        if self.last_token >= 0 && !self.literal_pending_run_written {
            self.write_token_run(writer)?;
        }
        self.compress_and_flush(writer)?;
        writer.write_all(&[END_FLAG])?;
        // upstream: token.c:756-778 - only run state resets between files
        self.last_token = -1;
        self.run_start = 0;
        self.last_run_end = 0;
        self.needs_flush = false;
        self.literal_pending_run_written = false;
        Ok(())
    }

    /// Noop for zstd - no dictionary synchronization needed.
    /// upstream: token.c:1102-1104 (see_token for CPRES_ZSTD is empty)
    pub(in crate::wire::compressed_token) fn see_token(&mut self, _data: &[u8]) -> io::Result<()> {
        Ok(())
    }

    /// Feeds `len` bytes from the front of `literal_buf` into the zstd stream
    /// with `ZSTD_e_continue`, draining compressed output into `output_buf` and
    /// emitting a DEFLATED_DATA block whenever it fills. No flush is performed,
    /// so the wire bytes are identical to feeding the whole literal run at once;
    /// only the staging buffer stays bounded to `CHUNK_SIZE`.
    ///
    /// upstream: token.c:790-818 - the `ZSTD_e_continue` portion of the
    /// do/while loop, which drains a bounded output buffer.
    fn compress_chunk_continue<W: Write>(&mut self, writer: &mut W, len: usize) -> io::Result<()> {
        self.needs_flush = true;

        let mut input_pos = 0;
        while input_pos < len {
            // upstream: token.c:791-792 - reset the output buffer when full.
            if self.output_pos == MAX_DATA_COUNT {
                self.write_output_buffer(writer)?;
            }

            {
                let mut in_buf =
                    zstd::stream::raw::InBuffer::around(&self.literal_buf[input_pos..len]);
                let mut out_buf =
                    zstd::stream::raw::OutBuffer::around(&mut self.output_buf[self.output_pos..]);

                self.encoder.run(&mut in_buf, &mut out_buf)?;
                input_pos += in_buf.pos();
                self.output_pos += out_buf.pos();
            }

            // upstream: token.c:812-817 - write when the buffer is full.
            if self.output_pos == MAX_DATA_COUNT {
                self.write_output_buffer(writer)?;
            }
        }

        self.literal_buf.drain(..len);
        Ok(())
    }

    /// Drains any staged literals and flushes the zstd encoder at a token
    /// boundary.
    ///
    /// Mirrors upstream token.c:783-823. Full `CHUNK_SIZE` pieces are already
    /// streamed by [`send_literal`](Self::send_literal); this drains the
    /// remaining sub-chunk tail with `ZSTD_e_continue`, then performs the single
    /// `ZSTD_e_flush` that produces a decompressible boundary. Output is
    /// accumulated in a single `MAX_DATA_COUNT` buffer and written as
    /// DEFLATED_DATA blocks only when the buffer fills or on flush.
    ///
    /// upstream: token.c:783-823 (the `if (nb || flush_pending)` block)
    fn compress_and_flush<W: Write>(&mut self, writer: &mut W) -> io::Result<()> {
        // Drain the sub-chunk tail (at most one iteration; the loop also copes
        // with a direct call carrying more than a chunk).
        while !self.literal_buf.is_empty() {
            let len = self.literal_buf.len().min(CHUNK_SIZE);
            self.compress_chunk_continue(writer, len)?;
        }

        // Nothing was fed since the last flush - no boundary to emit.
        if !self.needs_flush {
            return Ok(());
        }

        // upstream: token.c:795-798 - ZSTD_e_flush produces a decompressible
        // boundary. After each flush call, write whatever is in the output
        // buffer (even if not full).
        loop {
            let remaining;
            {
                let mut out_buf =
                    zstd::stream::raw::OutBuffer::around(&mut self.output_buf[self.output_pos..]);
                remaining = self.encoder.flush(&mut out_buf)?;
                self.output_pos += out_buf.pos();
            }

            // upstream: token.c:812-817 - write when buffer full OR flushing.
            if self.output_pos > 0 {
                self.write_output_buffer(writer)?;
            }

            if remaining == 0 {
                break;
            }
        }

        self.needs_flush = false;
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
