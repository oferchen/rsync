//! File-list wrapper over `Vec<FileEntry>`.
//!
//! [`DualFileList`] is a thin newtype over `Vec<FileEntry>` that adds the
//! INC_RECURSE segment-reclaim, indexed access, and permutation-sort helpers
//! the generator relies on.

use std::fmt;
use std::ops::{Index, IndexMut, RangeFrom};

use super::FileEntry;

/// File list backed by a `Vec<FileEntry>`.
///
/// A thin newtype over `Vec<FileEntry>` that adds the segment-reclaim and
/// indexing helpers the generator relies on. [`push`](Self::push) is a plain
/// `Vec::push`.
pub struct DualFileList {
    legacy: Vec<FileEntry>,
}

impl fmt::Debug for DualFileList {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DualFileList")
            .field("len", &self.legacy.len())
            .finish()
    }
}

impl DualFileList {
    /// Creates an empty dual file list.
    #[must_use]
    pub fn new() -> Self {
        Self { legacy: Vec::new() }
    }

    /// Creates a dual file list pre-allocated for `cap` entries.
    #[must_use]
    pub fn with_capacity(cap: usize) -> Self {
        Self {
            legacy: Vec::with_capacity(cap),
        }
    }

    /// Appends an entry to the list.
    pub fn push(&mut self, entry: FileEntry) {
        self.legacy.push(entry);
    }

    /// Returns the number of entries in the list.
    #[must_use]
    pub fn len(&self) -> usize {
        self.legacy.len()
    }

