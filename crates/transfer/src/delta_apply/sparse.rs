//! Sparse file write state tracker.
//!
//! Tracks pending runs of zeros that should become holes in the output file
//! rather than being written as data. Mirrors upstream rsync's `write_sparse()`
//! behavior in `fileio.c`.

use std::io::{self, Seek, SeekFrom, Write};

use crate::constants::{CHUNK_SIZE, leading_zero_count, trailing_zero_count};

/// State tracker for sparse file writing.
///
/// Tracks pending runs of zeros that should become holes in the output file
/// rather than being written as data. Mirrors upstream rsync's `write_sparse()`
/// behavior in `fileio.c`.
#[derive(Debug, Default)]
pub struct SparseWriteState {
    pending_zeros: u64,
}

impl SparseWriteState {
    /// Creates a new sparse write state.
    #[must_use]
    pub const fn new() -> Self {
        Self { pending_zeros: 0 }
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

    /// Flushes pending zeros by seeking forward, creating a hole.
    #[inline]
    pub fn flush<W: Write + Seek>(&mut self, writer: &mut W) -> io::Result<()> {
        if self.pending_zeros == 0 {
            return Ok(());
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

    /// Finalizes sparse writing and returns final position.
    pub fn finish<W: Write + Seek>(&mut self, writer: &mut W) -> io::Result<u64> {
        if self.pending_zeros > 0 {
            let skip = self.pending_zeros.saturating_sub(1);
            if skip > 0 {
                let mut remaining = skip;
                while remaining > 0 {
                    let step = remaining.min(i64::MAX as u64);
                    writer.seek(SeekFrom::Current(step as i64))?;
                    remaining -= step;
                }
            }
            writer.write_all(&[0])?;
            self.pending_zeros = 0;
        }
        writer.stream_position()
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
    fn sparse_state_finish() {
        let mut state = SparseWriteState::new();
        state.accumulate(10);
        let mut cursor = Cursor::new(vec![0u8; 100]);
        assert_eq!(state.finish(&mut cursor).unwrap(), 10);
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
        // Write data with leading zeros, non-zero content, trailing zeros
        let data = [0, 0, 0, 1, 2, 3, 0, 0];
        let result = state.write(&mut cursor, &data);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 8);
    }

    #[test]
    fn sparse_state_accumulate_overflow_protection() {
        let mut state = SparseWriteState::new();
        // accumulate uses saturating_add to prevent overflow
        state.accumulate(usize::MAX);
        state.accumulate(1);
        // Should not overflow
        assert!(state.pending() > 0);
    }

    #[test]
    fn sparse_state_finish_with_no_pending() {
        let mut state = SparseWriteState::new();
        let mut cursor = Cursor::new(vec![0u8; 100]);
        let pos = state.finish(&mut cursor).unwrap();
        assert_eq!(pos, 0); // No movement when no pending zeros
    }

    #[test]
    fn sparse_state_large_pending_zeros() {
        let mut state = SparseWriteState::new();
        state.accumulate(1000);
        let mut cursor = Cursor::new(vec![0u8; 2000]);
        // finish should seek to position 999, write one byte
        let pos = state.finish(&mut cursor).unwrap();
        assert_eq!(pos, 1000);
    }
}
