//! An owning per-segment file-list container mirroring upstream's
//! `struct file_list` (rsync.h:984-995).
//!
//! # Scope
//!
//! [`FlistSegment`] is the owning counterpart of the range-borrowing
//! `PendingSegment` / `IncrementalState::ndx_segments` pair: one INC_RECURSE
//! sub-list that owns its entries and carries the index bookkeeping upstream
//! keeps on `struct file_list`. Nothing on the live path is wired to it yet -
//! the generator still drives `NdxMap` and the flat `DualFileList`. The chain
//! type (upstream's `first_flist`/`cur_flist` list-of-lists) and the routing of
//! the generator onto it are the follow-up seams; the `in_progress`/`to_redo`
//! redo counters declared here gain their transition methods with the phase-2
//! redo port. This mirrors the staged-construction precedent of
//! `receiver/ndx_stream.rs`.
//!
//! # Field map to upstream
//!
//! | field         | upstream anchor | meaning |
//! |---------------|-----------------|---------|
//! | `files`       | rsync.h:986 `files`, :988 `used`/`malloced` | entries in WIRE order; `used == files.len()`, `malloced == capacity` |
//! | `sorted`      | rsync.h:986 `sorted` | the sorted VIEW - alias of `files` or a cloned order (see below) |
//! | `low`/`high`  | rsync.h:990 | 0-relative bounds of the sorted view excluding empties |
//! | `ndx_start`   | rsync.h:991 | wire NDX of the first entry (inc_recurse offset) |
//! | `flist_num`   | rsync.h:992 | 1-relative list number, or 0 outside inc_recurse |
//! | `parent_ndx`  | rsync.h:993 | `dir_flist` index of the parent directory, or -1 |
//! | `in_progress` | rsync.h:994 | files from this list still being acted on |
//! | `to_redo`     | rsync.h:994 | files from this list queued for the phase-2 redo |
//!
//! # The sorted view is usually an alias
//!
//! Upstream sorts the POINTER array `sorted[]`, never the ndx-addressed
//! `files[]`: "We keep the 'files' list unsorted for our exchange of index
//! numbers with the other side (since their names may not sort the same)"
//! (flist.c:3270-3273). At every construction site `sorted` starts as a plain
//! alias of `files` and is cloned only when `need_unsorted_flist` demands a
//! separately-ordered copy (flist.c:2696-2700, :2811-2815, :3025-3046; the
//! flag is set for iconv at options.c:2200 and consumed at compat.c:800).
//! [`SortedView`] models exactly that: `Alias` is the common case, and
//! `Cloned` holds a permutation of indices into `files` so the entries are
//! never duplicated.

use protocol::flist::FileEntry;
use thiserror::Error;

/// Sentinel for "no parent directory": upstream stores `-1` in `parent_ndx`
/// for the initial list (flist.c:3088, :3082); every real parent is a
/// non-negative `dir_flist` index (flist.c:3366).
pub const PARENT_NDX_NONE: i32 = -1;

/// An invariant violation refused by [`FlistSegment`]'s constructors and
/// setters.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum FlistSegmentError {
    /// `parent_ndx` below the `-1` sentinel: upstream's field is either `-1`
    /// or a non-negative `dir_flist` index (rsync.h:993, flist.c:3366).
    #[error("invalid parent_ndx {0}: must be -1 or a non-negative dir_flist index")]
    InvalidParentNdx(i32),
    /// A cloned sorted order that is not a permutation of `0..used`.
    #[error("cloned sorted order is not a permutation of 0..{used}")]
    InvalidSortedOrder {
        /// Entry count the order had to cover.
        used: usize,
    },
    /// `low`/`high` outside the range upstream's `flist_sort_and_clean()` can
    /// produce (flist.c:3568-3571 for the empty list, :3341/:3406 otherwise).
    #[error("invalid sorted bounds low={low} high={high} for used={used}")]
    InvalidSortedBounds {
        /// Attempted lower bound.
        low: i32,
        /// Attempted upper bound.
        high: i32,
        /// Entry count of the segment.
        used: usize,
    },
}

/// The sorted view over a segment's entries.
///
/// Mirrors upstream's `sorted` pointer array: an alias of `files` in the
/// common case, or - only when a separately-ordered copy is needed
/// (`need_unsorted_flist`, flist.c:2696-2700) - an owned permutation. The
/// permutation holds indices into `files`, matching upstream's
/// pointer-array-copy shape without duplicating entries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SortedView {
    /// `sorted == files` (flist.c:2700, :2815, :3046): the view IS the wire
    /// order.
    Alias,
    /// A cloned order (flist.c:2697-2698, :2812-2813, :3031): element `i` of
    /// the view is `files[order[i]]`.
    Cloned(Vec<u32>),
}

