//! Pipeline dispatch types for the async transfer architecture.
//!
//! This module defines [`FileList`] (immutable, sorted file list shared via `Arc`)
//! and [`FileJob`] (a unit of work dispatched through a bounded channel with
//! backpressure). Together they implement the two-phase pipeline:
//!
//! **Phase 1 — File List**: Build, sort, and freeze the file list into an `Arc`.
//! The NDX (0-based index into the sorted vector) remains stable and is used as
//! the protocol index for all subsequent wire communication.
//!
//! **Phase 2 — Transfer Dispatch**: A producer iterates the file list creating
//! `FileJob` values, a consumer pulls them in FIFO order and executes transfers.
//! Bounded channel capacity provides natural backpressure.

use std::path::PathBuf;
use std::sync::Arc;

use protocol::flist::FileEntry;

/// Maximum number of retry attempts per file before giving up.
///
/// Matches upstream rsync's redo limit (2 retries = 3 total attempts).
pub const MAX_RETRY_COUNT: u8 = 2;

/// Immutable, sorted file list shared between producer and consumer tasks.
///
/// Wraps a `Vec<FileEntry>` in `Arc` for zero-copy sharing across tokio tasks.
/// After construction (receiving from wire + sorting), the list is frozen — no
/// entries are added, removed, or reordered. NDX indices are the `Vec` positions,
/// which remain stable after sort.
///
/// # Protocol Invariant
///
/// The rsync protocol uses delta-encoded NDX values that depend on the file list
/// being identically sorted on both sides. `FileList::new()` takes a pre-sorted
/// vector; callers must sort before constructing.
#[derive(Debug, Clone)]
pub struct FileList {
    entries: Arc<Vec<FileEntry>>,
}

impl FileList {
    /// Creates a new `FileList` from a pre-sorted vector of entries.
    ///
    /// The caller is responsible for sorting entries using `protocol::flist::sort_file_list()`
    /// or equivalent before constructing. The NDX for each entry is its index in the vector.
    #[must_use]
    pub fn new(entries: Vec<FileEntry>) -> Self {
        Self {
            entries: Arc::new(entries),
        }
    }

    /// Returns the entry at the given NDX (0-based protocol index).
    #[must_use]
    pub fn get(&self, ndx: u32) -> Option<&FileEntry> {
        self.entries.get(ndx as usize)
    }

    /// Returns the number of entries in the file list.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns `true` if the file list contains no entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Returns a slice of all entries.
    #[must_use]
    pub fn entries(&self) -> &[FileEntry] {
        &self.entries
    }

    /// Returns the inner `Arc` for sharing with tasks.
    #[must_use]
    pub fn shared(&self) -> Arc<Vec<FileEntry>> {
        Arc::clone(&self.entries)
    }
}

/// Transfer control flags for a [`FileJob`].
#[derive(Debug, Clone, Copy, Default)]
pub struct TransferFlags {
    /// Whether this is a redo/retry attempt.
    pub is_retry: bool,
    /// Retry attempt number (0 = first attempt).
    pub retry_count: u8,
}

/// A unit of work for the transfer pipeline.
///
/// Produced by iterating the [`FileList`] and consumed by the transfer executor.
/// Contains everything needed to initiate one file's transfer: the protocol index
/// (NDX), a reference to the file entry, and control flags.
///
/// `FileJob` flows through a bounded `tokio::sync::mpsc` channel from the producer
/// task to the consumer task. Channel backpressure naturally throttles the producer
/// when the consumer falls behind.
#[derive(Debug, Clone)]
pub struct FileJob {
    /// Protocol index (position in the sorted file list). Stable after sort.
    ndx: u32,
    /// Destination path for this file (dest_dir joined with entry path).
    dest_path: PathBuf,
    /// Reference to the file entry from the shared file list.
    entry: Arc<FileEntry>,
    /// Transfer control flags (retry state).
    flags: TransferFlags,
}

impl FileJob {
    /// Creates a new file job for initial transfer.
    #[must_use]
    pub fn new(ndx: u32, dest_path: PathBuf, entry: Arc<FileEntry>) -> Self {
        Self {
            ndx,
            dest_path,
            entry,
            flags: TransferFlags::default(),
        }
    }

    /// Creates a retry job from this job, incrementing the retry count.
    ///
    /// Returns `None` if the maximum retry count has been reached.
    #[must_use]
    pub fn retry(mut self) -> Option<Self> {
        if self.flags.retry_count >= MAX_RETRY_COUNT {
            return None;
        }
        self.flags.retry_count += 1;
        self.flags.is_retry = true;
        Some(self)
    }

    /// Returns the protocol NDX (0-based index into the sorted file list).
    #[must_use]
    pub const fn ndx(&self) -> u32 {
        self.ndx
    }

    /// Returns the destination path for this file.
    #[must_use]
    pub fn dest_path(&self) -> &std::path::Path {
        &self.dest_path
    }

    /// Returns a reference to the file entry.
    #[must_use]
    pub fn entry(&self) -> &FileEntry {
        &self.entry
    }

    /// Returns the shared `Arc` reference to the file entry.
    #[must_use]
    pub fn entry_arc(&self) -> &Arc<FileEntry> {
        &self.entry
    }

