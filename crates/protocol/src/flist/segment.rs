//! Segmented file list for incremental recursion.
//!
//! When `INC_RECURSE` is negotiated, the file list is sent as multiple segments
//! (sub-lists), one per directory. Each segment has its own NDX range.
//!
//! # Upstream Reference
//!
//! - `flist.c:flist_new()` — allocates sub-lists with `ndx_start` offsets
//! - `flist.c:send_extra_file_list()` — sends one segment per directory
//! - `rsync.h:285-288` — NDX_FLIST_OFFSET constant

use super::FileEntry;

/// A single file list segment corresponding to one directory's contents.
///
/// Mirrors upstream's `struct file_list` in incremental mode, where each
/// directory gets its own file list with a distinct NDX range.
#[derive(Debug, Clone)]
pub struct FileListSegment {
    /// Global NDX of the first entry in this segment.
    ///
    /// Upstream: `flist->ndx_start`. First segment starts at 1 (with INC_RECURSE),
    /// each subsequent: `prev.ndx_start + prev.used + 1`.
    pub ndx_start: i32,
    /// Global NDX of the parent directory (in the dir_flist).
    /// -1 for the initial (root) segment.
    pub parent_dir_ndx: i32,
    /// File entries in this segment.
    pub entries: Vec<FileEntry>,
}

impl FileListSegment {
    /// Creates a new segment with the given NDX start and parent.
    #[must_use]
    pub fn new(ndx_start: i32, parent_dir_ndx: i32) -> Self {
        Self {
            ndx_start,
            parent_dir_ndx,
            entries: Vec::new(),
        }
    }

    /// Returns the number of entries in this segment.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns `true` if this segment has no entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Returns the NDX value just past the last entry in this segment.
    ///
    /// Used to compute `ndx_start` for the next segment.
    #[must_use]
    pub fn ndx_end(&self) -> i32 {
        self.ndx_start + self.entries.len() as i32
    }
}

/// Collection of file list segments for incremental recursion.
///
/// Manages multiple segments with non-overlapping NDX ranges and provides
/// global NDX lookup across all segments.
#[derive(Debug)]
pub struct SegmentedFileList {
    /// Ordered segments (by ndx_start).
    segments: Vec<FileListSegment>,
    /// Total number of entries across all segments.
    total_entries: usize,
    /// Whether all file lists have been received (NDX_FLIST_EOF seen).
    flist_eof: bool,
}

impl SegmentedFileList {
    /// Creates a new empty segmented file list.
    #[must_use]
    pub fn new() -> Self {
        Self {
            segments: Vec::new(),
            total_entries: 0,
            flist_eof: false,
        }
    }

    /// Adds a segment to the collection.
    pub fn add_segment(&mut self, segment: FileListSegment) {
        self.total_entries += segment.len();
        self.segments.push(segment);
    }

    /// Returns the total number of entries across all segments.
    #[must_use]
    pub fn total_entries(&self) -> usize {
        self.total_entries
    }

    /// Returns the number of segments.
    #[must_use]
    pub fn segment_count(&self) -> usize {
        self.segments.len()
    }

    /// Marks the file list as complete (NDX_FLIST_EOF received).
    pub fn set_flist_eof(&mut self) {
        self.flist_eof = true;
    }