/// One INC_RECURSE file-list segment that OWNS its entries.
///
/// The Rust shape of upstream's `struct file_list` (rsync.h:984-995) minus
/// the intrusive `next`/`prev` chain links (owned by the future chain type)
/// and the allocation pool (`file_pool`/`pool_boundary`, a later step).
///
/// # Invariants
///
/// - `ndx_start` is fixed at construction: the first list starts at
///   `inc_recurse ? 1 : 0` (flist.c:3503) and every successor at
///   `prev.ndx_start + prev.used + 1` (flist.c:3511) - the skipped slot is
///   the parent directory's gap NDX. [`FlistSegment::chain_after`] takes the
///   predecessor itself, so no call site can spell the arithmetic wrong.
/// - `flist_num` is monotonic along a chain: first `inc_recurse ? 1 : 0`
///   (flist.c:3503), successor `prev.flist_num + 1` (flist.c:3512).
/// - `parent_ndx` is `-1` or non-negative (rsync.h:993). It indexes the
///   separate `dir_flist`, so range validation against that list belongs to
///   the future `dir_flist` owner, not to this container.
/// - `files` stays in wire order for the life of the segment; only the
///   [`SortedView`] reorders (flist.c:3270-3273).
#[derive(Debug)]
pub struct FlistSegment {
    /// Entries in wire order. upstream: `files`/`used`/`malloced`
    /// (rsync.h:986, :988).
    files: Vec<FileEntry>,
    /// The sorted view. upstream: `sorted` (rsync.h:986).
    sorted: SortedView,
    /// Lower bound of the sorted view excluding leading empties
    /// (rsync.h:990). Zero-initialized like upstream's `new0`
    /// (flist.c:3491); meaningful only once the sort pass has run.
    low: i32,
    /// Upper bound of the sorted view (rsync.h:990); `-1` marks an empty
    /// cleaned list (flist.c:3568-3571). Zero-initialized like `new0`.
    high: i32,
    /// Wire NDX of `files[0]` (rsync.h:991).
    ndx_start: i32,
    /// 1-relative list number, 0 outside inc_recurse (rsync.h:992).
    flist_num: i32,
    /// `dir_flist` index of the parent directory, or [`PARENT_NDX_NONE`]
    /// (rsync.h:993).
    parent_ndx: i32,
    /// Files from this list the generator has started on and not yet retired
    /// (rsync.h:994; incremented at generator.c:2215/:2320/:2371, decremented
    /// at generator.c:2642 and io.c:1238). Transition methods arrive with the
    /// phase-2 redo port; until then the counter only reports its initial 0.
    in_progress: i32,
    /// Files from this list queued for the phase-2 redo (rsync.h:994;
    /// incremented at io.c:1270, decremented at generator.c:2673). Same seam
    /// as `in_progress`.
    to_redo: i32,
}

impl FlistSegment {
    /// Creates the FIRST segment of a chain.
    ///
    /// upstream: flist.c:3503 -
    /// `flist->ndx_start = flist->flist_num = inc_recurse ? 1 : 0;` for the
    /// list that founds the chain. Its `parent_ndx` is `-1` (flist.c:3088).
    #[must_use]
    pub fn first(inc_recurse: bool) -> Self {
        let start = i32::from(inc_recurse);
        Self::with_indices(start, start, PARENT_NDX_NONE)
    }

    /// Creates the segment that follows `prev` in a chain, owned by the
    /// directory at `parent_ndx` in the (separate) `dir_flist`.
    ///
    /// The wire start is derived here from the predecessor -
    /// `ndx_start = prev->ndx_start + prev->used + 1` (flist.c:3511) - so the
    /// `+ 1` parent-directory gap is a constructor invariant rather than
    /// call-site arithmetic. `flist_num = prev->flist_num + 1` (flist.c:3512)
    /// keeps the list number monotonic.
    ///
    /// # Errors
    ///
    /// Refuses a `parent_ndx` below [`PARENT_NDX_NONE`]: upstream's field is
    /// `-1` or a non-negative `dir_flist` index (rsync.h:993, flist.c:3366).
    /// Whether a non-negative value is in the `dir_flist`'s range is the
    /// `dir_flist` owner's check, since that list lives outside this segment.
    pub fn chain_after(prev: &Self, parent_ndx: i32) -> Result<Self, FlistSegmentError> {
        if parent_ndx < PARENT_NDX_NONE {
            return Err(FlistSegmentError::InvalidParentNdx(parent_ndx));
        }
        Ok(Self::with_indices(
            prev.ndx_start + prev.used_i32() + 1,
            prev.flist_num + 1,
            parent_ndx,
        ))
    }

