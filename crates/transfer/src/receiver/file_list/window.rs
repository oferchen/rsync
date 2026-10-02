//! The receiver's flat file list as a window that can release its oldest
//! entries.
//!
//! Every receiver pass addresses entries by their absolute flat index (the
//! position in the concatenation of every received segment). Under
//! INC_RECURSE upstream frees each finished sub-list (`flist_free(first_flist)`,
//! generator.c:2711) so resident file-list memory stays bounded by the
//! lookahead window rather than the tree size. [`FileListWindow`] keeps the
//! absolute indexing and lets the streaming receiver release a finished prefix.
//!
//! An index below [`FileListWindow::live_start`] is released: [`get`] returns
//! `None` and direct indexing panics, the local analog of upstream's
//! `flist_for_ndx()` failing on a freed list (rsync.c:311-316).
//!
//! Released entries are dropped in place at once (their heap is freed) but the
//! backing storage is compacted only once the released prefix is at least as
//! large as the live part, so each entry is moved at most a constant number of
//! times.
//!
//! [`get`]: FileListWindow::get

use std::ops::{Index, IndexMut, Range, RangeFrom, RangeTo};

use protocol::flist::FileEntry;

/// Released entries retained before the backing storage is compacted.
const MIN_COMPACT: usize = 4096;

/// The receiver's flat file list, addressable by absolute flat index.
#[derive(Debug, Default, Clone)]
pub(in crate::receiver) struct FileListWindow {
    /// Absolute flat index of `entries[0]`.
    phys_base: usize,
    /// Absolute flat index of the oldest live entry; `>= phys_base`.
    live_start: usize,
    entries: Vec<FileEntry>,
}

impl FileListWindow {
    /// One past the absolute flat index of the newest entry.
    pub(in crate::receiver) fn len(&self) -> usize {
        self.phys_base + self.entries.len()
    }

    /// Absolute flat index of the oldest entry still resident.
    pub(in crate::receiver) const fn live_start(&self) -> usize {
        self.live_start
    }

    /// The live entries, starting at [`Self::live_start`].
    pub(in crate::receiver) fn live(&self) -> &[FileEntry] {
        &self.entries[self.live_start - self.phys_base..]
    }

    /// The entry at absolute flat index `idx`, or `None` when it is released or
    /// not yet received.
    pub(in crate::receiver) fn get(&self, idx: usize) -> Option<&FileEntry> {
        if idx < self.live_start {
            return None;
        }
        self.entries.get(idx - self.phys_base)
    }

    /// Mutable form of [`Self::get`].
    pub(in crate::receiver) fn get_mut(&mut self, idx: usize) -> Option<&mut FileEntry> {
        if idx < self.live_start {
            return None;
        }
        self.entries.get_mut(idx - self.phys_base)
    }

    /// The entry at absolute flat index 0 while nothing has been released.
    pub(in crate::receiver) fn first(&self) -> Option<&FileEntry> {
        self.get(0)
    }

    /// Mutable form of [`Self::first`].
    pub(in crate::receiver) fn first_mut(&mut self) -> Option<&mut FileEntry> {
        self.get_mut(0)
    }

    /// Appends one entry at absolute index [`Self::len`].
    pub(in crate::receiver) fn push(&mut self, entry: FileEntry) {
        self.entries.push(entry);
    }

    /// Appends entries at absolute index [`Self::len`].
    pub(in crate::receiver) fn extend<I: IntoIterator<Item = FileEntry>>(&mut self, iter: I) {
        self.entries.extend(iter);
    }

    /// Detaches every entry from absolute index `at` on.
    ///
    /// # Panics
    ///
    /// When `at` falls inside the released prefix.
    pub(in crate::receiver) fn split_off(&mut self, at: usize) -> Vec<FileEntry> {
        assert!(at >= self.live_start, "split inside the released prefix");
        self.entries.split_off(at - self.phys_base)
    }

