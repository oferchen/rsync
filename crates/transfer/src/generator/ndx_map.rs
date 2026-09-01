//! The wire-NDX to flat-index map: oc's analogue of upstream's `flist_for_ndx`.
//!
//! # What this replaces
//!
//! The same information used to live in three fields of `IncrementalState`,
//! held in lockstep by hand:
//!
//! ```text
//! ndx_segments:        Vec<(usize, i32)>   // (flat_start, ndx_start)
//! segment_parent_flat: Vec<i32>            // aligned 1:1 with the above
//! first_segment_idx:   usize               // reclaim cursor into both
//! ```
//!
//! Two parallel arrays whose alignment was an unwritten invariant, plus three
//! separate searches over them - `partition_point` in two directions and a
//! `binary_search_by` for the gap - each spelled out at its own call site. The
//! `+ 1` gap rule (`flist.c:2966`) was open-coded where sub-lists are pushed,
//! away from the lookups that depend on it.
//!
//! [`NdxSegment`] makes the three values one record, so they cannot drift, and
//! the searches become methods on [`NdxMap`], so the mapping has one owner.
//!
//! # Why this is NOT a `VecDeque`
//!
//! Upstream's chain of `struct file_list` is a deque - `flist_free()` drops the
//! head - and it is tempting to mirror that here. It would be wrong. Upstream
//! frees a whole sub-list, *including its index range*, because a receiver that
//! is done with a sub-list never names it again. oc reclaims only the entry
//! storage (`file_list.reclaim_segment`) and keeps the flat index space, so the
//! mapping has to stay **total** for the entire transfer: a wire NDX is
//! absolute, and resolving one from a reclaimed segment must still land on the
//! right flat slot rather than on a neighbour's.
//!
//! So [`NdxMap::first_live`] is a reclaim cursor, not a front pointer, and the
//! backing store stays a `Vec`. The deque belongs to the *owning* segment chain
//! that a later change introduces; putting it here would silently corrupt
//! every NDX below the reclaim point.
//!
//! # Upstream Reference
//!
//! - `flist.c:2966` - `ndx_start = prev->ndx_start + prev->used + 1`
//! - `rsync.c:424` - `i = ndx - cur_flist->ndx_start`
//! - `sender.c:267-272` - a gap NDX resolves to the owning directory
//! - `flist.c:flist_free()` / `sender.c:248` - segment reclaim

/// One sub-list's position in both index spaces.
///
/// upstream keeps these on `struct file_list`; oc holds one flat entry list, so
/// a segment records where it starts in that list rather than owning entries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct NdxSegment {
    /// First entry's index in the flat `file_list`.
    flat_start: usize,
    /// First entry's wire NDX. upstream `ndx_start`.
    ndx_start: i32,
    /// Flat index of the directory this sub-list expands, or `-1` when there is
    /// none to itemize (an initial list whose first entry is not `.`).
    ///
    /// upstream reaches the same entry through
    /// `dir_flist->files[cur_flist->parent_ndx]` (`sender.c:269-272`). oc has no
    /// separate `dir_flist`, so the flat index is recorded directly and needs no
    /// second translation.
    parent_flat: i32,
}

impl NdxSegment {
    /// Wire NDX of this segment's first entry.
    pub(crate) fn ndx_start(&self) -> i32 {
        self.ndx_start
    }

    /// Owning directory's flat index, if this sub-list has one to itemize.
    fn parent_flat(&self) -> Option<usize> {
        (self.parent_flat >= 0).then_some(self.parent_flat as usize)
    }
}

/// The ordered segment table and the reclaim cursor into it.
#[derive(Debug)]
pub(crate) struct NdxMap {
    /// Ordered by both `flat_start` and `ndx_start`, which increase together.
    /// Never shrinks: see the module note on totality.
    segments: Vec<NdxSegment>,
    /// Index of the oldest segment whose entries have not been reclaimed.
    ///
    /// upstream `first_flist` (`flist.c:101`), advanced by `flist_free()`
    /// (`sender.c:248`). Distinct from index 0 because the *mapping* outlives
    /// the *storage*.
    first_live: usize,
}

impl NdxMap {
    /// A map holding just the initial list.
    ///
    /// `initial_ndx_start` comes from INC_RECURSE negotiation; without
    /// INC_RECURSE it is 0 and this stays the only segment.
    pub(crate) fn new(initial_ndx_start: i32) -> Self {
        Self {
            segments: vec![NdxSegment {
                flat_start: 0,
                ndx_start: initial_ndx_start,
                parent_flat: -1,
            }],
            first_live: 0,
        }
    }

    /// Wire NDX of the very first entry.
    ///
    /// Read by the flist writer to decide abbreviated vs unabbreviated hardlink
    /// followers, and by hardlink numbering, which stores `ndx_start + i` as a
    /// leader's gnum (`hlink.c:match_hard_links()`).
    pub(crate) fn first_ndx_start(&self) -> i32 {
        self.segments.first().map_or(0, NdxSegment::ndx_start)
    }

