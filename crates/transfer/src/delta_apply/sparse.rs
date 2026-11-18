//! Sparse file write state tracker for hole optimization during delta apply.
//!
//! upstream: `fileio.c` `write_sparse()` and `sparse_end()`.

use std::io::{self, Seek, SeekFrom, Write};

use engine::BlockTouchTracker;
use fast_io::zero_detect::{SparseSpan, sparse_spans};

use crate::constants::SPARSE_WRITE_SIZE;

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
/// upstream: `fileio.c:88` `if (sparse_past_write >= preallocated_len)`.
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
    /// Running mirror of the writer's stream position, matching upstream
    /// `write_file()`'s caller-tracked `offset` / `sparse_past_write`. Primed
    /// once from the real position on first use, then advanced in step with
    /// every data write and hole seek so no per-zero-run `stream_position()`
    /// query (and its buffer flush + `lseek`) is issued.
    ///
    /// upstream: `fileio.c:186` `sparse_past_write = offset + len - l2`.
    stream_offset: u64,
    /// Whether [`Self::stream_offset`] has been primed from the writer's real
    /// position. Guards the single position query per file.
    offset_primed: bool,
    /// Distinct 4 KiB blocks written by data spans at their absolute offsets.
    /// Zero runs and [`Self::skip_matched`] spans are never credited.
    /// upstream: fileio.c:168-169,179-180 - `if (!use_seek)
    /// track_block_touches(f, offset + start, ...)`.
    touched: BlockTouchTracker,
}

impl SparseWriteState {
    /// Creates a new sparse write state.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            pending_zeros: 0,
            preallocated_len: 0,
            holes: Vec::new(),
            stream_offset: 0,
            offset_primed: false,
            touched: BlockTouchTracker::new(),
        }
    }

    /// Returns the number of distinct 4 KiB blocks the data spans wrote.
    #[must_use]
    pub const fn touched_blocks(&self) -> u64 {
        self.touched.total()
    }

    /// Returns the writer's current stream position, priming the tracked
    /// offset from the real position on the first call and returning the
    /// maintained value thereafter. This replaces the per-zero-run
    /// `stream_position()` query with at most one query per file.
    ///
    /// upstream: `write_file()` passes a caller-tracked `offset` rather than
    /// querying the OS position (`fileio.c:247`).
    #[inline]
    fn ensure_offset<W: Seek>(&mut self, writer: &mut W) -> io::Result<u64> {
        if !self.offset_primed {
            self.stream_offset = writer.stream_position()?;
            self.offset_primed = true;
        }
        Ok(self.stream_offset)
    }

    /// Records the destination's pre-existing length so zero runs that overlap
    /// stale basis data are punched rather than merely seeked over (which would
    /// leave the old bytes on disk in an `--inplace` update).
    ///
    /// upstream: `fileio.c:88` seek-vs-punch decision keyed on `preallocated_len`.
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
    /// upstream: `fileio.c:84-99` `flush_sparse_hole()`.
    #[inline]
    pub fn flush<W: Write + Seek>(&mut self, writer: &mut W) -> io::Result<()> {
        if self.pending_zeros == 0 {
            return Ok(());
        }

        let start = self.ensure_offset(writer)?;
        if start < self.preallocated_len {
            self.holes.push((start, self.pending_zeros));
        }

        let mut remaining = self.pending_zeros;
        while remaining > 0 {
            let step = remaining.min(i64::MAX as u64);
            writer.seek(SeekFrom::Current(step as i64))?;
            remaining -= step;
        }

        // Advance the tracked offset over the seeked hole so the next flush
        // needs no position query (upstream keeps `offset` running in sync).
        self.stream_offset = start.saturating_add(self.pending_zeros);
        self.pending_zeros = 0;
        Ok(())
    }

    /// Writes data with sparse optimization.
    ///
    /// The leading and trailing zero runs of `data`, and every interior zero
    /// run of at least `SPARSE_WRITE_SIZE` bytes, are deferred as holes; the
    /// data between two such runs is written in one `write()`.
    ///
    /// upstream: fileio.c:130-189 `write_sparse()`.
    #[inline]
    pub fn write<W: Write + Seek>(&mut self, writer: &mut W, data: &[u8]) -> io::Result<usize> {
        self.write_spans(writer, data, true)
    }

    /// Consumes an in-place matched block that already sits at its offset.
    ///
    /// The bytes take the same zero-run scan as [`Self::write`], so interior
    /// holes are still punched, but the data spans are not credited as touched
    /// blocks: upstream seeks over them instead of writing.
    ///
    /// upstream: fileio.c:298-300 skip_matched() -> `write_file(fd, 1 /*use_seek*/,
    /// ...)`, and fileio.c:168,179 skip block accounting when `use_seek`.
    pub fn skip_matched<W: Write + Seek>(
        &mut self,
        writer: &mut W,
        data: &[u8],
    ) -> io::Result<usize> {
        self.write_spans(writer, data, false)
    }

    fn write_spans<W: Write + Seek>(
        &mut self,
        writer: &mut W,
        data: &[u8],
        credit_touched: bool,
    ) -> io::Result<usize> {
        if data.is_empty() {
            return Ok(0);
        }

        // Prime the tracked offset before any write advances the position, so
        // the interior flush() calls never issue a position query.
        self.ensure_offset(writer)?;

        for span in sparse_spans(data, SPARSE_WRITE_SIZE) {
            match span {
                SparseSpan::Zeros(len) => self.accumulate(len),
                SparseSpan::Data(range) => {
                    self.flush(writer)?;
                    let chunk = &data[range];
                    writer.write_all(chunk)?;
                    if credit_touched {
                        self.touched.record(self.stream_offset, chunk.len() as u64);
                    }
                    self.stream_offset = self.stream_offset.saturating_add(chunk.len() as u64);
                }
            }
        }

        Ok(data.len())
    }

    /// Finalizes sparse writing and returns the file's logical end offset.
    ///
    /// A trailing zero run is not seeked over: the caller's `set_len` to the
    /// returned length makes it a hole. When the run overlaps the basis it is
    /// recorded for punching, since truncating cannot free those blocks.
    ///
    /// upstream: `fileio.c:47-66` `sparse_end()` - `do_ftruncate(f, size)` with
    /// no lseek, after `do_punch_hole()` when updating a basis.
    pub fn finish<W: Write + Seek>(&mut self, writer: &mut W) -> io::Result<u64> {
        let start = self.ensure_offset(writer)?;
        let logical_end = start.saturating_add(self.pending_zeros);
        if self.pending_zeros > 0 && start < self.preallocated_len {
            self.holes.push((start, self.pending_zeros));
        }
        self.pending_zeros = 0;
        Ok(logical_end)
    }
}

