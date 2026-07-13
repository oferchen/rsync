//! Segment scheduling and incremental recursion state for the generator.
//!
//! Defines the per-directory sub-list types (`PendingSegment`, `DirSegment`,
//! `TaggedIndex`), the cursor-based `SegmentScheduler` that throttles segment
//! dispatch via `MIN_FILECNT_LOOKAHEAD`, and the `IncrementalState` mutable
//! state carried by `GeneratorContext` for INC_RECURSE segmented file list
//! sending.
//!
//! # Upstream Reference
//!
//! - `flist.c:46` - `#define MIN_FILECNT_LOOKAHEAD 1000`
//! - `flist.c:2498-2510` - `send_extra_file_list()` lookahead throttling
//! - `sender.c:227,261` - send loop calls into segment scheduling at top/bottom

/// Minimum file count lookahead before the sender emits the next incremental
/// sub-list. The sender accumulates at least this many unsent entries before
/// flushing a new segment to the receiver, amortizing per-segment overhead.
///
/// # Upstream Reference
///
/// - `flist.c:46` - `#define MIN_FILECNT_LOOKAHEAD 1000`
/// - `sender.c:send_files()` line 250 - `send_extra_file_list(f, MIN_FILECNT_LOOKAHEAD)`
pub const MIN_FILECNT_LOOKAHEAD: usize = 1000;

/// A pending file list sub-segment for incremental recursion sending.
///
/// References entries in `GeneratorContext::file_list` by range rather than
/// storing cloned entries, avoiding double allocation.
///
/// # Upstream Reference
///
/// - `flist.c:send_extra_file_list()` - sends one directory's entries as a sub-list
/// - `flist.c:2931` - `ndx_start = prev->ndx_start + prev->used + 1`
#[derive(Debug)]
pub(crate) struct PendingSegment {
    /// Global NDX of the parent directory.
    pub(crate) parent_dir_ndx: i32,
    /// Start index into `GeneratorContext::file_list`.
    pub(crate) flist_start: usize,
    /// Number of entries in this segment.
    pub(crate) count: usize,
}

/// A file list index tagged with an optional directory node ID.
///
/// During classification, directory entries are tagged with their internal
/// node ID so the reorder phase can assign wire `dir_ndx` values via dense
/// Vec lookup instead of name-based HashMap probes.
#[derive(Debug, Clone, Copy)]
pub(crate) struct TaggedIndex {
    /// Index into the original (pre-reorder) file list.
    pub(crate) file_idx: usize,
    /// For directory entries: the internal node ID for tree building.
    /// `None` for regular files and the "." root entry.
    pub(crate) node_id: Option<usize>,
}

/// Per-directory segment with tagged child entries for incremental recursion.
///
/// Groups children belonging to a single directory, along with the directory's
/// internal node ID. The final wire `dir_ndx` is computed during reordering
/// to match the upstream receiver's `dir_flist` growth order.
#[derive(Debug)]
pub(crate) struct DirSegment {
    /// Internal node ID for tree building (NOT the wire dir_ndx).
    pub(crate) node_id: usize,
    /// Tagged entries belonging to this directory.
    pub(crate) children: Vec<TaggedIndex>,
}

/// Cursor-based scheduler that yields pending segments on demand.
///
/// Controls *when* sub-lists are sent during the transfer loop using
/// upstream's `MIN_FILECNT_LOOKAHEAD` throttling heuristic. The transfer
/// loop calls `next_if_needed()` at top and bottom of each iteration,
/// matching upstream `sender.c:227,261`.
///
/// # Upstream Reference
///
/// - `sender.c:227,261` - checks `inc_recurse` at top/bottom of send loop
/// - `flist.c:2498` - `send_extra_file_list()` uses `MIN_FILECNT_LOOKAHEAD`
#[derive(Debug)]
pub(crate) struct SegmentScheduler {
    segments: Vec<PendingSegment>,
    cursor: usize,
}

impl SegmentScheduler {
    /// Creates a scheduler that will yield segments in order.
    pub(crate) fn new(segments: Vec<PendingSegment>) -> Self {
        Self {
            segments,
            cursor: 0,
        }
    }

    /// Returns the next segment if the lookahead heuristic indicates we should send.
    ///
    /// Yields when `remaining_in_current` drops below `MIN_FILECNT_LOOKAHEAD`,
    /// matching upstream's throttling in `flist.c:2498-2510`.
    pub(crate) fn next_if_needed(
        &mut self,
        remaining_in_current: usize,
    ) -> Option<&PendingSegment> {
        if self.cursor >= self.segments.len() {
            return None;
        }
        if remaining_in_current < MIN_FILECNT_LOOKAHEAD {
            let seg = &self.segments[self.cursor];
            self.cursor += 1;
            Some(seg)
        } else {
            None
        }
    }

