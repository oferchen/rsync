//! Segment scheduling and incremental recursion state for the generator.
//!
//! Defines the per-directory sub-list types (`PendingSegment`, `DirSegment`,
//! `TaggedIndex`), the cursor-based `SegmentScheduler` that sizes the segment
//! lookahead window between `MIN_FILECNT_LOOKAHEAD` and
//! `MAX_FILECNT_LOOKAHEAD`, and the `IncrementalState` mutable state carried by
//! `GeneratorContext` for INC_RECURSE segmented file list sending.
//!
//! # Upstream Reference
//!
//! - `rsync.h:151-152` - `MIN_FILECNT_LOOKAHEAD` / `MAX_FILECNT_LOOKAHEAD`
//! - `flist.c:2396-2411` - `send_extra_file_list()` lookahead loop head
//! - `sender.c:515,549` - send loop tops the window up to the minimum
//! - `io.c:836-857` - `perform_io()` grows the window towards the maximum
//!   whenever the sender would otherwise idle

use super::ndx_map::NdxMap;

/// Entries the sender keeps queued in sub-lists beyond the one the receiver is
/// currently working through. Once the backlog reaches this many entries the
/// sender stops emitting sub-lists and waits for the receiver to catch up, so a
/// peer that throttles is never flooded with the whole tree up front.
///
/// # Upstream Reference
///
/// - `rsync.h:151` - `#define MIN_FILECNT_LOOKAHEAD 1000`
/// - `sender.c:515,549` - `send_extra_file_list(f_out, MIN_FILECNT_LOOKAHEAD)`
pub const MIN_FILECNT_LOOKAHEAD: usize = 1000;

/// Ceiling on the lookahead backlog the sender will grow to while it is idle.
///
/// [`MIN_FILECNT_LOOKAHEAD`] is a floor, not a target: upstream's send loop
/// only ever tops the backlog *up to* it. The window past that floor is grown
/// opportunistically, one sub-list at a time, at moments the sender would
/// otherwise sit blocked on receiver input - and this constant is what stops
/// that growth, so the sender never queues the whole remaining tree ahead of a
/// receiver that has not asked for it.
///
/// # Upstream Reference
///
/// - `rsync.h:152` - `#define MAX_FILECNT_LOOKAHEAD 10000`
/// - `io.c:836-843` - `perform_io()` keeps `extra_flist_sending_enabled` set,
///   and the poll timeout at zero, only while
///   `file_total - file_old_total < MAX_FILECNT_LOOKAHEAD`; at or above the
///   ceiling it clears the flag and reverts to the blocking poll timeout.
pub const MAX_FILECNT_LOOKAHEAD: usize = 10000;

/// A pending file list sub-segment for incremental recursion sending.
///
/// References entries in `GeneratorContext::file_list` by range rather than
/// storing cloned entries, avoiding double allocation.
///
/// # Upstream Reference
///
/// - `flist.c:2396` - `send_extra_file_list()` sends one directory's entries
///   as a sub-list
/// - `flist.c:2966` - `ndx_start = prev->ndx_start + prev->used + 1`
#[derive(Debug)]
pub(crate) struct PendingSegment {
    /// Wire `dir_ndx` of the owning directory in the receiver's `dir_flist`.
    ///
    /// Written to the wire as `NDX_FLIST_OFFSET - parent_dir_ndx`
    /// (flist.c:2152) to identify which directory this sub-list belongs to.
    pub(crate) parent_dir_ndx: i32,
    /// Flat `GeneratorContext::file_list` index of the owning directory entry.
    ///
    /// Distinct from `parent_dir_ndx`: the wire `dir_ndx` counts only
    /// directories in `dir_flist` growth order, whereas this indexes the flat
    /// entry list that drives itemize formatting. A directory itemized via its
    /// sub-list's gap NDX (`ndx_start - 1`) resolves to this flat index so the
    /// printed row carries the directory's own path and type char.
    pub(crate) parent_flat_idx: usize,
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
/// Sole owner of upstream's lookahead sizing rule: the `MIN_FILECNT_LOOKAHEAD`
/// floor, the `MAX_FILECNT_LOOKAHEAD` ceiling, and the
/// `file_total - file_old_total` backlog both are compared against. The
/// transfer loop drives it at the top of each iteration (upstream
/// `sender.c:515,549`), again at the point it is about to block for receiver
/// input (upstream `io.c:855`), and tells it when the receiver has finished a
/// sub-list - but never re-derives either bound itself.
///
/// # Lookahead accounting
///
/// Upstream's loop condition is `file_total - file_old_total < at_least`
/// (`flist.c:2411`), where `file_total` counts entries in every live file-list
/// and `file_old_total` counts entries in the lists from `first_flist` through
/// `cur_flist`. The difference is therefore the number of entries queued in
/// sub-lists *beyond the one the receiver is currently working through* - a
/// pure lookahead backlog. It is not "sent minus transferred": it steps by
/// whole sub-lists, at `flist_free()` time, never per transferred file.
///
/// [`Self::lookahead_total`] reproduces that difference directly: dispatching a
/// sub-list adds its entry count (`flist.c:2467` `file_total += flist->used`,
/// with `file_old_total` untouched), and retiring the current sub-list on the
/// receiver's `NDX_DONE` subtracts the count of the sub-list promoted in its
/// place (`sender.c:531-535`).
///
/// # Upstream Reference
///
/// - `sender.c:515,549` - checks `inc_recurse` at top/bottom of send loop
/// - `flist.c:2411` - `send_extra_file_list()` re-tests the lookahead
///   condition on every iteration
/// - `io.c:836-857` - the idle-time growth path and its ceiling
#[derive(Debug)]
pub(crate) struct SegmentScheduler {
    segments: Vec<PendingSegment>,
    /// Number of segments handed to the wire so far.
    cursor: usize,
    /// Number of segments the receiver has already worked through, i.e. the
    /// index of the segment playing upstream's `cur_flist` role. Always
    /// `<= cursor`.
    promoted: usize,
    /// Entries in `segments[promoted..cursor]`: upstream's
    /// `file_total - file_old_total`.
    lookahead_total: usize,
}

impl SegmentScheduler {
    /// Creates a scheduler that will yield segments in order.
    ///
    /// The backlog starts at zero because the initial file list is `cur_flist`:
    /// `send_file_list()` adds its `used` to both `file_total` and
    /// `file_old_total` (`flist.c:2545-2546`), leaving the difference at 0.
    pub(crate) fn new(segments: Vec<PendingSegment>) -> Self {
        Self {
            segments,
            cursor: 0,
            promoted: 0,
            lookahead_total: 0,
        }
    }