    /// Records which flat entry the *initial* list itemizes through its own gap
    /// NDX, or `-1` when it has none.
    ///
    /// The initial segment is built before the file list is classified, so its
    /// owning entry is only known afterwards. upstream keeps the equivalent in
    /// `flist->parent_ndx` (`flist.c:2572`), which points at `dir_flist[0]`
    /// (`.`) unless the first sorted entry's basename is not `.`.
    pub(crate) fn set_initial_parent_flat(&mut self, parent_flat: i32) {
        self.segments[0].parent_flat = parent_flat;
    }

    /// Appends a sub-list beginning at `flat_start`, owned by `parent_flat`.
    ///
    /// The wire `ndx_start` is derived here rather than by the caller, because
    /// it is the one place the `+ 1` gap can be stated once:
    /// `ndx_start = prev->ndx_start + prev->used + 1` (`flist.c:2966`). The
    /// skipped slot belongs to the parent directory; computing it at a call
    /// site is how it drifts from the lookups that assume it.
    ///
    /// `prev->used` is the previous segment's entry count, which for a flat
    /// list is the distance between the two `flat_start` values.
    pub(crate) fn push_sublist(&mut self, flat_start: usize, parent_flat: i32) -> i32 {
        let prev = self
            .segments
            .last()
            .copied()
            .expect("the initial segment always exists");
        let prev_used = (flat_start - prev.flat_start) as i32;
        let ndx_start = prev.ndx_start + prev_used + 1;
        self.segments.push(NdxSegment {
            flat_start,
            ndx_start,
            parent_flat,
        });
        ndx_start
    }

    /// Number of segments recorded.
    pub(crate) fn len(&self) -> usize {
        self.segments.len()
    }

    /// Resolves a wire NDX to its flat `file_list` index.
    ///
    /// upstream: `rsync.c:424` - `i = ndx - cur_flist->ndx_start`, after
    /// `flist_for_ndx()` has selected the list.
    pub(crate) fn wire_to_flat(&self, wire_ndx: i32) -> usize {
        let seg = self.segment_containing_ndx(wire_ndx);
        seg.flat_start + (wire_ndx - seg.ndx_start) as usize
    }

    /// Resolves a wire NDX read back from the receiver to the flat index whose
    /// entry drives itemize formatting and xattr responses.
    ///
    /// This is [`Self::wire_to_flat`] for every regular entry. Under
    /// INC_RECURSE the remote generator itemizes a directory by sending its
    /// sub-list's *gap* NDX `ndx_start - 1` (`generator.c:2313`) rather than the
    /// directory's own NDX. Feeding that through the plain mapping lands on the
    /// trailing file of the previous segment, so the row would print a file type
    /// char and the wrong path. upstream recovers the directory via
    /// `dir_flist->files[cur_flist->parent_ndx]` (`sender.c:269-272`); this maps
    /// the gap to the sub-list's recorded owning directory.
    ///
    /// Each sub-list resolves to its own directory - the initial list's gap to
    /// the `.` root, a subdirectory's gap to that subdirectory - so every
    /// directory is itemized exactly once, as upstream emits one row per
    /// directory.
    pub(crate) fn resolve_itemize(&self, wire_ndx: i32) -> usize {
        // A gap NDX `g` satisfies `g + 1 == ndx_start` for exactly one
        // sub-list, and no regular entry's NDX can equal a sub-list start minus
        // one because that slot is reserved (flist.c:2966). Binary search is
        // valid because `ndx_start` is strictly increasing.
        if let Ok(idx) = self
            .segments
            .binary_search_by(|seg| seg.ndx_start.cmp(&(wire_ndx + 1)))
            && let Some(parent) = self.segments[idx].parent_flat()
        {
            return parent;
        }
        self.wire_to_flat(wire_ndx)
    }

    /// Converts a flat index back to its wire NDX.
    ///
    /// upstream: `generator.c:2338` - `ndx = i + cur_flist->ndx_start`.
    ///
    /// Only used by tests since the NDX echo-back fix: the transfer loop
    /// preserves the original wire NDX rather than round-tripping, which is what
    /// keeps INC_RECURSE gap NDX values intact.
    #[cfg(test)]
    pub(crate) fn flat_to_wire(&self, flat_idx: usize) -> i32 {
        let idx = self
            .segments
            .partition_point(|seg| seg.flat_start <= flat_idx)
            - 1;
        let seg = self.segments[idx];
        seg.ndx_start + (flat_idx - seg.flat_start) as i32
    }

    /// The flat entry range of the oldest unreclaimed segment, when one can be
    /// reclaimed.
    ///
    /// Returns `None` unless a *later* segment exists: the segment the receiver
    /// is currently working through must stay live, which is why upstream frees
    /// `first_flist` only after `cur_flist` has moved past it
    /// (`sender.c:248`).
    pub(crate) fn reclaimable_range(&self) -> Option<(usize, usize)> {
        let first = self.first_live;
        let next = self.segments.get(first + 1)?;
        Some((self.segments[first].flat_start, next.flat_start))
    }