    /// Drops every entry from absolute index `len` on.
    ///
    /// # Panics
    ///
    /// When `len` falls inside the released prefix.
    pub(in crate::receiver) fn truncate(&mut self, len: usize) {
        assert!(
            len >= self.live_start,
            "truncate inside the released prefix"
        );
        self.entries.truncate(len - self.phys_base);
    }

    /// Iterates the live entries, oldest first.
    pub(in crate::receiver) fn iter(&self) -> std::slice::Iter<'_, FileEntry> {
        self.live().iter()
    }

    /// Iterates the live entries mutably, oldest first.
    pub(in crate::receiver) fn iter_mut(&mut self) -> std::slice::IterMut<'_, FileEntry> {
        self.into_iter()
    }

    /// Iterates the live entries with their absolute flat indices.
    pub(in crate::receiver) fn iter_indexed(
        &self,
    ) -> impl DoubleEndedIterator<Item = (usize, &FileEntry)> {
        let start = self.live_start;
        self.live()
            .iter()
            .enumerate()
            .map(move |(i, e)| (start + i, e))
    }

    /// The whole list as a vector, for the passes that run on the complete
    /// initial list before anything can be released.
    ///
    /// # Panics
    ///
    /// When any entry has been released.
    pub(in crate::receiver) fn whole_mut(&mut self) -> &mut Vec<FileEntry> {
        assert_eq!(self.live_start, 0, "whole-list pass after a release");
        &mut self.entries
    }

    /// Releases every live entry below absolute index `end`.
    ///
    /// upstream: flist.c `flist_free()` via generator.c:2711 - a finished
    /// sub-list is freed once the generator has moved past it.
    pub(in crate::receiver) fn release_before(&mut self, end: usize) {
        let end = end.min(self.len());
        if end <= self.live_start {
            return;
        }
        let lo = self.live_start - self.phys_base;
        let hi = end - self.phys_base;
        for entry in &mut self.entries[lo..hi] {
            entry.reclaim_heap_data();
        }
        self.live_start = end;
        let released = hi;
        if released >= MIN_COMPACT && released >= self.entries.len() - released {
            self.entries.drain(..released);
            self.phys_base = end;
            if self.entries.capacity() > 4 * self.entries.len().max(MIN_COMPACT) {
                self.entries
                    .shrink_to(2 * self.entries.len().max(MIN_COMPACT));
            }
        }
    }
}

impl From<Vec<FileEntry>> for FileListWindow {
    fn from(entries: Vec<FileEntry>) -> Self {
        Self {
            phys_base: 0,
            live_start: 0,
            entries,
        }
    }
}

impl FromIterator<FileEntry> for FileListWindow {
    fn from_iter<I: IntoIterator<Item = FileEntry>>(iter: I) -> Self {
        Vec::from_iter(iter).into()
    }
}

impl Index<usize> for FileListWindow {
    type Output = FileEntry;

    fn index(&self, idx: usize) -> &FileEntry {
        assert!(idx >= self.live_start, "flat index {idx} was released");
        &self.entries[idx - self.phys_base]
    }
}

impl IndexMut<usize> for FileListWindow {
    fn index_mut(&mut self, idx: usize) -> &mut FileEntry {
        assert!(idx >= self.live_start, "flat index {idx} was released");
        &mut self.entries[idx - self.phys_base]
    }
}

impl FileListWindow {
    fn physical(&self, range: Range<usize>) -> Range<usize> {
        assert!(
            range.start >= self.live_start,
            "flat range {range:?} reaches into the released prefix"
        );
        range.start - self.phys_base..range.end - self.phys_base
    }
}

impl Index<Range<usize>> for FileListWindow {
    type Output = [FileEntry];

    fn index(&self, range: Range<usize>) -> &[FileEntry] {
        let r = self.physical(range);
        &self.entries[r]
    }
}

impl IndexMut<Range<usize>> for FileListWindow {
    fn index_mut(&mut self, range: Range<usize>) -> &mut [FileEntry] {
        let r = self.physical(range);
        &mut self.entries[r]
    }
}

