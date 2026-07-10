//! Sparse file write state tracker for hole optimization during delta apply.
//!
//! upstream: `fileio.c` `write_sparse()` and `sparse_end()`.

use std::io::{self, Seek, SeekFrom, Write};

use crate::constants::{CHUNK_SIZE, leading_zero_count, trailing_zero_count};

/// Tracks pending runs of zeros so they become holes in the output file rather
/// than being written as data.
///
/// Mirrors upstream rsync's `write_sparse()` / `sparse_end()` in `fileio.c`.
/// A zero run is always seeked over (advancing the writer to leave a natural
/// hole); when the run starts inside the destination's pre-existing extent
/// (`start < preallocated_len`, the `--inplace` basis case) its byte range is
/// additionally recorded so the caller can punch it, deallocating the stale
/// basis blocks and reading them back as zeros. Runs starting at or beyond
/// `preallocated_len` (fresh temp file, or bytes past the old EOF) need no
/// punch because a seek over never-written space already reads as zeros.
///
/// upstream: `fileio.c:92` `if (sparse_past_write >= preallocated_len)`.
#[derive(Debug, Default)]
pub struct SparseWriteState {
    /// Accumulated pending zero bytes (upstream `sparse_seek`).
    pending_zeros: u64,
    /// Length of the destination's pre-existing extent (upstream
    /// `preallocated_len`). Zero runs starting below this offset carry stale
    /// basis data and are recorded for hole-punching.
    preallocated_len: u64,
    /// Absolute `(start, len)` ranges to punch after the write pass, in file
    /// order. Only populated for in-place updates over an existing basis.
    holes: Vec<(u64, u64)>,
}

