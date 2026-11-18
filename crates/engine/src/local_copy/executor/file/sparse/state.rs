//! Sparse write state machine for tracking pending zero runs.
//!
//! Maintains the offset of the current pending zero-byte region during a
//! sequential write pass, converting zero runs into seeks (holes) and
//! flushing them with hole-punch or trailing-zero finalization.

// upstream: fileio.c:write_sparse() - sparse write buffering logic

use std::fs;
use std::io::SeekFrom;
use std::path::Path;

use fast_io::zero_detect::{SparseSpan, sparse_spans};

use crate::block_touch::BlockTouchTracker;
use crate::local_copy::LocalCopyError;

use super::SPARSE_WRITE_SIZE;
use super::hole_punch::SparseSink;

/// Tracks pending zero runs during sparse file writing.
///
/// Mirrors upstream `write_sparse()`: consecutive zero bytes are accumulated
/// and flushed as a single seek or hole-punch when non-zero data (or
/// finalization) follows. A zero run is punched into a hole when it starts
/// inside the file's preallocated extent (`start < preallocated_len`) and
/// seeked over otherwise, matching upstream's `sparse_past_write >=
/// preallocated_len` decision.
///
/// The start of a pending run is the writer's position just past the last data
/// write. The state reads that position from the writer once and then tracks
/// it through every write, seek and punch it issues, so callers must not
/// reposition the writer between sparse calls: the run would start at the
/// tracked offset, not where the writer was moved to.
#[derive(Default)]
pub(crate) struct SparseWriteState {
    /// Accumulated pending zero bytes (upstream `sparse_seek`).
    pending_zero_run: u64,
    /// Length of the destination's preallocated extent (upstream
    /// `preallocated_len`). Zero runs starting below this offset are punched
    /// into holes; runs starting at or beyond it are seeked over.
    preallocated_len: u64,
    /// Logical file offset of the next byte handed in, advanced by every chunk
    /// whether its bytes are written, seeked over, or deferred as a hole.
    /// upstream: the caller-tracked `offset` passed to `write_file()`.
    logical_offset: u64,
    /// Distinct 4 KiB blocks written by data spans. Seek-mode spans (in-place
    /// matched blocks) and zero runs are never credited.
    /// upstream: fileio.c:168-169,179-180 - `if (!use_seek)
    /// track_block_touches(f, offset + start, ...)`.
    touched: BlockTouchTracker,
    /// The writer's position, read from the writer once and then advanced by
    /// every write, seek and punch this state issues, so a hole costs one
    /// `lseek` rather than a position query plus the seek.
    /// upstream: fileio.c:186 `sparse_past_write = offset + len - l2` - the
    /// position is tracked, never queried.
    position: Option<u64>,
}

impl SparseWriteState {
    /// Records the preallocated length so zero runs inside the reserved extent
    /// are punched out rather than merely seeked over (which would leave the
    /// preallocated blocks allocated).
    /// upstream: fileio.c:88 - if (sparse_past_write >= preallocated_len)
    pub(crate) const fn set_preallocated_len(&mut self, len: u64) {
        self.preallocated_len = len;
    }

    /// Sets the logical file offset of the first byte this state will see.
    ///
    /// Non-zero only when an append resumes past an existing prefix, so block
    /// accounting credits the blocks the resumed data actually lands in.
    pub(crate) const fn set_start_offset(&mut self, offset: u64) {
        self.logical_offset = offset;
    }

    /// Returns the number of distinct 4 KiB blocks the data spans wrote.
    pub(crate) const fn touched_blocks(&self) -> u64 {
        self.touched.total()
    }

    /// Extends the pending zero run by `additional` bytes, saturating on overflow.
    pub(super) const fn accumulate(&mut self, additional: usize) {
        self.pending_zero_run = self.pending_zero_run.saturating_add(additional as u64);
    }

    /// Returns the writer's position, querying the writer only the first time.
    fn position<W: SparseSink>(
        &mut self,
        writer: &mut W,
        destination: &Path,
    ) -> Result<u64, LocalCopyError> {
        if let Some(position) = self.position {
            return Ok(position);
        }
        let position = writer.stream_position().map_err(|error| {
            LocalCopyError::destination_write("seek in destination file", destination, error)
        })?;
        self.position = Some(position);
        Ok(position)
    }

    /// Advances the tracked position past `len` bytes the writer moved over.
    fn advance(&mut self, len: usize) {
        if let Some(position) = self.position.as_mut() {
            *position = position.saturating_add(len as u64);
        }
    }

    /// Flushes the pending zero run using upstream's seek-vs-punch rule.
    ///
    /// The run starts at the writer's position (data writes and hole-punches
    /// always leave it just past the last data). Seeks
    /// forward when that position is at or beyond the preallocated extent (the
    /// natural-hole case for a fresh file), otherwise punches a hole to
    /// deallocate the blocks that preallocation reserved.
    /// upstream: fileio.c:84-99 flush_sparse_hole()
    fn flush<W: SparseSink>(
        &mut self,
        writer: &mut W,
        destination: &Path,
    ) -> Result<(), LocalCopyError> {
        if self.pending_zero_run == 0 {
            return Ok(());
        }

        let start = self.position(writer, destination)?;

        if start >= self.preallocated_len {
            self.seek_over(writer, destination)?;
        } else {
            writer.punch_hole(destination, start, self.pending_zero_run)?;
        }

        self.position = Some(start.saturating_add(self.pending_zero_run));
        self.pending_zero_run = 0;
        Ok(())
    }

