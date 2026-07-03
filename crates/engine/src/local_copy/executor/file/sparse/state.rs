//! Sparse write state machine for tracking pending zero runs.
//!
//! Maintains the offset of the current pending zero-byte region during a
//! sequential write pass, converting zero runs into seeks (holes) and
//! flushing them with hole-punch or trailing-zero finalization.

// upstream: fileio.c:write_sparse() - sparse write buffering logic

use std::fs;
use std::io::{Seek, SeekFrom, Write};
use std::path::Path;

use crate::local_copy::LocalCopyError;

use super::hole_punch::punch_hole;
use super::{SPARSE_WRITE_SIZE, leading_zero_run, trailing_zero_run};

/// Tracks pending zero runs during sparse file writing.
///
/// Mirrors upstream `write_sparse()`: consecutive zero bytes are accumulated
/// and flushed as a single seek or hole-punch when non-zero data (or
/// finalization) follows. A zero run is punched into a hole when it starts
/// inside the file's preallocated extent (`start < preallocated_len`) and
/// seeked over otherwise, matching upstream's `sparse_past_write >=
/// preallocated_len` decision.
///
/// The start of a pending run is the writer's current stream position (the
/// point just past the last data write), so the state stays correct even when
/// the caller seeks the writer between chunks (the delta/inplace path).
#[derive(Default)]
pub(crate) struct SparseWriteState {
    /// Accumulated pending zero bytes (upstream `sparse_seek`).
    pending_zero_run: u64,
    /// Length of the destination's preallocated extent (upstream
    /// `preallocated_len`). Zero runs starting below this offset are punched
    /// into holes; runs starting at or beyond it are seeked over.
    preallocated_len: u64,
}

impl SparseWriteState {
    /// Records the preallocated length so zero runs inside the reserved extent
    /// are punched out rather than merely seeked over (which would leave the
    /// preallocated blocks allocated).
    // upstream: fileio.c:92 - if (sparse_past_write >= preallocated_len)
    pub(crate) const fn set_preallocated_len(&mut self, len: u64) {
        self.preallocated_len = len;
    }

    pub(super) const fn accumulate(&mut self, additional: usize) {
        self.pending_zero_run = self.pending_zero_run.saturating_add(additional as u64);
    }

    /// Flushes the pending zero run using upstream's seek-vs-punch rule.
    ///
    /// The run starts at the writer's current stream position (data writes and
    /// hole-punches always leave the position just past the last data). Seeks
    /// forward when that position is at or beyond the preallocated extent (the
    /// natural-hole case for a fresh file), otherwise punches a hole to
    /// deallocate the blocks that preallocation reserved.
    // upstream: fileio.c:90-99 write_sparse()
    fn flush(&mut self, writer: &mut fs::File, destination: &Path) -> Result<(), LocalCopyError> {
        if self.pending_zero_run == 0 {
            return Ok(());
        }

        let start = writer
            .stream_position()
            .map_err(|error| LocalCopyError::io("seek in destination file", destination, error))?;

        if start >= self.preallocated_len {
            self.seek_over(writer, destination)?;
        } else {
            punch_hole(writer, destination, start, self.pending_zero_run)?;
        }

        self.pending_zero_run = 0;
        Ok(())
    }

    /// Seeks forward over the pending zero run, creating a natural hole.
    fn seek_over(
        &mut self,
        writer: &mut fs::File,
        destination: &Path,
    ) -> Result<(), LocalCopyError> {
        let mut remaining = self.pending_zero_run;
        while remaining > 0 {
            let step = remaining.min(i64::MAX as u64);
            writer
                .seek(SeekFrom::Current(step as i64))
                .map_err(|error| {
                    LocalCopyError::io("seek in destination file", destination, error)
                })?;
            remaining -= step;
        }
        Ok(())
    }

    pub(super) const fn replace(&mut self, next_run: usize) {
        self.pending_zero_run = next_run as u64;
    }

    /// Returns the pending zero run length.
    #[cfg(test)]
    pub(crate) const fn pending_zeros(&self) -> u64 {
        self.pending_zero_run
    }

    /// Finishes sparse writing by flushing any remaining zeros.
    ///
    /// Returns the final logical file position (including any trailing hole)
    /// for the caller's `set_len`.
    // upstream: fileio.c:43 sparse_end()
    pub(crate) fn finish(
        &mut self,
        writer: &mut fs::File,
        destination: &Path,
    ) -> Result<u64, LocalCopyError> {
        // The logical end of file is the current position plus any trailing
        // pending zero run that becomes a hole.
        let position = writer
            .stream_position()
            .map_err(|error| LocalCopyError::io("seek in destination file", destination, error))?;
        let logical_end = position.saturating_add(self.pending_zero_run);
        self.flush(writer, destination)?;
        Ok(logical_end)
    }
}

/// Writes a chunk of data with sparse hole detection.
///
/// Zero runs within the chunk are accumulated rather than written, and flushed
/// as seeks (natural holes) or hole-punches (inside a preallocated extent) when
/// non-zero data follows. This produces sparse files when the filesystem
/// supports it.
// upstream: fileio.c:write_sparse()
pub(crate) fn write_sparse_chunk(
    writer: &mut fs::File,
    state: &mut SparseWriteState,
    chunk: &[u8],
    destination: &Path,
) -> Result<usize, LocalCopyError> {
    // Mirror rsync's write_sparse: always report the full chunk length as
    // consumed even when large sections become holes. Callers that track
    // literal bytes should account for sparseness separately.
    if chunk.is_empty() {
        return Ok(0);
    }

    let mut offset = 0usize;

    while offset < chunk.len() {
        let segment_end = (offset + SPARSE_WRITE_SIZE).min(chunk.len());
        let segment = &chunk[offset..segment_end];

        let leading = leading_zero_run(segment);
        state.accumulate(leading);

        if leading == segment.len() {
            offset = segment_end;
            continue;
        }

        let trailing = trailing_zero_run(&segment[leading..]);
        let data_start = offset + leading;
        let data_end = segment_end - trailing;

        if data_end > data_start {
            // upstream: flush the pending zero run (seek or punch) before the
            // data write. The flush reads the writer's current position as the
            // run start, so an interleaved external seek stays correct.
            state.flush(writer, destination)?;
            writer
                .write_all(&chunk[data_start..data_end])
                .map_err(|error| LocalCopyError::io("copy file", destination, error))?;
        }

        state.replace(trailing);
        offset = segment_end;
    }

    Ok(chunk.len())
}
