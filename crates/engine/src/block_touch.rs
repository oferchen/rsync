//! Receiver-side accounting of distinct 4 KiB logical blocks written.
//!
//! Protocol 33 reports this figure as the `--stats` line
//! `Number of 4 KiB logical blocks touched`. Counting is unconditional; only
//! the report is gated on the negotiated protocol.
//!
//! # Upstream Reference
//!
//! - `fileio.c:212-216` `reset_block_tracker()` - per-file reset, called from
//!   `receiver.c:489` at the top of `receive_data()`.
//! - `fileio.c:218-243` `track_block_touches()` - the forward high-water mark.
//! - `fileio.c:169,180` - sparse data spans are counted only when not seeking.
//! - `fileio.c:251-252` - dense writes are counted only when not seeking.

/// Size of one logical block in bytes. upstream: fileio.c:228 `offset / 4096`.
pub const TOUCHED_BLOCK_SIZE: u64 = 4096;

/// Largest count upstream can report: `stats.touched_blocks_4k` is an `int64`
/// that saturates at `INT64_MAX` (fileio.c:236-237).
const TOUCHED_BLOCKS_MAX: u64 = i64::MAX as u64;

/// Counts the distinct 4 KiB logical blocks a receiver writes.
///
/// Only forward progress is counted: a write is credited with the blocks
/// beyond the highest block already counted for the current file. Seeked-over
/// sparse holes and `--inplace` blocks that are skipped because they already
/// sit at their offset are never passed in, so they never count.
///
/// The running total spans every file; [`Self::start_file`] resets only the
/// per-file high-water mark.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct BlockTouchTracker {
    /// Highest block index counted for the current file. `None` is upstream's
    /// `last_touched_blk = -1`.
    last_touched_blk: Option<u64>,
    /// Blocks counted so far, saturating at `INT64_MAX`.
    total: u64,
}