    /// Seeks forward over the pending zero run, creating a natural hole.
    fn seek_over<W: SparseSink>(
        &mut self,
        writer: &mut W,
        destination: &Path,
    ) -> Result<(), LocalCopyError> {
        let mut remaining = self.pending_zero_run;
        while remaining > 0 {
            let step = remaining.min(i64::MAX as u64);
            writer
                .seek(SeekFrom::Current(step as i64))
                .map_err(|error| {
                    LocalCopyError::destination_write(
                        "seek in destination file",
                        destination,
                        error,
                    )
                })?;
            remaining -= step;
        }
        Ok(())
    }

    /// Returns the pending zero run length.
    #[cfg(test)]
    pub(crate) const fn pending_zeros(&self) -> u64 {
        self.pending_zero_run
    }

    /// Finishes sparse writing without seeking over a trailing zero run.
    ///
    /// Returns the final logical file position (including any trailing hole);
    /// the caller's `set_len` makes that run a hole. A run inside the
    /// preallocated extent is still punched, since truncating cannot free it.
    /// upstream: fileio.c:47-66 sparse_end() - `do_ftruncate(f, size)`, no
    /// lseek; `do_punch_hole()` first when updating a basis.
    pub(crate) fn finish<W: SparseSink>(
        &mut self,
        writer: &mut W,
        destination: &Path,
    ) -> Result<u64, LocalCopyError> {
        // The logical end of file is the current position plus any trailing
        // pending zero run that becomes a hole.
        let position = self.position(writer, destination)?;
        let logical_end = position.saturating_add(self.pending_zero_run);
        if position < self.preallocated_len {
            self.flush(writer, destination)?;
        }
        self.pending_zero_run = 0;
        Ok(logical_end)
    }
}

/// How a run of non-zero data inside a sparse chunk reaches the destination.
///
/// upstream: fileio.c:121-128 `emit_sparse_span()` - the `use_seek` flag is set
/// when the bytes are already present at this offset (an in-place matched
/// block), so the span is seeked over instead of rewritten. The zero-run scan
/// is identical in both modes, and that is exactly what lets `--inplace
/// --sparse` punch an interior hole out of an otherwise-matching block.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum SpanWrite {
    /// `use_seek == 0`: write the bytes (`full_sparse_write`).
    Data,
    /// `use_seek == 1`: the bytes already sit at this offset, so advance past
    /// them (`do_lseek`).
    Seek,
}

/// Writes a chunk of data with sparse hole detection.
///
/// The chunk's leading and trailing zero runs, and every interior zero run of
/// at least [`SPARSE_WRITE_SIZE`] bytes, are accumulated rather than written,
/// and flushed as seeks (natural holes) or hole-punches (inside a preallocated
/// extent) when non-zero data follows. The data between two such runs goes out
/// in one write.
/// upstream: fileio.c:130-189 write_sparse()
pub(crate) fn write_sparse_chunk(
    writer: &mut fs::File,
    state: &mut SparseWriteState,
    chunk: &[u8],
    destination: &Path,
) -> Result<usize, LocalCopyError> {
    write_sparse_spans(writer, state, chunk, destination, SpanWrite::Data)
}

/// Consumes an in-place matched block through the sparse processor.
///
/// The block's bytes already sit at the writer's current offset, so its data
/// spans are seeked over rather than rewritten - but its interior zero runs are
/// still scanned and punched. Skipping that scan is what leaves a hole-y basis
/// file fully allocated under `--inplace --sparse`.
/// upstream: fileio.c:298-301 skip_matched() - `if (sparse_files > 0)
/// write_file(fd, 1 /*use_seek*/, offset, buf, len)`, only the non-sparse arm
/// lseeks straight past the block.
pub(crate) fn skip_matched_sparse(
    writer: &mut fs::File,
    state: &mut SparseWriteState,
    chunk: &[u8],
    destination: &Path,
) -> Result<usize, LocalCopyError> {
    write_sparse_spans(writer, state, chunk, destination, SpanWrite::Seek)
}

/// Runs one chunk through upstream's sparse scan and returns its full length,
/// which is consumed even when large sections become holes.
///
/// upstream: fileio.c:130-189 write_sparse() - the chunk's leading and trailing
/// zero runs are always deferred, an interior run only from `SPARSE_WRITE_SIZE`
/// bytes up, and each data span between them is emitted in one go.
pub(super) fn write_sparse_spans<W: SparseSink>(
    writer: &mut W,
    state: &mut SparseWriteState,
    chunk: &[u8],
    destination: &Path,
    mode: SpanWrite,
) -> Result<usize, LocalCopyError> {
    for span in sparse_spans(chunk, SPARSE_WRITE_SIZE) {
        let data = match span {
            SparseSpan::Zeros(len) => {
                state.accumulate(len);
                continue;
            }
            SparseSpan::Data(data) => data,
        };
        // upstream: fileio.c:123 emit_sparse_span() flushes the pending zero
        // run (seek or punch) before emitting the data span. The flush reads
        // the writer's current position as the run start.
        state.flush(writer, destination)?;
        match mode {
            SpanWrite::Data => {
                writer.write_all(&chunk[data.clone()]).map_err(|error| {
                    LocalCopyError::destination_write("copy file", destination, error)
                })?;
                state.advance(data.len());
                state
                    .touched
                    .record(state.logical_offset + data.start as u64, data.len() as u64);
            }
            // upstream: fileio.c:125-126 - the bytes are already at this
            // offset, so advance past them instead of rewriting them.
            SpanWrite::Seek => {
                writer
                    .seek(SeekFrom::Current(data.len() as i64))
                    .map_err(|error| {
                        LocalCopyError::destination_write(
                            "seek in destination file",
                            destination,
                            error,
                        )
                    })?;
                state.advance(data.len());
            }
        }
    }

    state.logical_offset = state.logical_offset.saturating_add(chunk.len() as u64);
    Ok(chunk.len())
}