    /// Returns a slice of all remaining unconsumed segments.
    pub(crate) fn remaining(&self) -> &[PendingSegment] {
        &self.segments[self.cursor..]
    }

    /// Returns `true` when all segments have been dispatched.
    pub(crate) fn is_exhausted(&self) -> bool {
        self.cursor >= self.segments.len()
    }
}

/// Mutable state for INC_RECURSE segmented file list sending.
///
/// # Upstream Reference
///
/// - `flist.c:2534-2545` - INC_RECURSE sub-list and NDX_FLIST_EOF dispatch
/// - `flist.c:send_file_entry()` - static variables cached via `flist_writer_cache`
#[derive(Debug)]
pub(crate) struct IncrementalState {
    /// Pending file list segments for incremental recursion (INC_RECURSE).
    ///
    /// When INC_RECURSE is negotiated, the initial `send_file_list()` sends
    /// only top-level entries. Remaining per-directory segments are queued here
    /// and consumed by `SegmentScheduler` during the transfer loop.
    pub(crate) pending_segments: Vec<PendingSegment>,
    /// Whether all incremental file list segments have been sent.
    pub(crate) flist_eof_sent: bool,
    /// Cached file list writer for compression state continuity across sub-lists.
    ///
    /// Upstream rsync uses `static` variables in `send_file_entry()` that persist
    /// across `send_file_list()` calls. This field preserves the same state
    /// (prev_name, prev_mode, prev_uid, prev_gid) between `send_file_list()`
    /// and `encode_and_send_segment()`.
    pub(crate) flist_writer_cache: Option<protocol::flist::FileListWriter>,
    /// Number of entries in the initial segment when INC_RECURSE is active.
    ///
    /// When set, `send_file_list()` only sends the first `initial_segment_count`
    /// entries. The remaining entries are sent via the segment scheduler.
    pub(crate) initial_segment_count: Option<usize>,
    /// Segment boundary table for mapping wire NDX values to flat array indices.
    ///
    /// With INC_RECURSE, the sender sends segmented file lists with +1 gaps
    /// between segments (upstream `flist.c:2931`). When the receiver sends
    /// wire NDX values back, this table maps them to flat array indices.
    /// Each entry is `(flat_start, ndx_start)`.
    ///
    /// Without INC_RECURSE, this contains a single entry `(0, 0)`.
    pub(crate) ndx_segments: Vec<(usize, i32)>,
    /// Wire NDX of each segment's parent directory, aligned 1:1 with
    /// `ndx_segments`. The initial segment has no parent and stores `-1`.
    ///
    /// Under INC_RECURSE the remote generator itemizes a directory by sending
    /// the "gap NDX" `ndx_start - 1` of that directory's sub-list
    /// (generator.c:2313 `ndx = cur_flist->ndx_start - 1`). The sender must map
    /// that gap back to the parent directory entry rather than to a regular
    /// file, mirroring `dir_flist->files[cur_flist->parent_ndx]`
    /// (sender.c:269-272). This table records the parent wire NDX for each
    /// sub-list so `resolve_itemize_ndx` can perform that lookup.
    pub(crate) segment_parent_ndx: Vec<i32>,
    /// Index into `ndx_segments` of the oldest unreclaimed segment.
    ///
    /// Advances by one each time a completed segment is reclaimed via
    /// `reclaim_oldest_segment()`. Mirrors upstream's `first_flist`
    /// pointer which advances through the circular list as segments
    /// are freed by `flist_free()`.
    ///
    /// # Upstream Reference
    ///
    /// - `flist.c:101` - `first_flist` pointer
    /// - `sender.c:244` - `flist_free(first_flist)` advances `first_flist`
    pub(crate) first_segment_idx: usize,
}

impl IncrementalState {
    /// Creates initial state with `ndx_start` derived from INC_RECURSE negotiation.
    pub(crate) fn new(initial_ndx_start: i32) -> Self {
        Self {
            pending_segments: Vec::new(),
            flist_eof_sent: false,
            flist_writer_cache: None,
            initial_segment_count: None,
            ndx_segments: vec![(0, initial_ndx_start)],
            segment_parent_ndx: vec![-1],
            first_segment_idx: 0,
        }
    }
}