impl BlockTouchTracker {
    /// Creates a tracker with no blocks counted.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            last_touched_blk: None,
            total: 0,
        }
    }

    /// Returns the number of blocks a single write of `len` bytes at `offset`
    /// spans, as counted for a file with nothing written yet.
    ///
    /// A whole-file copy that lands in one pass (clone, reflink, kernel copy)
    /// is credited with this, matching upstream writing the same bytes through
    /// `write_file()`.
    #[must_use]
    pub const fn blocks_spanned(offset: u64, len: u64) -> u64 {
        if len == 0 {
            return 0;
        }
        let start_blk = offset / TOUCHED_BLOCK_SIZE;
        let end_blk = start_blk + ((offset % TOUCHED_BLOCK_SIZE) + len - 1) / TOUCHED_BLOCK_SIZE;
        end_blk - start_blk + 1
    }

    /// Resets the per-file high-water mark before a new file is written.
    ///
    /// upstream: fileio.c:212-216 `reset_block_tracker()`, called at
    /// receiver.c:489 so a recycled descriptor cannot suppress the next file's
    /// blocks.
    pub const fn start_file(&mut self) {
        self.last_touched_blk = None;
    }

    /// Credits a write of `len` bytes at absolute file `offset`.
    ///
    /// upstream: fileio.c:218-243 `track_block_touches()`.
    pub fn record(&mut self, offset: u64, len: u64) {
        if len == 0 {
            return;
        }
        let start_blk = offset / TOUCHED_BLOCK_SIZE;
        let end_blk = start_blk + ((offset % TOUCHED_BLOCK_SIZE) + len - 1) / TOUCHED_BLOCK_SIZE;
        let added = match self.last_touched_blk {
            Some(last) if start_blk <= last => end_blk.saturating_sub(last),
            _ => end_blk - start_blk + 1,
        };
        self.total = self.total.saturating_add(added).min(TOUCHED_BLOCKS_MAX);
        if self.last_touched_blk.is_none_or(|last| end_blk > last) {
            self.last_touched_blk = Some(end_blk);
        }
    }

    /// Returns the number of blocks counted across every file so far.
    #[must_use]
    pub const fn total(&self) -> u64 {
        self.total
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// WHY: a sub-block write still dirties one whole 4 KiB block; upstream's
    /// write-touched-blocks TEST 1 zeroes 3000 bytes and expects exactly 1.
    #[test]
    fn sub_block_write_touches_one_block() {
        let mut t = BlockTouchTracker::new();
        t.record(0, 3000);
        assert_eq!(t.total(), 1);
    }

    /// WHY: a write straddling a block boundary dirties both blocks.
    #[test]
    fn straddling_write_touches_both_blocks() {
        let mut t = BlockTouchTracker::new();
        t.record(4000, 200);
        assert_eq!(t.total(), 2);
    }

    /// WHY: upstream TEST 2 flips one byte in each of 10 separate blocks; each
    /// literal lands in its own block, so the count is 10, not 1.
    #[test]
    fn scattered_writes_count_each_block() {
        let mut t = BlockTouchTracker::new();
        for i in 1..=10u64 {
            t.record(i * 4096, 1);
        }
        assert_eq!(t.total(), 10);
    }

    /// WHY: upstream TEST 4 rewrites a 4 MiB file; 1,024 blocks, and chunking
    /// the stream into many writes must not double count shared blocks.
    #[test]
    fn contiguous_chunked_stream_counts_each_block_once() {
        let mut t = BlockTouchTracker::new();
        let mut offset = 0u64;
        // 700-byte chunks never align with 4 KiB, so most writes share a block
        // with their predecessor - the high-water mark must absorb that.
        while offset < 4 * 1024 * 1024 {
            let len = 700.min(4 * 1024 * 1024 - offset);
            t.record(offset, len);
            offset += len;
        }
        assert_eq!(t.total(), 1024);
    }

    /// WHY: upstream TEST 5 writes one block, seeks a 4 MiB hole, writes one
    /// block. The hole is never passed in, so only the two data blocks count.
    #[test]
    fn seeked_hole_is_not_counted() {
        let mut t = BlockTouchTracker::new();
        t.record(0, 4096);
        t.record(4096 + 4 * 1024 * 1024, 4096);
        assert_eq!(t.total(), 2);
    }

    /// WHY: upstream TEST 6 writes two 4 KiB files in one run. Without the
    /// per-file reset the second file's block 0 is below the first file's
    /// high-water mark and would be dropped, reporting 1 instead of 2.
    #[test]
    fn per_file_reset_counts_each_files_first_block() {
        let mut t = BlockTouchTracker::new();
        t.start_file();
        t.record(0, 4096);
        t.start_file();
        t.record(0, 4096);
        assert_eq!(t.total(), 2);
    }

    /// WHY: the reset is load-bearing - without it the same two writes collapse
    /// to one block. This pins the failure mode the reset exists to prevent.
    #[test]
    fn missing_reset_undercounts_second_file() {
        let mut t = BlockTouchTracker::new();
        t.record(0, 4096);
        t.record(0, 4096);
        assert_eq!(t.total(), 1);
    }

    /// WHY: upstream TEST 3 (identical files) writes nothing; zero-length
    /// writes must not count a block.
    #[test]
    fn empty_write_counts_nothing() {
        let mut t = BlockTouchTracker::new();
        t.record(12345, 0);
        assert_eq!(t.total(), 0);
        assert_eq!(BlockTouchTracker::blocks_spanned(0, 0), 0);
    }

    /// WHY: `blocks_spanned` credits one-pass whole-file copies; it must agree
    /// with streaming the same bytes through `record`.
    #[test]
    fn blocks_spanned_matches_record() {
        for (offset, len) in [(0, 1), (0, 4096), (0, 4097), (4095, 2), (20000, 20000)] {
            let mut t = BlockTouchTracker::new();
            t.record(offset, len);
            assert_eq!(BlockTouchTracker::blocks_spanned(offset, len), t.total());
        }
    }

    /// WHY: upstream saturates the int64 counter at INT64_MAX (fileio.c:236).
    #[test]
    fn total_saturates_at_int64_max() {
        let mut t = BlockTouchTracker {
            last_touched_blk: None,
            total: TOUCHED_BLOCKS_MAX - 1,
        };
        t.record(0, 3 * TOUCHED_BLOCK_SIZE);
        assert_eq!(t.total(), TOUCHED_BLOCKS_MAX);
    }
}
