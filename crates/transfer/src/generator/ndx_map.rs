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
//! `+ 1` gap rule (`flist.c:3511`) was open-coded where sub-lists are pushed,
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
//! - `flist.c:3511` - `ndx_start = prev->ndx_start + prev->used + 1`
//! - `rsync.c:437` - `i = ndx - cur_flist->ndx_start`
//! - `sender.c:552-558` - a gap NDX resolves to the owning directory
//! - `flist.c:flist_free()` / `sender.c:533` - segment reclaim

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
    /// `dir_flist->files[cur_flist->parent_ndx]` (`sender.c:552-558`). oc has no
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
    /// upstream `first_flist` (`flist.c:109`), advanced by `flist_free()`
    /// (`sender.c:533`). Distinct from index 0 because the *mapping* outlives
    /// the *storage*.
    first_live: usize,
    /// Flat end of the newest sub-list sent to the peer, or `None` while the
    /// initial list is the whole file list (no INC_RECURSE).
    ///
    /// A sub-list's end is the next one's start, so only the newest needs its
    /// own record: the file list may already hold entries of sub-lists not yet
    /// sent, and upstream's `flist_for_ndx()` knows only the lists it has.
    sent_end: Option<usize>,
}

/// A wire NDX that no live sub-list owns: past the newest list sent, or in
/// one already freed.
///
/// upstream: `rsync.c:972-981` - the `first` and `last` of the
/// `File-list index %d not in %d - %d` refusal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct NdxOutOfRange {
    /// `first_flist->ndx_start - 1`: the oldest live list's gap slot.
    pub(crate) first: i32,
    /// The newest list's last index.
    pub(crate) last: i32,
}