    /// Shared field initialization: everything not passed in starts zeroed,
    /// matching upstream's `new0(struct file_list)` (flist.c:3491).
    fn with_indices(ndx_start: i32, flist_num: i32, parent_ndx: i32) -> Self {
        Self {
            files: Vec::new(),
            sorted: SortedView::Alias,
            low: 0,
            high: 0,
            ndx_start,
            flist_num,
            parent_ndx,
            in_progress: 0,
            to_redo: 0,
        }
    }

    /// Appends an entry, extending the wire order.
    pub fn push(&mut self, entry: FileEntry) {
        self.files.push(entry);
    }

    /// Number of entries. upstream: `used` (rsync.h:989).
    #[must_use]
    pub fn used(&self) -> usize {
        self.files.len()
    }

    /// `used` in upstream's `int` width, for wire-NDX arithmetic.
    fn used_i32(&self) -> i32 {
        self.files.len() as i32
    }

    /// Wire NDX of the first entry. upstream: `ndx_start` (rsync.h:991).
    #[must_use]
    pub fn ndx_start(&self) -> i32 {
        self.ndx_start
    }

    /// 1-relative list number, 0 outside inc_recurse. upstream: `flist_num`
    /// (rsync.h:992).
    #[must_use]
    pub fn flist_num(&self) -> i32 {
        self.flist_num
    }

    /// `dir_flist` index of the parent directory, or [`PARENT_NDX_NONE`].
    /// upstream: `parent_ndx` (rsync.h:993).
    #[must_use]
    pub fn parent_ndx(&self) -> i32 {
        self.parent_ndx
    }

    /// The reserved wire NDX just below this segment: `ndx_start - 1`.
    ///
    /// The `+ 1` in the chain arithmetic (flist.c:3511) leaves this slot
    /// unassigned; the remote generator uses it to itemize the segment's
    /// parent directory (`ndx = cur_flist->ndx_start - 1`, generator.c:2313),
    /// which the sender resolves through `parent_ndx` (sender.c:270-275).
    /// `NdxMap::resolve_itemize` relies on the same identity
    /// (`gap + 1 == ndx_start`).
    #[must_use]
    pub fn gap_ndx(&self) -> i32 {
        self.ndx_start - 1
    }

    /// One past the last wire NDX owned by this segment:
    /// `ndx_start + used`. A successor built by [`Self::chain_after`] starts
    /// at `end_ndx() + 1`, leaving its own gap slot.
    #[must_use]
    pub fn end_ndx(&self) -> i32 {
        self.ndx_start + self.used_i32()
    }

    /// Whether `wire_ndx` addresses an entry of this segment (its gap NDX is
    /// NOT contained - the slot is reserved, never assigned an entry).
    #[must_use]
    pub fn contains_ndx(&self, wire_ndx: i32) -> bool {
        wire_ndx >= self.ndx_start && wire_ndx < self.end_ndx()
    }

    /// The entry addressed by `wire_ndx`, if this segment owns it.
    ///
    /// upstream: `f = flist->files[ndx - flist->ndx_start]` (rsync.c:437,
    /// after `flist_for_ndx()` selected the list). Always resolves through
    /// `files` - wire NDX values address the WIRE order, never the sorted
    /// view (flist.c:3270-3273).
    #[must_use]
    pub fn entry_for_ndx(&self, wire_ndx: i32) -> Option<&FileEntry> {
        if !self.contains_ndx(wire_ndx) {
            return None;
        }
        self.files.get((wire_ndx - self.ndx_start) as usize)
    }

    /// Lower bound of the sorted view. upstream: `low` (rsync.h:990).
    #[must_use]
    pub fn low(&self) -> i32 {
        self.low
    }

    /// Upper bound of the sorted view; `-1` after cleaning an empty list
    /// (flist.c:3568-3571). upstream: `high` (rsync.h:990).
    #[must_use]
    pub fn high(&self) -> i32 {
        self.high
    }