    /// Returns the next segment when the lookahead backlog is under
    /// `MIN_FILECNT_LOOKAHEAD`, accounting for the dispatch before returning.
    ///
    /// Because the backlog is updated here, driving this in a `while let` loop
    /// re-evaluates upstream's condition on every iteration exactly as
    /// `flist.c:2139` does, and the loop stops as soon as enough entries are
    /// queued ahead.
    pub(crate) fn next_to_send(&mut self) -> Option<&PendingSegment> {
        if self.lookahead_total >= MIN_FILECNT_LOOKAHEAD {
            return None;
        }
        self.advance()
    }

    /// Returns one further segment when the sender is about to block for
    /// receiver input and the backlog is still under
    /// [`MAX_FILECNT_LOOKAHEAD`], growing the window past the floor.
    ///
    /// Upstream reaches the same effect through its poll loop rather than a
    /// second call site: `perform_io()` drops the poll timeout to zero while
    /// the backlog is under the ceiling (`io.c:836-843`), and when that poll
    /// finds no input ready it calls `send_extra_file_list(sock_f_out, -1)`
    /// (`io.c:855`), whose `at_least` resolves to `backlog + 1` - exactly one
    /// more sub-list per idle turn. oc has no poll loop to hang that off, so
    /// the caller signals the same "about to block" moment directly and this
    /// method supplies the one-segment step and the ceiling.
    ///
    /// Like the floor gate - and like upstream's loop head - the bound is
    /// tested against the backlog *before* the segment is folded in, never
    /// against the segment's own count. A directory larger than the whole
    /// ceiling is therefore still admitted whole the moment the backlog is
    /// under it. Sub-lists are never split to fit: `send1extra()` emits one
    /// entire directory per iteration (`flist.c:2427`), and truncating a
    /// sub-list would change the wire framing the receiver reconstructs its
    /// `dir_flist` from.
    pub(crate) fn next_when_idle(&mut self) -> Option<&PendingSegment> {
        if self.lookahead_total >= MAX_FILECNT_LOOKAHEAD {
            return None;
        }
        self.advance()
    }

    /// Returns the next segment unconditionally, ignoring the lookahead.
    ///
    /// Used for the end-of-transfer flush, where every sub-list must reach the
    /// receiver before `NDX_FLIST_EOF` regardless of the backlog.
    pub(crate) fn next_forced(&mut self) -> Option<&PendingSegment> {
        self.advance()
    }

    /// Hands out `segments[cursor]` and folds its entry count into the backlog.
    fn advance(&mut self) -> Option<&PendingSegment> {
        let idx = self.cursor;
        let count = self.segments.get(idx)?.count;
        self.cursor = idx + 1;
        self.lookahead_total += count;
        Some(&self.segments[idx])
    }

    /// Records that the receiver has finished the sub-list it was working on,
    /// promoting the oldest queued segment to `cur_flist`.
    ///
    /// # Upstream Reference
    ///
    /// - `sender.c:531-535` - `file_old_total -= first_flist->used` followed by
    ///   `flist_free()` and `file_old_total = cur_flist->used`
    pub(crate) fn retire_current_flist(&mut self) {
        if self.promoted < self.cursor {
            self.lookahead_total -= self.segments[self.promoted].count;
            self.promoted += 1;
        }
    }

    /// Number of segments dispatched so far.
    pub(crate) fn dispatched_count(&self) -> usize {
        self.cursor
    }

    /// Entries queued in sub-lists beyond the receiver's current one.
    #[cfg(test)]
    pub(crate) fn lookahead_total(&self) -> usize {
        self.lookahead_total
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
    /// Wire-NDX to flat-index map, plus the reclaim cursor.
    ///
    /// With INC_RECURSE the sender emits segmented file lists with `+1` gaps
    /// between them (`flist.c:2966`); this resolves the NDX values the receiver
    /// sends back. Without INC_RECURSE it holds a single segment and every
    /// lookup is the identity.
    pub(crate) ndx_map: NdxMap,
}

impl IncrementalState {
    /// Creates initial state with `ndx_start` derived from INC_RECURSE negotiation.
    pub(crate) fn new(initial_ndx_start: i32) -> Self {
        Self {
            pending_segments: Vec::new(),
            flist_eof_sent: false,
            flist_writer_cache: None,
            initial_segment_count: None,
            ndx_map: NdxMap::new(initial_ndx_start),
        }
    }
}