impl SparseWriteState {
    /// Creates a new sparse write state.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            pending_zeros: 0,
            preallocated_len: 0,
            holes: Vec::new(),
        }
    }

    /// Records the destination's pre-existing length so zero runs that overlap
    /// stale basis data are punched rather than merely seeked over (which would
    /// leave the old bytes on disk in an `--inplace` update).
    ///
    /// upstream: `fileio.c:92` seek-vs-punch decision keyed on `preallocated_len`.
    pub const fn set_preallocated_len(&mut self, len: u64) {
        self.preallocated_len = len;
    }

    /// Adds zero bytes to the pending run.
    #[inline]
    pub const fn accumulate(&mut self, count: usize) {
        self.pending_zeros = self.pending_zeros.saturating_add(count as u64);
    }

    /// Returns the number of pending zero bytes.
    #[must_use]
    pub const fn pending(&self) -> u64 {
        self.pending_zeros
    }

    /// Takes the recorded hole ranges to punch, leaving the state empty.
    ///
    /// The caller punches each `(start, len)` on the raw destination file after
    /// establishing the final length with `set_len`.
    #[must_use]
    pub fn take_holes(&mut self) -> Vec<(u64, u64)> {
        std::mem::take(&mut self.holes)
    }

    /// Flushes the pending zero run: seeks forward to leave a hole and records
    /// the range for punching when it overlaps the pre-existing basis extent.
    ///
    /// upstream: `fileio.c:90-99` `write_sparse()`.
    #[inline]
    pub fn flush<W: Write + Seek>(&mut self, writer: &mut W) -> io::Result<()> {
        if self.pending_zeros == 0 {
            return Ok(());
        }

        let start = writer.stream_position()?;
        if start < self.preallocated_len {
            self.holes.push((start, self.pending_zeros));
        }

        let mut remaining = self.pending_zeros;
        while remaining > 0 {
            let step = remaining.min(i64::MAX as u64);
            writer.seek(SeekFrom::Current(step as i64))?;
            remaining -= step;
        }

        self.pending_zeros = 0;
        Ok(())
    }

    /// Writes data with sparse optimization.
    ///
    /// Zero runs are tracked and become holes; non-zero data is written normally.
    /// Uses 32KB chunks matching upstream rsync's CHUNK_SIZE for efficient processing.
    #[inline]
    pub fn write<W: Write + Seek>(&mut self, writer: &mut W, data: &[u8]) -> io::Result<usize> {
        if data.is_empty() {
            return Ok(0);
        }

        let mut offset = 0;

        while offset < data.len() {
            let end = (offset + CHUNK_SIZE).min(data.len());
            let chunk = &data[offset..end];

            let leading_zeros = leading_zero_count(chunk);
            self.accumulate(leading_zeros);

            if leading_zeros == chunk.len() {
                offset = end;
                continue;
            }

            let tail = &chunk[leading_zeros..];
            let trailing_zeros = trailing_zero_count(tail);
            let data_start = offset + leading_zeros;
            let data_end = end - trailing_zeros;

            if data_end > data_start {
                self.flush(writer)?;
                writer.write_all(&data[data_start..data_end])?;
            }

            self.pending_zeros = trailing_zeros as u64;
            offset = end;
        }

        Ok(data.len())
    }

    /// Finalizes sparse writing and returns the file's logical end offset.
    ///
    /// Any trailing zero run becomes a hole (seeked over, and recorded for
    /// punching when it overlaps the basis). Unlike a plain writer this does
    /// NOT materialize the final byte; the caller establishes the logical size
    /// with `set_len(returned_len)`, leaving the trailing region a true hole.
    ///
    /// upstream: `fileio.c:43` `sparse_end()` -> `do_ftruncate(f, size)`.
    pub fn finish<W: Write + Seek>(&mut self, writer: &mut W) -> io::Result<u64> {
        let position = writer.stream_position()?;
        let logical_end = position.saturating_add(self.pending_zeros);
        self.flush(writer)?;
        Ok(logical_end)
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    #[test]
    fn sparse_state_new() {
        let state = SparseWriteState::new();
        assert_eq!(state.pending(), 0);
    }

    #[test]
    fn sparse_state_accumulate() {
        let mut state = SparseWriteState::new();
        state.accumulate(100);
        assert_eq!(state.pending(), 100);
        state.accumulate(50);
        assert_eq!(state.pending(), 150);
    }

    #[test]
    fn sparse_state_flush_empty() {
        let mut state = SparseWriteState::new();
        let mut cursor = Cursor::new(Vec::new());
        state.flush(&mut cursor).unwrap();
        assert_eq!(cursor.position(), 0);
    }

    #[test]
    fn sparse_state_flush_with_pending() {
        let mut state = SparseWriteState::new();
        state.accumulate(100);
        let mut cursor = Cursor::new(vec![0u8; 200]);
        state.flush(&mut cursor).unwrap();
        assert_eq!(cursor.position(), 100);
        assert_eq!(state.pending(), 0);
    }

    #[test]
    fn sparse_state_write_non_zero() {
        let mut state = SparseWriteState::new();
        let mut cursor = Cursor::new(Vec::new());
        assert_eq!(state.write(&mut cursor, b"hello").unwrap(), 5);
    }

    #[test]
    fn sparse_state_write_zeros_accumulates() {
        let mut state = SparseWriteState::new();
        let mut cursor = Cursor::new(vec![0u8; 100]);
        assert_eq!(state.write(&mut cursor, &[0u8; 50]).unwrap(), 50);
        assert!(state.pending() > 0);
    }

    #[test]
    fn sparse_state_finish_returns_logical_end_without_writing_byte() {
        // A file that is data followed by a trailing zero run: finish must
        // report the logical size (incl. the hole). upstream sparse_end()
        // ftruncate leaves the tail unallocated; the caller feeds the returned
        // length to set_len.
        let mut state = SparseWriteState::new();
        let mut cursor = Cursor::new(Vec::new());
        state.write(&mut cursor, b"abc").unwrap();
        state.accumulate(97);
        let logical = state.finish(&mut cursor).unwrap();
        assert_eq!(logical, 100, "logical size includes trailing hole");
    }

    #[test]
    fn sparse_state_write_empty_data() {
        let mut state = SparseWriteState::new();
        let mut cursor = Cursor::new(Vec::new());
        let result = state.write(&mut cursor, &[]);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 0);
    }

    #[test]
    fn sparse_state_write_mixed_data() {
        let mut state = SparseWriteState::new();
        let mut cursor = Cursor::new(vec![0u8; 200]);
        let data = [0, 0, 0, 1, 2, 3, 0, 0];
        let result = state.write(&mut cursor, &data);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 8);
    }

    #[test]
    fn sparse_state_accumulate_overflow_protection() {
        let mut state = SparseWriteState::new();
        state.accumulate(usize::MAX);
        state.accumulate(1);
        assert!(state.pending() > 0);
    }

    #[test]
    fn sparse_state_finish_with_no_pending() {
        let mut state = SparseWriteState::new();
        let mut cursor = Cursor::new(vec![0u8; 100]);
        let pos = state.finish(&mut cursor).unwrap();
        assert_eq!(pos, 0);
    }

    #[test]
    fn in_basis_zero_run_is_recorded_as_hole() {
        // A zero run that is seeked over (accumulated as pending, then flushed
        // by a following data write) starting inside the preallocated basis
        // extent is recorded for punching, so an --inplace update deallocates
        // stale basis blocks instead of leaving them on disk. The run is fed as
        // a distinct write, mirroring receive_data streaming CHUNK_SIZE pieces;
        // interior zeros within one write are written literally (as upstream)
        // and correctly overwrite the basis, so only seeked runs are punched.
        // upstream: fileio.c:90-99 write_sparse().
        let mut state = SparseWriteState::new();
        state.set_preallocated_len(1000);
        let mut cursor = Cursor::new(vec![0xAAu8; 2000]);
        state.write(&mut cursor, &[1u8; 100]).unwrap(); // data -> offset 100
        state.write(&mut cursor, &[0u8; 300]).unwrap(); // zero run -> pending
        state.write(&mut cursor, &[9u8; 10]).unwrap(); // data forces the flush
        let holes = state.take_holes();
        assert_eq!(holes, vec![(100, 300)], "in-basis run recorded for punch");
    }

    #[test]
    fn zero_run_beyond_basis_is_not_recorded() {
        let mut state = SparseWriteState::new();
        state.set_preallocated_len(50);
        let mut cursor = Cursor::new(vec![0u8; 2000]);
        state.write(&mut cursor, &[1u8; 100]).unwrap(); // data ends at 100 (> basis 50)
        state.write(&mut cursor, &[0u8; 300]).unwrap(); // zero run starts at 100
        state.write(&mut cursor, &[9u8; 10]).unwrap(); // data forces the flush
        assert!(
            state.take_holes().is_empty(),
            "run past basis extent needs no punch"
        );
    }
}