impl Index<RangeFrom<usize>> for FileListWindow {
    type Output = [FileEntry];

    fn index(&self, range: RangeFrom<usize>) -> &[FileEntry] {
        &self[range.start..self.len()]
    }
}

impl IndexMut<RangeFrom<usize>> for FileListWindow {
    fn index_mut(&mut self, range: RangeFrom<usize>) -> &mut [FileEntry] {
        let end = self.len();
        &mut self[range.start..end]
    }
}

impl Index<RangeTo<usize>> for FileListWindow {
    type Output = [FileEntry];

    fn index(&self, range: RangeTo<usize>) -> &[FileEntry] {
        &self[self.live_start..range.end]
    }
}

impl<'a> IntoIterator for &'a FileListWindow {
    type Item = &'a FileEntry;
    type IntoIter = std::slice::Iter<'a, FileEntry>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl<'a> IntoIterator for &'a mut FileListWindow {
    type Item = &'a mut FileEntry;
    type IntoIter = std::slice::IterMut<'a, FileEntry>;

    fn into_iter(self) -> Self::IntoIter {
        let lo = self.live_start - self.phys_base;
        self.entries[lo..].iter_mut()
    }
}

#[cfg(test)]
mod tests {
    use super::{FileListWindow, MIN_COMPACT};
    use protocol::flist::FileEntry;

    fn window(n: usize) -> FileListWindow {
        (0..n)
            .map(|i| FileEntry::new_file(format!("f{i}").into(), i as u64, 0o644))
            .collect::<Vec<_>>()
            .into()
    }

    #[test]
    fn indices_stay_absolute_across_a_release() {
        // The whole receiver addresses entries by wire-derived flat index; a
        // release must not shift the index of any live entry.
        let mut w = window(10);
        w.release_before(4);
        assert_eq!(w.len(), 10);
        assert_eq!(w.live_start(), 4);
        assert_eq!(w[4].size(), 4);
        assert_eq!(w[9].size(), 9);
        assert_eq!(w[5..7].len(), 2);
        assert_eq!(w[7..][0].size(), 7);
        let idx: Vec<usize> = w.iter_indexed().map(|(i, _)| i).collect();
        assert_eq!(idx, (4..10).collect::<Vec<_>>());
    }

    #[test]
    fn a_released_index_is_absent_like_a_freed_flist() {
        // upstream: rsync.c:311-316 flist_for_ndx() finds no list for a freed
        // index; the window reports None rather than a stale entry.
        let mut w = window(6);
        w.release_before(3);
        assert!(w.get(2).is_none());
        assert!(w.get(3).is_some());
        assert!(w.get(6).is_none());
    }

    #[test]
    #[should_panic(expected = "was released")]
    fn indexing_a_released_entry_fails_loudly() {
        let mut w = window(3);
        w.release_before(2);
        let _ = &w[1];
    }

    #[test]
    fn compaction_keeps_indices_and_bounds_storage() {
        // Releasing most of a large list must return the backing storage, or
        // resident memory would still grow with the tree size.
        let n = 4 * MIN_COMPACT;
        let mut w = window(n);
        w.release_before(n - 10);
        assert_eq!(w.len(), n);
        assert_eq!(w.live().len(), 10);
        assert_eq!(w[n - 1].size(), (n - 1) as u64);
        assert!(
            w.entries.len() <= MIN_COMPACT,
            "released prefix was compacted"
        );
        w.push(FileEntry::new_file("tail".into(), 7, 0o644));
        assert_eq!(w[n].size(), 7);
    }

    #[test]
    fn releases_are_idempotent_and_monotonic() {
        let mut w = window(8);
        w.release_before(5);
        w.release_before(3);
        assert_eq!(w.live_start(), 5);
        w.release_before(100);
        assert_eq!(w.live_start(), 8);
        assert!(w.live().is_empty());
    }
}