    /// Returns `true` if the list contains no entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.legacy.is_empty()
    }

    /// Returns a slice of all legacy entries.
    #[must_use]
    pub fn as_slice(&self) -> &[FileEntry] {
        &self.legacy
    }

    /// Returns a reference to the entry at `index`, or `None` if out of bounds.
    #[must_use]
    pub fn get(&self, index: usize) -> Option<&FileEntry> {
        self.legacy.get(index)
    }

    /// Returns an iterator over references to the legacy entries.
    pub fn iter(&self) -> std::slice::Iter<'_, FileEntry> {
        self.legacy.iter()
    }

    /// Returns an iterator over mutable references to the legacy entries.
    pub fn iter_mut(&mut self) -> std::slice::IterMut<'_, FileEntry> {
        self.legacy.iter_mut()
    }

    /// Returns the current length of the legacy Vec, for use as a segment
    /// start index in INC_RECURSE sub-list building.
    #[must_use]
    pub fn segment_start(&self) -> usize {
        self.legacy.len()
    }

    /// Clears all entries from the list.
    pub fn clear(&mut self) {
        self.legacy.clear();
    }

    /// Reserves capacity for at least `additional` more entries.
    pub fn reserve(&mut self, additional: usize) {
        self.legacy.reserve(additional);
    }

    /// Returns a mutable reference to the underlying legacy Vec.
    ///
    /// Exposed for call sites that need direct Vec access (e.g. sorting,
    /// filtering, INC_RECURSE segment manipulation).
    pub fn as_mut_vec(&mut self) -> &mut Vec<FileEntry> {
        &mut self.legacy
    }

    /// Consumes the dual list and returns the underlying legacy Vec.
    #[must_use]
    pub fn into_vec(self) -> Vec<FileEntry> {
        self.legacy
    }

    /// Sort the file list using upstream `f_name_cmp` ordering and apply the
    /// resulting permutation to `parallel` in lockstep so caller-owned arrays
    /// (e.g. the generator's `full_paths`) stay aligned with the sorted list.
    ///
    /// `use_qsort` selects the unstable sort matching upstream `--qsort`. When
    /// `false`, the stable sort matches upstream's default behaviour. Both
    /// invariants are preserved from the prior external sort site that called
    /// [`apply_permutation_in_place`](super::sort::apply_permutation_in_place)
    /// directly on the legacy Vec.
    ///
    /// # Panics
    ///
    /// Panics in debug builds when `parallel.len() != self.len()`.
    ///
    /// upstream: flist.c:f_name_cmp() with indirect permutation
    pub fn sort_with_parallel<P>(&mut self, parallel: &mut [P], use_qsort: bool) {
        let n = self.legacy.len();
        if n == 0 {
            return;
        }
        debug_assert_eq!(parallel.len(), n);

        let mut indices: Vec<usize> = (0..n).collect();
        let cmp = |&a: &usize, &b: &usize| {
            super::sort::compare_file_entries(&self.legacy[a], &self.legacy[b])
        };
        if use_qsort {
            indices.sort_unstable_by(cmp);
        } else {
            indices.sort_by(cmp);
        }

        super::sort::apply_permutation_in_place(&mut self.legacy, parallel, indices);
    }

    /// Removes duplicate-name entries in-place after sorting, keeping the
    /// upstream survivor, and applies the same removals to `parallel` in
    /// lockstep so a caller-owned array (the generator's `source_bases`) stays
    /// aligned with the cleaned list.
    ///
    /// The list MUST already be sorted (call
    /// [`sort_with_parallel`](Self::sort_with_parallel) first) - dedup only
    /// collapses adjacent equal names. On a list with no duplicates this changes
    /// neither order nor length.
    ///
    /// # Sender skip
    ///
    /// A non-incremental sender (`am_sender && !inc_recurse`) must NOT remove
    /// duplicates: upstream skips the clean loop entirely (`flist.c:3039-3042`)
    /// and transmits every entry as-is, so the receiver's in-place tombstones
    /// keep both sides' NDX numbering aligned. This method returns immediately in
    /// that case, leaving the list (and `parallel`) untouched.
    ///
    /// Under INC_RECURSE the pass still runs (upstream's `!am_sender ||
    /// inc_recurse` branch). Two rules apply, both mirroring the sender side of
    /// `flist_sort_and_clean()`:
    ///
    /// - **Top-level duplicates are kept alive.** Upstream gates `clear_file()`
    ///   on `!am_sender` (flist.c:3083-3090), so the sender never removes a
    ///   duplicate; a repeated top-level source arg (e.g. `rsync -r foo foo
    ///   dest`) stays on the wire and the receiver tombstones it, keeping both
    ///   sides' NDX aligned. A duplicate top-level directory is additionally
    ///   marked FLAG_DUPLICATE (flist.c:3073) so the sub-list scheduler batches
    ///   the same-named dirs into one sub-list.
    /// - **Nested duplicates are collapsed.** oc walks the whole tree eagerly, so
    ///   a repeated source dir yields duplicate *nested* entries that upstream -
    ///   scanning each physical dir once - never produces. Those are collapsed
    ///   via `resolve_duplicate`'s keep/drop
    ///   tie-break, leaving the wire byte-identical to upstream's single scan.
    ///
    /// # Panics
    ///
    /// Panics in debug builds when `parallel.len() != self.len()`.
    ///
    /// # Upstream Reference
    ///
    /// - `flist.c:2544` - `flist_sort_and_clean(flist, 0)` runs in
    ///   `send_file_list()`; the sender passes `strip_root = 0`.
    /// - `flist.c:3039-3042` - `am_sender && !inc_recurse` skips the clean loop.
    /// - `flist.c:3046-3082` - the duplicate-removal tie-break this mirrors.
    pub fn dedup_with_parallel<P>(
        &mut self,
        parallel: &mut Vec<P>,
        am_sender: bool,
        inc_recurse: bool,
    ) -> super::sort::CleanResult {
        let len = self.legacy.len();
        let mut stats = super::sort::CleanResult::default();
        if len == 0 {
            return stats;
        }
        // upstream: flist.c:3039-3042 - a non-incremental sender transmits
        // duplicates as-is (skips the clean loop) so the receiver's tombstones
        // keep both sides' NDX numbering aligned. A sender cannot tombstone-skip
        // a slot without transmitting fewer entries than its array holds, which
        // would desync the wire NDX; transmitting the full array is the only
        // NDX-safe behaviour.
        if am_sender && !inc_recurse {
            return stats;
        }
        debug_assert_eq!(parallel.len(), len);

        // Write cursor `w` marks the last kept entry; read cursor `r` scans
        // ahead. Adjacent equal names collapse; the survivor's parallel base
        // travels with it.
        let mut w: usize = 0;
        let mut r: usize = 1;
        while r < len {
            if self.legacy[w].name() != self.legacy[r].name() {
                w += 1;
                if w != r {
                    self.legacy.swap(w, r);
                    parallel.swap(w, r);
                }
                r += 1;
                continue;
            }

            // upstream: flist.c:3072-3090 - `clear_file()` is gated on
            // `!am_sender`, so the sender NEVER removes a duplicate; a duplicate
            // top-level source arg stays alive and the receiver's own clean pass
            // tombstones it, keeping both sides' NDX aligned. Under INC_RECURSE a
            // duplicate directory is additionally marked FLAG_DUPLICATE so the
            // sub-list scheduler batches the same-named dirs into one sub-list
            // (flist.c:2162-2168). oc walks the whole tree eagerly, so a repeated
            // source dir also yields duplicate *nested* entries that upstream -
            // which scans each physical dir once - never produces; those nested
            // duplicates are collapsed below. Only top-level duplicates (the
            // genuine repeated source args) are kept, matching upstream's wire
            // entry count and NDX numbering.
            let top_level = !self.legacy[r].name().contains('/');
            if am_sender && top_level {
                let both_dirs = self.legacy[w].is_dir() && self.legacy[r].is_dir();
                w += 1;
                if w != r {
                    self.legacy.swap(w, r);
                    parallel.swap(w, r);
                }
                // upstream: flist.c:3072-3073 - FLAG_DUPLICATE marks the later
                // directory (only when both entries are dirs) so
                // `send_extra_file_list()` batches the duplicate-named dirs into a
                // single sub-list. The later entry now sits at index `w`.
                if both_dirs {
                    self.legacy[w].set_duplicate(true);
                }
                r += 1;
                continue;
            }

            let (left, right) = self.legacy.split_at_mut(r);
            if super::sort::resolve_duplicate(&mut left[w], &right[0], &mut stats) {
                self.legacy.swap(w, r);
                parallel.swap(w, r);
            }
            r += 1;
        }

        self.legacy.truncate(w + 1);
        parallel.truncate(w + 1);
        stats
    }

    /// Reclaims heap data from entries in the range `[start..end)`.
    ///
    /// Calls [`FileEntry::reclaim_heap_data`] on each entry in the range,
    /// freeing PathBuf, dirname Arc, and extras Box allocations while
    /// keeping the entries in place so NDX-based indexing remains valid.
    ///
    /// This mirrors upstream rsync's `flist_free()` which deallocates
    /// completed INC_RECURSE segments during the transfer loop.
    ///
    /// # Panics
    ///
    /// Panics if `end > self.len()` or `start > end`.
    pub fn reclaim_segment(&mut self, start: usize, end: usize) {
        assert!(
            end <= self.legacy.len() && start <= end,
            "reclaim_segment: [{start}..{end}) out of bounds (len={})",
            self.legacy.len()
        );
        for entry in &mut self.legacy[start..end] {
            entry.reclaim_heap_data();
        }
    }
}