    /// Returns the transfer flags.
    #[must_use]
    pub const fn flags(&self) -> &TransferFlags {
        &self.flags
    }

    /// Returns `true` if this is a retry attempt.
    #[must_use]
    pub const fn is_retry(&self) -> bool {
        self.flags.is_retry
    }

    /// Returns the retry count (0 = first attempt).
    #[must_use]
    pub const fn retry_count(&self) -> u8 {
        self.flags.retry_count
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_test_entry(name: &str, size: u64) -> FileEntry {
        FileEntry::new_file(name.into(), size, 0o644)
    }

    // ==================== FileList tests ====================

    #[test]
    fn file_list_new_empty() {
        let list = FileList::new(Vec::new());
        assert!(list.is_empty());
        assert_eq!(list.len(), 0);
        assert!(list.get(0).is_none());
    }

    #[test]
    fn file_list_new_populated() {
        let entries = vec![
            make_test_entry("aaa", 100),
            make_test_entry("bbb", 200),
            make_test_entry("ccc", 300),
        ];
        let list = FileList::new(entries);
        assert_eq!(list.len(), 3);
        assert!(!list.is_empty());
    }

    #[test]
    fn file_list_get_valid_ndx() {
        let entries = vec![make_test_entry("first", 10), make_test_entry("second", 20)];
        let list = FileList::new(entries);
        assert_eq!(list.get(0).unwrap().size(), 10);
        assert_eq!(list.get(1).unwrap().size(), 20);
    }

    #[test]
    fn file_list_get_invalid_ndx() {
        let list = FileList::new(vec![make_test_entry("only", 42)]);
        assert!(list.get(1).is_none());
        assert!(list.get(100).is_none());
        assert!(list.get(u32::MAX).is_none());
    }

    #[test]
    fn file_list_entries_slice() {
        let entries = vec![make_test_entry("a", 1), make_test_entry("b", 2)];
        let list = FileList::new(entries);
        let slice = list.entries();
        assert_eq!(slice.len(), 2);
        assert_eq!(slice[0].size(), 1);
        assert_eq!(slice[1].size(), 2);
    }

    #[test]
    fn file_list_shared_returns_arc() {
        let entries = vec![make_test_entry("x", 99)];
        let list = FileList::new(entries);
        let arc1 = list.shared();
        let arc2 = list.shared();
        // Both Arcs point to the same allocation
        assert!(Arc::ptr_eq(&arc1, &arc2));
    }

    #[test]
    fn file_list_clone_shares_data() {
        let list1 = FileList::new(vec![make_test_entry("shared", 50)]);
        let list2 = list1.clone();
        // Cloned FileList shares the same underlying data via Arc
        assert!(Arc::ptr_eq(&list1.shared(), &list2.shared()));
    }

    // ==================== FileJob tests ====================

    #[test]
    fn file_job_new() {
        let entry = Arc::new(make_test_entry("test.txt", 1024));
        let job = FileJob::new(42, PathBuf::from("/dest/test.txt"), Arc::clone(&entry));
        assert_eq!(job.ndx(), 42);
        assert_eq!(job.dest_path(), std::path::Path::new("/dest/test.txt"));
        assert_eq!(job.entry().size(), 1024);
        assert!(!job.is_retry());
        assert_eq!(job.retry_count(), 0);
    }

    #[test]
    fn file_job_retry_increments_count() {
        let entry = Arc::new(make_test_entry("retry.txt", 512));
        let job = FileJob::new(0, PathBuf::from("/dest/retry.txt"), entry);

        let job = job.retry().expect("first retry should succeed");
        assert!(job.is_retry());
        assert_eq!(job.retry_count(), 1);
        assert_eq!(job.ndx(), 0);
    }

    #[test]
    fn file_job_retry_twice() {
        let entry = Arc::new(make_test_entry("twice.txt", 256));
        let job = FileJob::new(5, PathBuf::from("/dest/twice.txt"), entry);

        let job = job.retry().expect("first retry");
        assert_eq!(job.retry_count(), 1);

        let job = job.retry().expect("second retry");
        assert_eq!(job.retry_count(), 2);
    }

    #[test]
    fn file_job_retry_returns_none_at_max() {
        let entry = Arc::new(make_test_entry("max.txt", 128));
        let job = FileJob::new(0, PathBuf::from("/dest/max.txt"), entry);

        let job = job.retry().unwrap(); // count = 1
        let job = job.retry().unwrap(); // count = 2
        assert!(job.retry().is_none()); // count = 2 >= MAX_RETRY_COUNT
    }

    #[test]
    fn file_job_entry_arc_returns_shared_ref() {
        let entry = Arc::new(make_test_entry("arc.txt", 64));
        let job = FileJob::new(0, PathBuf::from("/dest/arc.txt"), Arc::clone(&entry));
        assert!(Arc::ptr_eq(job.entry_arc(), &entry));
    }

    // ==================== TransferFlags tests ====================

    #[test]
    fn transfer_flags_default() {
        let flags = TransferFlags::default();
        assert!(!flags.is_retry);
        assert_eq!(flags.retry_count, 0);
    }
}