    /// Records that the range from [`Self::reclaimable_range`] has been freed.
    ///
    /// The segment stays in the table - only the cursor moves - because the
    /// index mapping must remain total. upstream can drop the record because it
    /// frees the index space with the storage; oc cannot.
    pub(crate) fn advance_reclaimed(&mut self) {
        self.first_live += 1;
    }

    /// Index of the oldest unreclaimed segment, for diagnostics.
    pub(crate) fn first_live(&self) -> usize {
        self.first_live
    }

    /// The segment owning `wire_ndx`.
    ///
    /// upstream: `flist.c:flist_for_ndx()`. Saturating rather than panicking on
    /// an NDX below the first segment preserves the previous behaviour exactly;
    /// such a value is a gap that [`Self::resolve_itemize`] has already handled,
    /// or a protocol violation the NDX reader rejects before reaching here.
    fn segment_containing_ndx(&self, wire_ndx: i32) -> NdxSegment {
        let idx = self
            .segments
            .partition_point(|seg| seg.ndx_start <= wire_ndx)
            .saturating_sub(1);
        self.segments[idx]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The initial list plus two sub-lists, laid out with upstream's gaps.
    /// Initial list holds 3 entries at flat 0..3, so the next starts at flat 3
    /// and its NDX skips one slot.
    fn three_segments() -> NdxMap {
        let mut map = NdxMap::new(0);
        map.push_sublist(3, 1);
        map.push_sublist(5, 4);
        map
    }

    #[test]
    fn ndx_start_reserves_upstream_s_one_slot_parent_gap() {
        let map = three_segments();
        // Initial list occupies wire 0..3; the next sub-list starts at 4, not 3.
        // flist.c:2966 - the skipped slot is the parent directory's.
        assert_eq!(map.first_ndx_start(), 0);
        assert_eq!(map.segments[1].ndx_start(), 4);
        // Second sub-list: prev started at flat 3, this at flat 5, so prev_used
        // is 2 -> 4 + 2 + 1.
        assert_eq!(map.segments[2].ndx_start(), 7);
    }

    #[test]
    fn wire_and_flat_round_trip_through_every_segment() {
        let map = three_segments();
        for (wire, flat) in [(0, 0), (2, 2), (4, 3), (5, 4), (7, 5), (9, 7)] {
            assert_eq!(map.wire_to_flat(wire), flat, "wire {wire}");
            assert_eq!(map.flat_to_wire(flat), wire, "flat {flat}");
        }
    }

    #[test]
    fn a_gap_ndx_itemizes_the_owning_directory_not_the_previous_file() {
        let map = three_segments();
        // Gap 3 belongs to the directory recorded at flat 1. Without the
        // redirect the plain mapping runs one past the initial list's three
        // entries; upstream's `i = ndx - ndx_start` would index off the end of
        // that list, and in oc's single flat vector it silently lands on flat 3,
        // the NEXT segment's first entry.
        assert_eq!(map.wire_to_flat(3), 3);
        assert_eq!(map.resolve_itemize(3), 1);

        assert_eq!(map.resolve_itemize(6), 4);
    }

    #[test]
    fn a_regular_ndx_is_unaffected_by_the_gap_redirect() {
        let map = three_segments();
        for wire in [0, 2, 4, 5, 7, 9] {
            assert_eq!(map.resolve_itemize(wire), map.wire_to_flat(wire), "{wire}");
        }
    }

    #[test]
    fn an_initial_list_with_no_directory_falls_through_to_the_plain_mapping() {
        // parent_flat -1 means there is no `.` root to itemize; the gap must not
        // resolve to entry 0.
        let map = NdxMap::new(0);
        assert_eq!(map.resolve_itemize(-1), map.wire_to_flat(-1));
    }

    #[test]
    fn reclaim_needs_a_later_segment_to_exist() {
        let mut map = NdxMap::new(0);
        // One segment: the receiver is still inside it, so nothing is freeable.
        assert_eq!(map.reclaimable_range(), None);

        map.push_sublist(3, 1);
        assert_eq!(map.reclaimable_range(), Some((0, 3)));
        map.advance_reclaimed();
        assert_eq!(map.reclaimable_range(), None);
    }

    #[test]
    fn reclaiming_does_not_disturb_the_mapping() {
        let mut map = three_segments();
        map.advance_reclaimed();
        // The whole point of keeping the record: an NDX in the reclaimed
        // segment must still resolve to its own flat slot, not to a neighbour.
        assert_eq!(map.wire_to_flat(0), 0);
        assert_eq!(map.wire_to_flat(2), 2);
        assert_eq!(map.resolve_itemize(3), 1);
        assert_eq!(map.first_live(), 1);
        assert_eq!(map.len(), 3);
    }

    #[test]
    fn a_negotiated_nonzero_initial_ndx_start_shifts_every_lookup() {
        let mut map = NdxMap::new(100);
        map.push_sublist(3, 1);
        assert_eq!(map.first_ndx_start(), 100);
        assert_eq!(map.wire_to_flat(100), 0);
        assert_eq!(map.wire_to_flat(104), 3);
        assert_eq!(map.resolve_itemize(103), 1);
    }
}