impl Default for DualFileList {
    fn default() -> Self {
        Self::new()
    }
}

impl Index<usize> for DualFileList {
    type Output = FileEntry;

    fn index(&self, index: usize) -> &Self::Output {
        &self.legacy[index]
    }
}

impl Index<RangeFrom<usize>> for DualFileList {
    type Output = [FileEntry];

    fn index(&self, index: RangeFrom<usize>) -> &Self::Output {
        &self.legacy[index]
    }
}

impl IndexMut<usize> for DualFileList {
    fn index_mut(&mut self, index: usize) -> &mut Self::Output {
        &mut self.legacy[index]
    }
}

impl<'a> IntoIterator for &'a DualFileList {
    type Item = &'a FileEntry;
    type IntoIter = std::slice::Iter<'a, FileEntry>;

    fn into_iter(self) -> Self::IntoIter {
        self.legacy.iter()
    }
}

impl<'a> IntoIterator for &'a mut DualFileList {
    type Item = &'a mut FileEntry;
    type IntoIter = std::slice::IterMut<'a, FileEntry>;

    fn into_iter(self) -> Self::IntoIter {
        self.legacy.iter_mut()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_is_empty() {
        let list = DualFileList::new();
        assert!(list.is_empty());
        assert_eq!(list.len(), 0);
        assert!(list.get(0).is_none());
    }

    #[test]
    fn with_capacity_is_empty() {
        let list = DualFileList::with_capacity(64);
        assert!(list.is_empty());
        assert_eq!(list.len(), 0);
    }

    #[test]
    fn default_is_empty() {
        let list = DualFileList::default();
        assert!(list.is_empty());
        assert_eq!(list.len(), 0);
    }

    #[test]
    fn push_and_read_back() {
        let mut list = DualFileList::new();
        let entry = FileEntry::new_file("src/main.rs".into(), 1024, 0o644);
        list.push(entry);

        assert_eq!(list.len(), 1);
        assert!(!list.is_empty());
        assert_eq!(list[0].name(), "src/main.rs");
        assert_eq!(list[0].size(), 1024);
    }

    #[test]
    fn get_returns_none_out_of_bounds() {
        let mut list = DualFileList::new();
        list.push(FileEntry::new_file("a.txt".into(), 10, 0o644));
        assert!(list.get(0).is_some());
        assert!(list.get(1).is_none());
        assert!(list.get(100).is_none());
    }

    #[test]
    fn as_slice_returns_all_entries() {
        let mut list = DualFileList::new();
        list.push(FileEntry::new_file("a.txt".into(), 10, 0o644));
        list.push(FileEntry::new_file("b.txt".into(), 20, 0o644));
        let slice = list.as_slice();
        assert_eq!(slice.len(), 2);
        assert_eq!(slice[0].name(), "a.txt");
        assert_eq!(slice[1].name(), "b.txt");
    }

    #[test]
    fn iter_yields_all_entries_in_order() {
        let mut list = DualFileList::new();
        let names = ["alpha", "beta", "gamma"];
        for name in &names {
            list.push(FileEntry::new_file((*name).into(), 0, 0o644));
        }
        let collected: Vec<&str> = list.iter().map(|e| e.name()).collect();
        assert_eq!(collected, names);
    }

    #[test]
    fn iter_mut_allows_modification() {
        let mut list = DualFileList::new();
        list.push(FileEntry::new_file("f.txt".into(), 100, 0o644));
        for entry in list.iter_mut() {
            entry.set_size(200);
        }
        assert_eq!(list[0].size(), 200);
    }

    #[test]
    fn segment_start_tracks_length() {
        let mut list = DualFileList::new();
        assert_eq!(list.segment_start(), 0);
        list.push(FileEntry::new_file("a.txt".into(), 0, 0o644));
        assert_eq!(list.segment_start(), 1);
        list.push(FileEntry::new_file("b.txt".into(), 0, 0o644));
        assert_eq!(list.segment_start(), 2);
    }

    #[test]
    fn index_usize() {
        let mut list = DualFileList::new();
        list.push(FileEntry::new_file("test.txt".into(), 42, 0o644));
        assert_eq!(list[0].size(), 42);
    }

    #[test]
    fn index_range_from() {
        let mut list = DualFileList::new();
        list.push(FileEntry::new_file("a.txt".into(), 1, 0o644));
        list.push(FileEntry::new_file("b.txt".into(), 2, 0o644));
        list.push(FileEntry::new_file("c.txt".into(), 3, 0o644));
        let tail = &list[1..];
        assert_eq!(tail.len(), 2);
        assert_eq!(tail[0].name(), "b.txt");
        assert_eq!(tail[1].name(), "c.txt");
    }

    #[test]
    fn index_mut_usize() {
        let mut list = DualFileList::new();
        list.push(FileEntry::new_file("f.txt".into(), 10, 0o644));
        list[0].set_size(99);
        assert_eq!(list[0].size(), 99);
    }

    #[test]
    fn into_vec_returns_legacy() {
        let mut list = DualFileList::new();
        list.push(FileEntry::new_file("x.txt".into(), 5, 0o644));
        let vec = list.into_vec();
        assert_eq!(vec.len(), 1);
        assert_eq!(vec[0].name(), "x.txt");
    }

    #[test]
    fn as_mut_vec_allows_direct_manipulation() {
        let mut list = DualFileList::new();
        list.push(FileEntry::new_file("z.txt".into(), 0, 0o644));
        list.push(FileEntry::new_file("a.txt".into(), 0, 0o644));
        list.as_mut_vec().sort_by(|a, b| a.name().cmp(b.name()));
        assert_eq!(list[0].name(), "a.txt");
        assert_eq!(list[1].name(), "z.txt");
    }

    #[test]
    fn reclaim_segment_clears_entries_in_range() {
        let mut list = DualFileList::new();
        for i in 0..5 {
            list.push(FileEntry::new_file(
                format!("file_{i}.txt").into(),
                (i + 1) as u64 * 100,
                0o644,
            ));
        }

        // Reclaim entries [1..3)
        list.reclaim_segment(1, 3);

        // Unreclaimed entries are intact.
        assert_eq!(list[0].name(), "file_0.txt");
        assert_eq!(list[0].size(), 100);
        assert_eq!(list[3].name(), "file_3.txt");
        assert_eq!(list[4].name(), "file_4.txt");

        // Reclaimed entries have empty names and zero sizes.
        assert_eq!(list[1].name(), "");
        assert_eq!(list[1].size(), 0);
        assert_eq!(list[2].name(), "");
        assert_eq!(list[2].size(), 0);

        // List length is unchanged (entries stay in place).
        assert_eq!(list.len(), 5);
    }

    #[test]
    fn dedup_with_parallel_noop_on_distinct_names() {
        // WHY: the dedup must not shift order or count for a normal
        // (duplicate-free) list, or sender/receiver NDX numbering desyncs.
        // Exercised in the INC_RECURSE mode where the sender still cleans.
        let mut list = DualFileList::new();
        for n in ["a.txt", "b.txt", "c.txt"] {
            list.push(FileEntry::new_file(n.into(), 0, 0o644));
        }
        let mut bases = vec!["ba", "bb", "bc"];
        let stats = list.dedup_with_parallel(&mut bases, true, true);
        assert_eq!(stats.duplicates_removed, 0);
        let names: Vec<&str> = list.iter().map(|e| e.name()).collect();
        assert_eq!(names, ["a.txt", "b.txt", "c.txt"]);
        assert_eq!(bases, ["ba", "bb", "bc"]);
    }

    #[test]
    fn dedup_with_parallel_sender_noninc_transmits_duplicates() {
        // WHY: a non-incremental sender must NOT remove duplicates - it transmits
        // every entry so the receiver's in-place tombstones keep the wire NDX
        // aligned. upstream: flist.c:3039-3042.
        let mut list = DualFileList::new();
        list.push(FileEntry::new_file("dup".into(), 0, 0o644));
        list.push(FileEntry::new_file("dup".into(), 0, 0o644));
        list.push(FileEntry::new_file("z".into(), 0, 0o644));
        let mut bases = vec!["first", "second", "zbase"];
        let stats = list.dedup_with_parallel(&mut bases, true, false);
        assert_eq!(stats.duplicates_removed, 0);
        let names: Vec<&str> = list.iter().map(|e| e.name()).collect();
        assert_eq!(names, ["dup", "dup", "z"]);
        assert_eq!(bases, ["first", "second", "zbase"]);
    }

    #[test]
    fn dedup_with_parallel_sender_inc_keeps_top_level_dups_alive() {
        // WHY: upstream gates clear_file() on !am_sender (flist.c:3083-3090), so
        // a sender NEVER removes a duplicate - even under INC_RECURSE. A repeated
        // top-level source arg stays on the wire; the receiver's own clean pass
        // tombstones it, keeping both sides' NDX numbering aligned. Truncating it
        // (the old behaviour) made oc send one fewer entry than upstream and
        // shifted every following NDX, desyncing an upstream receiver.
        let mut list = DualFileList::new();
        list.push(FileEntry::new_file("dup".into(), 0, 0o644));
        list.push(FileEntry::new_file("dup".into(), 0, 0o644));
        list.push(FileEntry::new_file("z".into(), 0, 0o644));
        let mut bases = vec!["first", "second", "zbase"];
        let stats = list.dedup_with_parallel(&mut bases, true, true);
        assert_eq!(stats.duplicates_removed, 0);
        let names: Vec<&str> = list.iter().map(|e| e.name()).collect();
        assert_eq!(names, ["dup", "dup", "z"]);
        // Bases stay aligned 1:1 with the (unremoved) entries.
        assert_eq!(bases, ["first", "second", "zbase"]);
        // Plain files are never marked FLAG_DUPLICATE - only dirs, for sub-list
        // batching (flist.c:3072-3073 sets it inside the both-dirs branch).
        assert!(!list[1].duplicate());
    }

    #[test]
    fn dedup_with_parallel_sender_inc_keeps_top_level_dir_dup_flagged() {
        // WHY: `rsync -r foo foo dest` under INC_RECURSE. Upstream keeps BOTH
        // top-level `foo` dirs (clear_file gated on !am_sender) and marks the
        // later FLAG_DUPLICATE so send_extra_file_list batches them into one
        // sub-list (flist.c:3073, 2162-2168). oc walks the tree eagerly, so
        // `foo`'s children appear twice; those NESTED duplicates are collapsed
        // (upstream scans each physical dir once), but the two top-level `foo`
        // entries stay - matching upstream's wire entry count and NDX numbering
        // so an upstream receiver stays in sync.
        let mut list = DualFileList::new();
        list.push(FileEntry::new_directory("foo".into(), 0o755));
        list.push(FileEntry::new_directory("foo".into(), 0o755));
        list.push(FileEntry::new_file("foo/a".into(), 0, 0o644));
        list.push(FileEntry::new_file("foo/a".into(), 0, 0o644));
        let mut bases = vec!["b0", "b1", "b2", "b3"];
        list.dedup_with_parallel(&mut bases, true, true);
        let names: Vec<&str> = list.iter().map(|e| e.name()).collect();
        assert_eq!(names, ["foo", "foo", "foo/a"]);
        assert!(
            !list[0].duplicate(),
            "the earlier survivor is not a duplicate"
        );
        assert!(list[1].duplicate(), "the later foo carries FLAG_DUPLICATE");
    }

    #[test]
    fn dedup_with_parallel_keeps_directory_over_file() {
        // WHY: a NESTED same-named dir must win over a same-named file "because
        // it might have contents in the list" (flist.c:3065); its base must
        // travel with it. Nested duplicates are oc double-walk artifacts
        // (upstream scans each physical dir once) and ARE collapsed, unlike the
        // top-level source-arg dups kept above.
        let mut list = DualFileList::new();
        list.push(FileEntry::new_file("sub/item".into(), 0, 0o644));
        list.push(FileEntry::new_directory("sub/item".into(), 0o755));
        let mut bases = vec!["file_base", "dir_base"];
        let stats = list.dedup_with_parallel(&mut bases, true, true);
        assert_eq!(stats.duplicates_removed, 1);
        assert_eq!(list.len(), 1);
        assert!(list[0].is_dir());
        assert_eq!(bases, ["dir_base"]);
    }

    #[test]
    fn sender_noninc_transmit_then_receiver_tombstones_align() {
        // WHY: the non-incremental sender transmits duplicates as-is; the
        // receiver's flist_clean then TOMBSTONES the duplicate in place, keeping
        // the array length (and every NDX slot) so both sides stay aligned (no
        // RERR_PROTOCOL desync). upstream: flist.c:3039-3042 + 3089.
        let mut list = DualFileList::new();
        list.push(FileEntry::new_file("a".into(), 0, 0o644));
        list.push(FileEntry::new_file("dup".into(), 0, 0o644));
        list.push(FileEntry::new_file("dup".into(), 0, 0o644));
        let mut bases = vec!["a", "d1", "d2"];
        // Non-inc sender: no removal.
        list.dedup_with_parallel(&mut bases, true, false);
        assert_eq!(list.len(), 3);
        let transmitted = list.into_vec();
        // Receiver tombstones the second "dup" but preserves the slot count.
        let (receiver_list, stats) =
            crate::flist::sort::flist_clean(transmitted.clone(), false, false);
        assert_eq!(stats.duplicates_removed, 1);
        assert_eq!(
            receiver_list.len(),
            transmitted.len(),
            "receiver must keep every NDX slot"
        );
        let active: Vec<&str> = receiver_list
            .iter()
            .filter(|e| e.is_active())
            .map(|e| e.name())
            .collect();
        assert_eq!(active, ["a", "dup"]);
    }

    #[test]
    fn reclaim_segment_empty_range_is_noop() {
        let mut list = DualFileList::new();
        list.push(FileEntry::new_file("a.txt".into(), 10, 0o644));
        list.reclaim_segment(0, 0);
        assert_eq!(list[0].name(), "a.txt");
    }

    #[test]
    fn reclaim_segment_full_range() {
        let mut list = DualFileList::new();
        list.push(FileEntry::new_file("a.txt".into(), 10, 0o644));
        list.push(FileEntry::new_file("b.txt".into(), 20, 0o644));
        list.reclaim_segment(0, 2);
        assert_eq!(list[0].name(), "");
        assert_eq!(list[1].name(), "");
        assert_eq!(list.len(), 2);
    }
}