    /// Returns `true` when all file lists have been received.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.flist_eof
    }

    /// Computes the `ndx_start` for the next segment.
    ///
    /// Upstream: `cur_flist->ndx_start + cur_flist->used` (flist.c recv_file_list).
    /// No gap between segments — each starts immediately after the previous.
    #[must_use]
    pub fn next_ndx_start(&self) -> i32 {
        if let Some(last) = self.segments.last() {
            last.ndx_start + last.len() as i32
        } else {
            0
        }
    }

    /// Returns a reference to the segments.
    #[must_use]
    pub fn segments(&self) -> &[FileListSegment] {
        &self.segments
    }

    /// Looks up a file entry by global NDX.
    ///
    /// Returns `None` if the NDX doesn't fall within any segment.
    #[must_use]
    pub fn get_by_ndx(&self, ndx: i32) -> Option<&FileEntry> {
        for segment in &self.segments {
            let offset = ndx - segment.ndx_start;
            if offset >= 0 && (offset as usize) < segment.len() {
                return Some(&segment.entries[offset as usize]);
            }
        }
        None
    }

    /// Collects all entries across all segments into a flat Vec.
    ///
    /// Useful for converting a segmented file list back to a flat list
    /// for use with existing code that expects `Vec<FileEntry>`.
    #[must_use]
    pub fn flatten(&self) -> Vec<FileEntry> {
        let mut result = Vec::with_capacity(self.total_entries);
        for segment in &self.segments {
            result.extend_from_slice(&segment.entries);
        }
        result
    }
}

impl Default for SegmentedFileList {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_entry(name: &str, size: u64) -> FileEntry {
        FileEntry::new_file(name.into(), size, 0o644)
    }

    #[test]
    fn empty_segmented_list() {
        let sfl = SegmentedFileList::new();
        assert_eq!(sfl.total_entries(), 0);
        assert_eq!(sfl.segment_count(), 0);
        assert!(!sfl.is_complete());
        assert_eq!(sfl.next_ndx_start(), 0);
    }

    #[test]
    fn add_segments_and_lookup() {
        let mut sfl = SegmentedFileList::new();

        let mut seg1 = FileListSegment::new(0, -1);
        seg1.entries.push(test_entry("file1.txt", 100));
        seg1.entries.push(test_entry("file2.txt", 200));
        sfl.add_segment(seg1);

        assert_eq!(sfl.total_entries(), 2);
        assert_eq!(sfl.next_ndx_start(), 2); // 0 + 2, no gap

        let mut seg2 = FileListSegment::new(2, 0);
        seg2.entries.push(test_entry("subdir/a.txt", 300));
        sfl.add_segment(seg2);

        assert_eq!(sfl.total_entries(), 3);
        assert_eq!(sfl.segment_count(), 2);

        // Lookup by NDX — contiguous ranges, no gaps
        assert_eq!(sfl.get_by_ndx(0).unwrap().name(), "file1.txt");
        assert_eq!(sfl.get_by_ndx(1).unwrap().name(), "file2.txt");
        assert_eq!(sfl.get_by_ndx(2).unwrap().name(), "subdir/a.txt");
        assert!(sfl.get_by_ndx(3).is_none());
    }

    #[test]
    fn flatten_preserves_order() {
        let mut sfl = SegmentedFileList::new();

        let mut seg1 = FileListSegment::new(0, -1);
        seg1.entries.push(test_entry("a.txt", 100));
        sfl.add_segment(seg1);

        let mut seg2 = FileListSegment::new(1, 0);
        seg2.entries.push(test_entry("b.txt", 200));
        seg2.entries.push(test_entry("c.txt", 300));
        sfl.add_segment(seg2);

        let flat = sfl.flatten();
        assert_eq!(flat.len(), 3);
        assert_eq!(flat[0].name(), "a.txt");
        assert_eq!(flat[1].name(), "b.txt");
        assert_eq!(flat[2].name(), "c.txt");
    }

    #[test]
    fn flist_eof() {
        let mut sfl = SegmentedFileList::new();
        assert!(!sfl.is_complete());
        sfl.set_flist_eof();
        assert!(sfl.is_complete());
    }

    #[test]
    fn segment_ndx_end() {
        let mut seg = FileListSegment::new(5, 0);
        assert_eq!(seg.ndx_end(), 5);
        seg.entries.push(test_entry("x.txt", 100));
        seg.entries.push(test_entry("y.txt", 200));
        assert_eq!(seg.ndx_end(), 7);
    }
}
