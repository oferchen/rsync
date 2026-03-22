//! Sparse write state machine for tracking pending zero runs.
//!
//! Maintains the offset of the current pending zero-byte region during a
//! sequential write pass, converting zero runs into seeks (holes) and
//! flushing them with hole-punch or trailing-zero finalization.
//!
//! // upstream: fileio.c - sparse write buffering logic

use std::fs;
use std::io::{Seek, SeekFrom, Write};
use std::path::Path;

use crate::local_copy::LocalCopyError;

use super::hole_punch::punch_hole;
use super::{SPARSE_WRITE_SIZE, leading_zero_run, trailing_zero_run};

/// Tracks pending zero runs during sparse file writing.
///
/// This struct accumulates consecutive zero bytes and flushes them either
/// by seeking (for new files) or by punching holes (for in-place updates).
#[derive(Default)]
pub(crate) struct SparseWriteState {
    pending_zero_run: u64,
    /// Position where the pending zero run starts (used for punch_hole path).
    #[cfg_attr(not(test), allow(dead_code))]
    zero_run_start_pos: u64,
}

impl SparseWriteState {
    pub(super) const fn accumulate(&mut self, additional: usize) {
        self.pending_zero_run = self.pending_zero_run.saturating_add(additional as u64);
    }

    /// Flushes pending zeros by seeking forward.
    ///
    /// This is the default strategy for new files where the filesystem
    /// automatically creates sparse regions when seeking past end of file.
    fn flush(&mut self, writer: &mut fs::File, destination: &Path) -> Result<(), LocalCopyError> {
        if self.pending_zero_run == 0 {
            return Ok(());
        }

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

        self.pending_zero_run = 0;
        Ok(())
    }

    /// Flushes pending zeros by punching a hole in the file.
    ///
    /// This is used for in-place updates where we need to deallocate
    /// disk blocks. Falls back to writing zeros if hole punching is
    /// not supported.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn flush_with_punch_hole(
        &mut self,
        writer: &mut fs::File,
        destination: &Path,
    ) -> Result<(), LocalCopyError> {
        if self.pending_zero_run == 0 {
            return Ok(());
        }

        let pos = self.zero_run_start_pos;
        let len = self.pending_zero_run;

        punch_hole(writer, destination, pos, len)?;

        self.pending_zero_run = 0;
        Ok(())
    }

    pub(super) const fn replace(&mut self, next_run: usize) {
        self.pending_zero_run = next_run as u64;
    }

    /// Updates the starting position for the next zero run.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn set_zero_run_start(&mut self, pos: u64) {
        self.zero_run_start_pos = pos;
    }

    /// Returns the pending zero run length.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) const fn pending_zeros(&self) -> u64 {
        self.pending_zero_run
    }

    /// Finishes sparse writing by flushing any remaining zeros via seeking.
    pub(crate) fn finish(
        &mut self,
        writer: &mut fs::File,
        destination: &Path,
    ) -> Result<u64, LocalCopyError> {
        self.flush(writer, destination)?;

        writer
            .stream_position()
            .map_err(|error| LocalCopyError::io("seek in destination file", destination, error))
    }

    /// Finishes sparse writing by punching holes for any remaining zeros.
    ///
    /// Use this variant when updating files in-place to deallocate disk
    /// blocks for zero regions.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn finish_with_punch_hole(
        &mut self,
        writer: &mut fs::File,
        destination: &Path,
    ) -> Result<u64, LocalCopyError> {
        self.flush_with_punch_hole(writer, destination)?;

        writer
            .stream_position()
            .map_err(|error| LocalCopyError::io("seek in destination file", destination, error))
    }
}

/// Writes a chunk of data with sparse hole detection.
///
/// Zero runs within the chunk are accumulated rather than written, and
/// flushed as seeks when non-zero data follows. This produces sparse
/// files when the filesystem supports it.
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