/// What a wire NDX inside the live range names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NdxSlot {
    /// A file-list entry, by flat index.
    Entry(usize),
    /// A sub-list's gap slot (`ndx_start - 1`, `flist.c:3511`), with the flat
    /// index of the directory that list expands, if it has one.
    Gap(Option<usize>),
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
            sent_end: None,
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
    /// `flist->parent_ndx` (`flist.c:3086-3088`), which points at `dir_flist[0]`
    /// (`.`) unless the first sorted entry's basename is not `.`.
    pub(crate) fn set_initial_parent_flat(&mut self, parent_flat: i32) {
        self.segments[0].parent_flat = parent_flat;
    }

    /// Records the initial list's entry count when INC_RECURSE splits the file
    /// list, so its index range ends where the list sent to the peer does.
    pub(crate) fn set_initial_used(&mut self, used: usize) {
        self.sent_end = Some(self.segments[0].flat_start + used);
    }

    /// Appends a sub-list beginning at `flat_start`, owned by `parent_flat`.
    ///
    /// The wire `ndx_start` is derived here rather than by the caller, because
    /// it is the one place the `+ 1` gap can be stated once:
    /// `ndx_start = prev->ndx_start + prev->used + 1` (`flist.c:3511`). The
    /// skipped slot belongs to the parent directory; computing it at a call
    /// site is how it drifts from the lookups that assume it.
    ///
    /// `prev->used` is the previous segment's entry count, which for a flat
    /// list is the distance between the two `flat_start` values. `used` is the
    /// new sub-list's own count, which bounds the indices the peer may name.
    pub(crate) fn push_sublist(&mut self, flat_start: usize, used: usize, parent_flat: i32) -> i32 {
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
        self.sent_end = Some(flat_start + used);
        ndx_start
    }

    /// Number of segments recorded.
    pub(crate) fn len(&self) -> usize {
        self.segments.len()
    }

    /// Resolves a wire NDX to its flat `file_list` index.
    ///
    /// upstream: `rsync.c:437` - `i = ndx - cur_flist->ndx_start`, after
    /// `flist_for_ndx()` has selected the list. Peer-supplied indices go
    /// through [`Self::resolve`] instead, which refuses what this would clamp.
    #[cfg(test)]
    pub(crate) fn wire_to_flat(&self, wire_ndx: i32) -> usize {
        let seg = self.segment_containing_ndx(wire_ndx);
        seg.flat_start + (wire_ndx - seg.ndx_start) as usize
    }

    /// Resolves a wire NDX the peer sent to the slot it names, refusing an
    /// index no live sub-list owns.
    ///
    /// A sub-list owns its entries and the gap slot just below them
    /// (`ndx_start - 1`, `flist.c:3511`), so the live lists tile one contiguous
    /// range from the oldest live gap to the newest list's last entry. Whether
    /// a gap is acceptable depends on who reads it, so it is returned as such:
    /// the remote generator itemizes a directory by sending that gap
    /// (`generator.c:2787`), while a `MSG_SUCCESS` for it is a protocol error.
    /// `entries` is the file-list length, the newest list's end when no
    /// INC_RECURSE split was recorded.
    ///
    /// # Upstream Reference
    ///
    /// - `rsync.c:951-983` - `flist_for_ndx()` walks `first_flist` and fails
    ///   outside `first_flist->ndx_start - 1 ..= last list's end`.
    /// - `sender.c:552-558` - a gap NDX names `dir_flist->files[parent_ndx]`.
    pub(crate) fn resolve(&self, wire_ndx: i32, entries: usize) -> Result<NdxSlot, NdxOutOfRange> {
        let newest = self.segments[self.segments.len() - 1];
        let used = self
            .sent_end
            .unwrap_or(entries)
            .saturating_sub(newest.flat_start);
        let first = self.segments[self.first_live].ndx_start - 1;
        let last = newest.ndx_start + used as i32 - 1;
        if wire_ndx < first || wire_ndx > last {
            return Err(NdxOutOfRange { first, last });
        }
        let idx = self
            .segments
            .partition_point(|seg| seg.ndx_start - 1 <= wire_ndx)
            - 1;
        let seg = self.segments[idx];
        if wire_ndx < seg.ndx_start {
            return Ok(NdxSlot::Gap(seg.parent_flat()));
        }
        Ok(NdxSlot::Entry(
            seg.flat_start + (wire_ndx - seg.ndx_start) as usize,
        ))
    }

    /// Converts a flat index back to its wire NDX.
    ///
    /// upstream: `generator.c:2812` - `ndx = i + cur_flist->ndx_start`.
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
    /// (`sender.c:533`).
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

    /// The segment owning `wire_ndx`, saturating below the first segment.
    #[cfg(test)]
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

    /// The initial list plus two sub-lists, laid out with upstream's gaps:
    /// flat 0..3 at wire 0..=2, flat 3..5 at wire 4..=5 (gap 3), flat 5..8 at
    /// wire 7..=9 (gap 6).
    fn three_segments() -> NdxMap {
        let mut map = NdxMap::new(0);
        map.push_sublist(3, 2, 1);
        map.push_sublist(5, 3, 4);
        map
    }

    /// Entries the file list holds for [`three_segments`].
    const ENTRIES: usize = 8;

    fn out_of_range(first: i32, last: i32) -> Result<NdxSlot, NdxOutOfRange> {
        Err(NdxOutOfRange { first, last })
    }

    fn entry(flat: usize) -> Result<NdxSlot, NdxOutOfRange> {
        Ok(NdxSlot::Entry(flat))
    }

    fn gap(parent: usize) -> Result<NdxSlot, NdxOutOfRange> {
        Ok(NdxSlot::Gap(Some(parent)))
    }

    #[test]
    fn ndx_start_reserves_upstream_s_one_slot_parent_gap() {
        let map = three_segments();
        // Initial list occupies wire 0..3; the next sub-list starts at 4, not 3.
        // flist.c:3511 - the skipped slot is the parent directory's.
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
            assert_eq!(map.resolve(wire, ENTRIES), entry(flat), "resolve {wire}");
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
        assert_eq!(map.resolve(3, ENTRIES), gap(1));
        assert_eq!(map.resolve(6, ENTRIES), gap(4));
    }

    /// The live range runs from the oldest gap slot to the newest list's last
    /// entry; one past either end is upstream's `File-list index` refusal.
    /// rsync.c:972-981 computes the same `first` and `last`.
    #[test]
    fn indices_past_either_end_of_the_live_range_are_refused() {
        let mut map = three_segments();
        map.set_initial_parent_flat(0);
        assert_eq!(map.resolve(-1, ENTRIES), gap(0), "first: the root's gap");
        assert_eq!(map.resolve(9, ENTRIES), entry(7), "last");
        assert_eq!(map.resolve(10, ENTRIES), out_of_range(-1, 9), "last + 1");
        assert_eq!(map.resolve(-2, ENTRIES), out_of_range(-1, 9), "first - 1");
        assert_eq!(map.resolve(i32::MAX, ENTRIES), out_of_range(-1, 9));
        assert_eq!(map.resolve(i32::MIN, ENTRIES), out_of_range(-1, 9));
    }

    /// The gap of a list with no directory names no entry at all, which the
    /// caller must refuse (sender.c:553-555); the plain mapping used to wrap
    /// here.
    #[test]
    fn the_gap_of_a_list_without_a_directory_names_no_entry() {
        let map = three_segments();
        assert_eq!(map.resolve(-1, ENTRIES), Ok(NdxSlot::Gap(None)));
    }

    /// Entries the sender has built but not yet sent are not the peer's to
    /// name: upstream's flist chain ends at the newest list sent.
    #[test]
    fn entries_of_an_unsent_sub_list_are_refused() {
        let map = three_segments();
        assert_eq!(map.resolve(10, ENTRIES + 50), out_of_range(-1, 9));
    }

    /// INC_RECURSE before any sub-list goes out: the initial list alone is
    /// live, even though the file list already holds later entries.
    #[test]
    fn the_initial_list_alone_bounds_the_range_until_a_sub_list_is_sent() {
        let mut map = NdxMap::new(1);
        map.set_initial_parent_flat(0);
        map.set_initial_used(3);
        assert_eq!(map.resolve(0, 40), gap(0), "the root's gap");
        assert_eq!(map.resolve(3, 40), entry(2));
        assert_eq!(map.resolve(4, 40), out_of_range(0, 3));
    }

    /// Without INC_RECURSE the single list is the whole file list.
    #[test]
    fn a_single_list_spans_the_whole_file_list() {
        let map = NdxMap::new(0);
        assert_eq!(map.resolve(0, 5), entry(0));
        assert_eq!(map.resolve(4, 5), entry(4));
        assert_eq!(map.resolve(5, 5), out_of_range(-1, 4));
        assert_eq!(map.resolve(0, 0), out_of_range(-1, -1), "an empty list");
    }

    #[test]
    fn reclaim_needs_a_later_segment_to_exist() {
        let mut map = NdxMap::new(0);
        // One segment: the receiver is still inside it, so nothing is freeable.
        assert_eq!(map.reclaimable_range(), None);
        map.push_sublist(3, 2, 1);
        assert_eq!(map.reclaimable_range(), Some((0, 3)));
        map.advance_reclaimed();
        assert_eq!(map.reclaimable_range(), None);
    }

    /// A freed list's indices are gone, as `flist_free()` drops them from
    /// upstream's chain; the next list's gap stays live because it belongs to
    /// that list.
    #[test]
    fn indices_in_a_reclaimed_segment_are_refused() {
        let mut map = three_segments();
        map.advance_reclaimed();
        assert_eq!(map.resolve(2, ENTRIES), out_of_range(3, 9));
        assert_eq!(map.resolve(0, ENTRIES), out_of_range(3, 9));
        assert_eq!(map.resolve(3, ENTRIES), gap(1), "sub-list 1's gap");
        assert_eq!(map.resolve(4, ENTRIES), entry(3));
        // The plain mapping stays total for the sender's own bookkeeping.
        assert_eq!(map.wire_to_flat(2), 2);
        assert_eq!(map.first_live(), 1);
        assert_eq!(map.len(), 3);
    }

    #[test]
    fn a_negotiated_nonzero_initial_ndx_start_shifts_every_lookup() {
        let mut map = NdxMap::new(100);
        map.push_sublist(3, 1, 1);
        assert_eq!(map.first_ndx_start(), 100);
        assert_eq!(map.wire_to_flat(100), 0);
        assert_eq!(map.wire_to_flat(104), 3);
        assert_eq!(map.resolve(103, 4), gap(1));
        assert_eq!(map.resolve(105, 4), out_of_range(99, 104));
    }
}