    /// Records the sorted-view bounds a clean pass computed.
    ///
    /// upstream maintains `low`/`high` only inside `flist_sort_and_clean()`
    /// (flist.c:3568-3571 empty, :3341 first active, :3406 last kept); this
    /// setter is that pass's seam and validates the shapes it can produce:
    /// `low >= 0`, `-1 <= high < used`.
    ///
    /// # Errors
    ///
    /// Refuses bounds outside those shapes.
    pub fn set_sorted_bounds(&mut self, low: i32, high: i32) -> Result<(), FlistSegmentError> {
        if low < 0 || low > self.used_i32() || high < -1 || high >= self.used_i32() {
            return Err(FlistSegmentError::InvalidSortedBounds {
                low,
                high,
                used: self.used(),
            });
        }
        self.low = low;
        self.high = high;
        Ok(())
    }

    /// Whether the sorted view is still the plain alias of `files`.
    #[must_use]
    pub fn sorted_is_alias(&self) -> bool {
        matches!(self.sorted, SortedView::Alias)
    }

    /// Installs a cloned sorted order over the entries.
    ///
    /// upstream clones the pointer array only under `need_unsorted_flist`
    /// (flist.c:2696-2700) and sorts the CLONE, leaving `files` in wire order
    /// for the index-number exchange (flist.c:3270-3273). `order[i]` names
    /// the wire index of the view's `i`-th element.
    ///
    /// # Errors
    ///
    /// Refuses an `order` that is not a permutation of `0..used`, which
    /// would alias or drop entries the way a corrupted pointer copy would.
    pub fn set_cloned_sorted(&mut self, order: Vec<u32>) -> Result<(), FlistSegmentError> {
        let used = self.used();
        let mut seen = vec![false; used];
        let valid = order.len() == used
            && order.iter().all(|&i| {
                let i = i as usize;
                i < used && !std::mem::replace(&mut seen[i], true)
            });
        if !valid {
            return Err(FlistSegmentError::InvalidSortedOrder { used });
        }
        self.sorted = SortedView::Cloned(order);
        Ok(())
    }

    /// The `i`-th entry of the SORTED view (not the wire order).
    ///
    /// Under [`SortedView::Alias`] this is `files[i]` (flist.c:2700); under a
    /// cloned order it is `files[order[i]]`, the Rust reading of upstream's
    /// sorted pointer array.
    #[must_use]
    pub fn sorted_entry(&self, i: usize) -> Option<&FileEntry> {
        match &self.sorted {
            SortedView::Alias => self.files.get(i),
            SortedView::Cloned(order) => self.files.get(*order.get(i)? as usize),
        }
    }

    /// Files from this list still being acted on. upstream: `in_progress`
    /// (rsync.h:994); with `to_redo`, the counters that keep a segment alive
    /// until `first_flist->in_progress || first_flist->to_redo` clears
    /// (generator.c:2695).
    #[must_use]
    pub fn in_progress(&self) -> i32 {
        self.in_progress
    }