#[cfg(test)]
mod tests {
    use std::io::{self, Cursor, Seek, SeekFrom, Write};

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
        // a distinct write, so it is that write's leading run. A run inside one
        // write is deferred the same way once it reaches SPARSE_WRITE_SIZE;
        // shorter ones are written literally (as upstream) and overwrite the basis.
        // upstream: fileio.c:84-99 flush_sparse_hole().
        let mut state = SparseWriteState::new();
        state.set_preallocated_len(1000);
        let mut cursor = Cursor::new(vec![0xAAu8; 2000]);
        state.write(&mut cursor, &[1u8; 100]).unwrap(); // data -> offset 100
        state.write(&mut cursor, &[0u8; 300]).unwrap(); // zero run -> pending
        state.write(&mut cursor, &[9u8; 10]).unwrap(); // data forces the flush
        let holes = state.take_holes();
        assert_eq!(holes, vec![(100, 300)], "in-basis run recorded for punch");
    }

    /// A writer that records how many bytes are written versus seeked over, so a
    /// test can prove interior zero runs became holes instead of literal data.
    struct CountingWriter {
        inner: Cursor<Vec<u8>>,
        written: u64,
        seeked: u64,
    }

    impl Write for CountingWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.written += buf.len() as u64;
            self.inner.write(buf)
        }
        fn flush(&mut self) -> io::Result<()> {
            self.inner.flush()
        }
    }

    impl Seek for CountingWriter {
        fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
            if let SeekFrom::Current(delta) = pos {
                self.seeked += delta.unsigned_abs();
            }
            self.inner.seek(pos)
        }
    }

    #[test]
    fn sub_window_interior_zero_run_becomes_hole_not_literal() {
        // A 4 KB..8 KB..4 KB layout has an 8 KB interior zero run that is smaller
        // than the old 32 KB scan window. Upstream `write_file()` scans in
        // SPARSE_WRITE_SIZE (1 KB) pieces, so the interior run is seeked over
        // (punched) rather than written as literal data. With a 32 KB window the
        // whole 16 KB fell in one window and the interior zeros were written
        // literally, leaving them allocated on disk (issue #257). The write pass
        // must therefore write only the 8 KB of real data and seek the 8 KB hole.
        // upstream: fileio.c:144-177 write_sparse() defers interior zero runs of
        // at least SPARSE_WRITE_SIZE as holes.
        let mut state = SparseWriteState::new();
        let mut w = CountingWriter {
            inner: Cursor::new(Vec::new()),
            written: 0,
            seeked: 0,
        };
        let mut data = vec![0xAAu8; 4096];
        data.extend(std::iter::repeat_n(0u8, 8192));
        data.extend(std::iter::repeat_n(0xBBu8, 4096));
        state.write(&mut w, &data).unwrap();
        state.finish(&mut w).unwrap();
        assert_eq!(w.written, 8192, "only the two 4 KB data blocks are written");
        assert_eq!(
            w.seeked, 8192,
            "the 8 KB interior zero run is seeked (hole)"
        );
    }

    /// WHY: only written data spans are touched blocks; a zero run between them
    /// is a seeked hole and must not be credited (upstream TEST 5 expects 2 for
    /// `data + hole + data`). fileio.c:168-180.
    #[test]
    fn touched_blocks_credit_data_spans_not_holes() {
        let mut state = SparseWriteState::new();
        let mut cursor = Cursor::new(Vec::new());
        let mut data = vec![0xAAu8; 4096];
        data.extend(std::iter::repeat_n(0u8, 64 * 1024));
        data.extend(std::iter::repeat_n(0xBBu8, 4096));
        state.write(&mut cursor, &data).unwrap();
        state.finish(&mut cursor).unwrap();
        assert_eq!(state.touched_blocks(), 2);
    }

    /// WHY: an in-place matched block is seeked over upstream (`use_seek`), so
    /// feeding it through `skip_matched` must credit nothing, while the same
    /// bytes through `write` do. fileio.c:298-300.
    #[test]
    fn skip_matched_credits_no_touched_blocks() {
        let mut skipped = SparseWriteState::new();
        let mut cursor = Cursor::new(vec![0u8; 8192]);
        skipped.skip_matched(&mut cursor, &[7u8; 8192]).unwrap();
        assert_eq!(skipped.touched_blocks(), 0);

        let mut written = SparseWriteState::new();
        let mut cursor = Cursor::new(vec![0u8; 8192]);
        written.write(&mut cursor, &[7u8; 8192]).unwrap();
        assert_eq!(written.touched_blocks(), 2);
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

    /// A writer that distinguishes forward hole seeks from position queries so
    /// a test can prove the sparse writer tracks its offset in a variable
    /// instead of querying the OS position on every zero run.
    ///
    /// `stream_position()` and every no-op relative seek land as
    /// `SeekFrom::Current(0)` and increment `position_queries`; each punched
    /// hole lands as a `SeekFrom::Current(n > 0)` and increments `hole_seeks`.
    struct SeekAccountingWriter {
        inner: Cursor<Vec<u8>>,
        hole_seeks: u64,
        position_queries: u64,
    }

    impl Write for SeekAccountingWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.inner.write(buf)
        }
        fn flush(&mut self) -> io::Result<()> {
            self.inner.flush()
        }
    }

    impl Seek for SeekAccountingWriter {
        fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
            if let SeekFrom::Current(delta) = pos {
                if delta == 0 {
                    self.position_queries += 1;
                } else {
                    self.hole_seeks += 1;
                }
            }
            self.inner.seek(pos)
        }
    }

    #[test]
    fn offset_tracked_without_per_run_position_query() {
        // WHY: upstream fileio.c keeps the file offset in a running variable
        // (sparse_past_write) and issues exactly one lseek per zero run - it
        // never queries the OS position. This asserts the Rust port matches
        // that syscall profile: N distinct interior zero runs cost N forward
        // seeks, the trailing run none (sparse_end() ftruncates instead), and
        // at most one position query for the whole file, while the byte output
        // is identical to a plain (non-sparse) writer.
        // upstream: fileio.c:130-189 write_sparse().
        let mut reference = vec![0xAAu8; 2048];
        reference.extend(std::iter::repeat_n(0u8, 3072)); // hole 1
        reference.extend(std::iter::repeat_n(0xBBu8, 2048));
        reference.extend(std::iter::repeat_n(0u8, 5120)); // hole 2
        reference.extend(std::iter::repeat_n(0xCCu8, 2048));
        reference.extend(std::iter::repeat_n(0u8, 1024)); // trailing hole 3
        let total = reference.len() as u64;

        let mut state = SparseWriteState::new();
        let mut w = SeekAccountingWriter {
            inner: Cursor::new(Vec::new()),
            hole_seeks: 0,
            position_queries: 0,
        };
        state.write(&mut w, &reference).unwrap();
        let logical = state.finish(&mut w).unwrap();

        assert_eq!(logical, total, "logical size includes every hole");
        assert_eq!(
            w.hole_seeks, 2,
            "one forward seek per interior zero run, none for the trailing one"
        );
        assert!(
            w.position_queries <= 1,
            "offset is variable-tracked, not queried per zero run (got {})",
            w.position_queries
        );

        // Byte-identical guarantee: materialize the trailing hole via set_len
        // (as the applicator does) and compare to the plain-writer bytes.
        let mut produced = w.inner.into_inner();
        produced.resize(logical as usize, 0);
        assert_eq!(produced, reference, "sparse output is byte-identical");
    }

    /// A writer that records the length of every `write` call.
    #[derive(Default)]
    struct WriteLengths {
        inner: Cursor<Vec<u8>>,
        writes: Vec<usize>,
    }

    impl Write for WriteLengths {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.writes.push(buf.len());
            self.inner.write(buf)
        }

        fn flush(&mut self) -> io::Result<()> {
            self.inner.flush()
        }
    }

    impl Seek for WriteLengths {
        fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
            self.inner.seek(pos)
        }
    }

    /// WHY: data without a long zero run goes out in one write however large
    /// it is; the old scan issued one write per 1,024-byte window and relied
    /// on the caller's buffering to hide it.
    /// upstream: fileio.c:174-182 - the tail `emit_sparse_span()`.
    #[test]
    fn hole_free_data_is_one_write() {
        let data: Vec<u8> = (0..64 * 1024usize).map(|i| (i % 251) as u8 + 1).collect();
        let mut state = SparseWriteState::new();
        let mut w = WriteLengths::default();
        state.write(&mut w, &data).unwrap();
        assert_eq!(w.writes, [data.len()]);
        assert_eq!(w.inner.into_inner(), data);
    }

    /// WHY: hole placement must match upstream's rule, not a 1,024-byte grid.
    /// A 600-byte zero run straddling the 1 KiB mark used to be split into a
    /// trailing and a leading window run and seeked over; upstream keeps a run
    /// below `SPARSE_WRITE_SIZE` inside the write. A run of exactly
    /// `SPARSE_WRITE_SIZE` is a hole wherever it falls.
    /// upstream: fileio.c:163 - `if (z < SPARSE_WRITE_SIZE)`.
    #[test]
    fn interior_run_is_a_hole_only_from_the_threshold() {
        for (run, writes) in [(600usize, vec![2200]), (1024, vec![700, 900])] {
            let mut data = vec![7u8; 700];
            data.extend(std::iter::repeat_n(0u8, run));
            data.extend(std::iter::repeat_n(9u8, 900));
            let mut state = SparseWriteState::new();
            let mut w = WriteLengths::default();
            state.write(&mut w, &data).unwrap();
            let logical = state.finish(&mut w).unwrap();
            assert_eq!(w.writes, writes, "run of {run}");
            let mut produced = w.inner.into_inner();
            produced.resize(logical as usize, 0);
            assert_eq!(produced, data, "run of {run}");
        }
    }

    /// WHY: rsync 3.5.2 does not seek over a file's trailing zero run; the
    /// receiver's single `set_len` to the logical length makes it a hole. A
    /// seek there is one wasted `lseek` per file that ends in a hole.
    /// upstream: fileio.c:47-66 sparse_end() - `do_ftruncate(f, size)`.
    #[test]
    fn trailing_zero_run_is_left_to_set_len() {
        let mut data = vec![0x5Au8; 3000];
        data.extend(std::iter::repeat_n(0u8, 8192));
        let mut state = SparseWriteState::new();
        let mut w = SeekAccountingWriter {
            inner: Cursor::new(Vec::new()),
            hole_seeks: 0,
            position_queries: 0,
        };
        state.write(&mut w, &data).unwrap();
        let logical = state.finish(&mut w).unwrap();

        assert_eq!(w.hole_seeks, 0, "the trailing run is not seeked over");
        assert_eq!(logical, data.len() as u64, "set_len target covers the run");
        assert!(
            state.take_holes().is_empty(),
            "nothing to punch on a new file"
        );
        let mut produced = w.inner.into_inner();
        assert_eq!(produced.len(), 3000, "only the data was written");
        produced.resize(logical as usize, 0);
        assert_eq!(produced, data);
    }

    /// WHY: over an existing basis the trailing run still holds stale bytes,
    /// so it must be punched even though it is not seeked over.
    /// upstream: fileio.c:51-54 sparse_end() - `do_punch_hole()` when
    /// updating a basis, then `do_ftruncate()`.
    #[test]
    fn trailing_zero_run_over_the_basis_is_recorded_for_punching() {
        let mut data = vec![0x5Au8; 3000];
        data.extend(std::iter::repeat_n(0u8, 8192));
        let mut state = SparseWriteState::new();
        state.set_preallocated_len(data.len() as u64);
        let mut w = SeekAccountingWriter {
            inner: Cursor::new(Vec::new()),
            hole_seeks: 0,
            position_queries: 0,
        };
        state.write(&mut w, &data).unwrap();
        state.finish(&mut w).unwrap();

        assert_eq!(w.hole_seeks, 0);
        assert_eq!(state.take_holes(), [(3000, 8192)]);
    }
}