    /// Files from this list queued for the phase-2 redo. upstream: `to_redo`
    /// (rsync.h:994, incremented at io.c:1270).
    #[must_use]
    pub fn to_redo(&self) -> i32 {
        self.to_redo
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generator::ndx_map::NdxMap;

    fn file(name: &str) -> FileEntry {
        FileEntry::new_file(name.into(), 0, 0o644)
    }

    fn seg_with(names: &[&str], inc_recurse: bool) -> FlistSegment {
        let mut seg = FlistSegment::first(inc_recurse);
        for n in names {
            seg.push(file(n));
        }
        seg
    }

    /// WHY: the founding list's start is negotiation-dependent -
    /// `ndx_start = flist_num = inc_recurse ? 1 : 0` (flist.c:3503) - and its
    /// parent is the -1 sentinel (flist.c:3088). Getting 0 vs 1 wrong here
    /// shifts every wire NDX of the transfer.
    #[test]
    fn first_segment_mirrors_the_inc_recurse_negotiation() {
        let plain = FlistSegment::first(false);
        assert_eq!(plain.ndx_start(), 0);
        assert_eq!(plain.flist_num(), 0);
        assert_eq!(plain.parent_ndx(), PARENT_NDX_NONE);

        let inc = FlistSegment::first(true);
        assert_eq!(inc.ndx_start(), 1);
        assert_eq!(inc.flist_num(), 1);
        assert_eq!(inc.parent_ndx(), PARENT_NDX_NONE);
        assert_eq!(inc.in_progress(), 0);
        assert_eq!(inc.to_redo(), 0);
    }

    /// WHY: each successor must start at `prev.ndx_start + prev.used + 1`
    /// (flist.c:3511) - the skipped slot is the parent directory's gap NDX
    /// that the remote generator itemizes through (generator.c:2313). An
    /// off-by-one here desyncs every NDX the peers exchange.
    #[test]
    fn gap_arithmetic_holds_across_three_chained_segments() {
        let root = seg_with(&["a", "b", "c"], true); // wire 1..4
        let child = FlistSegment::chain_after(&root, 0).expect("valid parent");
        assert_eq!(child.ndx_start(), 5); // 1 + 3 + 1
        assert_eq!(child.flist_num(), 2);
        assert_eq!(child.gap_ndx(), root.end_ndx()); // gap 4 is the skipped slot

        let mut child = child;
        child.push(file("c/d"));
        child.push(file("c/e")); // wire 5..7
        let grand = FlistSegment::chain_after(&child, 1).expect("valid parent");
        assert_eq!(grand.ndx_start(), 8); // 5 + 2 + 1
        assert_eq!(grand.flist_num(), 3);
        assert_eq!(grand.gap_ndx(), 7);

        // The gaps are owned by no segment: the slots are reserved.
        for seg in [&root, &child, &grand] {
            assert!(!seg.contains_ndx(4));
            assert!(!seg.contains_ndx(7));
        }
    }

    /// WHY: `NdxMap::resolve_itemize` decodes a gap NDX via
    /// `gap + 1 == ndx_start` over the SAME flist.c:3511 arithmetic - the
    /// owning container and the live resolver must agree on where every
    /// segment starts, or IR-6a's rewiring would move wire bytes.
    #[test]
    fn chain_ndx_starts_agree_with_the_live_ndx_map() {
        let root = seg_with(&["a", "b", "c"], true);
        let mut child = FlistSegment::chain_after(&root, 0).expect("valid parent");
        child.push(file("c/d"));
        child.push(file("c/e"));
        let grand = FlistSegment::chain_after(&child, 1).expect("valid parent");

        // The map's segments are (flat_start, parent_flat) pairs over one
        // flat list; flat starts are the running entry counts.
        let mut map = NdxMap::new(root.ndx_start());
        let child_start = map.push_sublist(root.used(), 0);
        let grand_start = map.push_sublist(root.used() + child.used(), 3);

        assert_eq!(map.first_ndx_start(), root.ndx_start());
        assert_eq!(child_start, child.ndx_start());
        assert_eq!(grand_start, grand.ndx_start());
        // And the map resolves each segment's gap to its parent slot, which
        // only works because both sides reserve the same slot.
        assert_eq!(map.resolve_itemize(child.gap_ndx()), 0);
        assert_eq!(map.resolve_itemize(grand.gap_ndx()), 3);
    }

    /// WHY: `parent_ndx` is `-1` or a non-negative `dir_flist` index
    /// (rsync.h:993, flist.c:3366); anything below the sentinel is
    /// unrepresentable upstream and must be refused, not stored.
    #[test]
    fn chain_after_refuses_a_parent_below_the_sentinel() {
        let root = FlistSegment::first(true);
        assert_eq!(
            FlistSegment::chain_after(&root, -2).unwrap_err(),
            FlistSegmentError::InvalidParentNdx(-2)
        );
        // The sentinel and any dir_flist index are accepted.
        assert!(FlistSegment::chain_after(&root, PARENT_NDX_NONE).is_ok());
        assert!(FlistSegment::chain_after(&root, 0).is_ok());
    }

    /// WHY: `sorted` starts as an ALIAS of `files` (flist.c:2700); cloning is
    /// the exception, not the rule. The alias view must read back the wire
    /// order unchanged.
    #[test]
    fn sorted_view_defaults_to_the_files_alias() {
        let seg = seg_with(&["b", "a"], false);
        assert!(seg.sorted_is_alias());
        assert_eq!(seg.sorted_entry(0).map(FileEntry::name), Some("b"));
        assert_eq!(seg.sorted_entry(1).map(FileEntry::name), Some("a"));
        assert!(seg.sorted_entry(2).is_none());
    }

    /// WHY: upstream sorts the pointer COPY and never `files[]`, because the
    /// peers exchange index numbers over the wire order (flist.c:3270-3273).
    /// A cloned view must reorder reads while `entry_for_ndx` stays put.
    #[test]
    fn cloned_sorted_reorders_the_view_and_leaves_wire_order_alone() {
        let mut seg = seg_with(&["b", "a"], false);
        seg.set_cloned_sorted(vec![1, 0]).expect("permutation");
        assert!(!seg.sorted_is_alias());
        assert_eq!(seg.sorted_entry(0).map(FileEntry::name), Some("a"));
        assert_eq!(seg.sorted_entry(1).map(FileEntry::name), Some("b"));
        // Wire addressing is untouched: NDX 0 is still "b".
        assert_eq!(seg.entry_for_ndx(0).map(FileEntry::name), Some("b"));
        assert_eq!(seg.entry_for_ndx(1).map(FileEntry::name), Some("a"));
    }

    /// WHY: a non-permutation order is the Rust spelling of a corrupted
    /// pointer copy - it would alias one entry and drop another - so it must
    /// be refused whole, leaving the alias in place.
    #[test]
    fn a_non_permutation_sorted_order_is_refused() {
        let mut seg = seg_with(&["b", "a"], false);
        for bad in [vec![0], vec![0, 0], vec![0, 2], vec![0, 1, 1]] {
            assert_eq!(
                seg.set_cloned_sorted(bad),
                Err(FlistSegmentError::InvalidSortedOrder { used: 2 })
            );
            assert!(seg.sorted_is_alias());
        }
    }

    /// WHY: `low`/`high` bound the SORTED view excluding empties
    /// (rsync.h:990). They zero-init like `new0` (flist.c:3491), an empty
    /// clean pass records `low=0, high=-1` (flist.c:3568-3571), and no pass
    /// can produce `high >= used` - shapes outside that set are refused.
    #[test]
    fn low_high_bounds_mirror_the_upstream_clean_pass() {
        let mut empty = FlistSegment::first(false);
        assert_eq!((empty.low(), empty.high()), (0, 0)); // new0 zeroes
        empty.set_sorted_bounds(0, -1).expect("empty-list bounds");
        assert_eq!((empty.low(), empty.high()), (0, -1));

        let mut seg = seg_with(&["a", "b", "c"], false);
        seg.set_sorted_bounds(1, 2).expect("in-range bounds");
        assert_eq!((seg.low(), seg.high()), (1, 2));
        for (low, high) in [(-1, 2), (0, 3), (4, 2), (0, -2)] {
            assert_eq!(
                seg.set_sorted_bounds(low, high),
                Err(FlistSegmentError::InvalidSortedBounds { low, high, used: 3 })
            );
        }
        // The refused writes left the last good bounds in place.
        assert_eq!((seg.low(), seg.high()), (1, 2));
    }

    /// WHY: over a chain, every wire NDX from the first gap through the last
    /// entry must resolve to EXACTLY ONE meaning - one segment's entry or one
    /// segment's reserved gap - or NDX decoding is ambiguous. This walks the
    /// whole range, pre-figuring the chain-level `flist_for_ndx` totality
    /// gate (upstream: flist.c:flist_for_ndx over rsync.h:991 ranges).
    #[test]
    fn flat_resolution_over_a_chain_is_total_and_unambiguous() {
        let root = seg_with(&["a", "b", "c"], true);
        let mut child = FlistSegment::chain_after(&root, 0).expect("valid parent");
        child.push(file("c/d"));
        child.push(file("c/e"));
        let mut grand = FlistSegment::chain_after(&child, 1).expect("valid parent");
        grand.push(file("e/f"));
        let chain = [&root, &child, &grand];

        for ndx in root.gap_ndx()..grand.end_ndx() {
            let owners = chain.iter().filter(|s| s.contains_ndx(ndx)).count();
            let gaps = chain.iter().filter(|s| s.gap_ndx() == ndx).count();
            assert_eq!(owners + gaps, 1, "wire NDX {ndx} must have one meaning");
            // entry_for_ndx answers exactly on the owned slots.
            let entries = chain
                .iter()
                .filter(|s| s.entry_for_ndx(ndx).is_some())
                .count();
            assert_eq!(entries, owners, "wire NDX {ndx}");
        }
        // One past the end belongs to nobody.
        assert!(
            chain
                .iter()
                .all(|s| s.entry_for_ndx(grand.end_ndx()).is_none())
        );
    }
}
