//! Incremental file list processing for rsync protocol.
//!
//! This module provides streaming access to file list entries as they arrive from
//! the sender, rather than waiting for the complete list before processing. This
//! significantly reduces startup latency for large directory transfers.
//!
//! # Benefits
//!
//! - Reduced startup delay: Transfers can begin as soon as first entries arrive
//! - Lower memory peak: Don't need to hold entire list in memory before starting
//! - Better progress feedback: Users see activity immediately
//!
//! # Usage
//!
//! ```ignore
//! use protocol::flist::IncrementalFileList;
//!
//! let mut incremental = IncrementalFileList::new();
//!
//! // Feed entries as they arrive from the wire
//! while let Some(entry) = reader.read_entry(&mut stream)? {
//!     // Entry is immediately ready for processing if its parent exists
//!     if let Some(ready_entry) = incremental.push(entry) {
//!         process_entry(ready_entry);
//!     }
//!     // Process any entries that became ready due to directory creation
//!     for ready in incremental.drain_ready() {
//!         process_entry(ready);
//!     }
//! }
//! ```
//!
//! # Dependency Tracking
//!
//! The incremental processor tracks parent directory dependencies. An entry is
//! only yielded when its parent directory has been processed, ensuring:
//!
//! 1. Directories are created before their contents
//! 2. Nested directories are created in order
//! 3. Files can be transferred immediately once their parent exists
//!
//! # Upstream Reference
//!
//! - `flist.c:recv_file_list()` - Traditional batch receiving
//! - `io.c:read_a_msg()` - Incremental file list segments in INC_RECURSE mode

use std::collections::{HashMap, HashSet, VecDeque};

use super::FileEntry;

/// Incremental file list that yields entries as soon as their dependencies are met.
///
/// Tracks which directories have been created (or are ready to be created) and
/// yields file entries only when their parent directories are available.
#[derive(Debug)]
pub struct IncrementalFileList {
    /// Entries ready to be processed (dependencies satisfied).
    ready: VecDeque<FileEntry>,
    /// Entries waiting for their parent directory to be created.
    /// Key: parent path (normalized), Value: list of waiting entries.
    pending: HashMap<String, Vec<FileEntry>>,
    /// Set of directories that have been yielded (created).
    created_dirs: HashSet<String>,
    /// Number of entries yielded so far.
    entries_yielded: usize,
    /// Number of entries still pending.
    entries_pending: usize,
    /// Whether we're in incremental recursion mode.
    incremental_recursion: bool,
}

impl Default for IncrementalFileList {
    fn default() -> Self {
        Self::new()
    }
}

impl IncrementalFileList {
    /// Creates a new incremental file list processor.
    #[must_use]
    pub fn new() -> Self {
        // Root directory (".") is implicitly available
        let mut created_dirs = HashSet::new();
        created_dirs.insert(String::new());
        created_dirs.insert(".".to_string());

        Self {
            ready: VecDeque::new(),
            pending: HashMap::new(),
            created_dirs,
            entries_yielded: 0,
            entries_pending: 0,
            incremental_recursion: false,
        }
    }

    /// Creates a new incremental file list processor with incremental recursion mode.
    ///
    /// In incremental recursion mode (`--inc-recursive`), the sender transmits
    /// file lists in segments as directories are traversed. This mode is compatible
    /// with rsync's `INC_RECURSE` compatibility flag.
    #[must_use]
    pub fn with_incremental_recursion() -> Self {
        let mut list = Self::new();
        list.incremental_recursion = true;
        list
    }

    /// Returns whether incremental recursion mode is enabled.
    #[must_use]
    pub const fn is_incremental_recursion(&self) -> bool {
        self.incremental_recursion
    }

    /// Pushes a new entry into the incremental list.
    ///
    /// If the entry's parent directory is already created, the entry is added
    /// to the ready queue. Otherwise, it's held in the pending queue until its
    /// parent becomes available.
    ///
    /// # Returns
    ///
    /// Returns `true` if the entry was immediately ready (parent exists),
    /// `false` if it was queued for later.
    pub fn push(&mut self, entry: FileEntry) -> bool {
        let parent = Self::parent_path(entry.name());

        // Check if parent directory exists
        if self.created_dirs.contains(&parent) {
            // If this is a directory, mark it as created so children can proceed
            if entry.is_dir() {
                self.created_dirs.insert(entry.name().to_string());
                // Check if any pending entries are now unblocked
                self.release_pending_children(entry.name());
            }
            self.ready.push_back(entry);
            true
        } else {
            // Parent doesn't exist yet - queue for later
            self.entries_pending += 1;
            self.pending.entry(parent).or_default().push(entry);
            false
        }
    }

    /// Returns the parent directory path for a given entry path.
    fn parent_path(name: &str) -> String {
        if name == "." || name.is_empty() {
            return String::new();
        }

        // Find last path separator
        match name.rfind('/') {
            Some(pos) if pos > 0 => name[..pos].to_string(),
            Some(0) => ".".to_string(), // e.g., "/file" -> root
            None => ".".to_string(),    // e.g., "file" -> current dir
            Some(_) => ".".to_string(),
        }
    }

    /// Releases any pending entries whose parent directory was just created.
    fn release_pending_children(&mut self, dir_name: &str) {
        if let Some(children) = self.pending.remove(dir_name) {
            let released_count = children.len();
            for child in children {
                // If this is a directory, mark it as created
                if child.is_dir() {
                    self.created_dirs.insert(child.name().to_string());
                    // Recursively release any children waiting on this directory
                    self.release_pending_children(child.name());
                }
                self.ready.push_back(child);
            }
            self.entries_pending = self.entries_pending.saturating_sub(released_count);
        }
    }

    /// Pops the next ready entry from the queue.
    ///
    /// This removes and returns an entry that is ready for processing.
    /// The entry's parent directory is guaranteed to have been yielded previously
    /// (or is the root directory).
    pub fn pop(&mut self) -> Option<FileEntry> {
        self.ready.pop_front().map(|entry| {
            self.entries_yielded += 1;
            entry
        })
    }

    /// Returns a reference to the next ready entry without removing it.
    #[must_use]
    pub fn peek(&self) -> Option<&FileEntry> {
        self.ready.front()
    }

    /// Returns the number of entries currently ready for processing.
    #[must_use]
    pub fn ready_count(&self) -> usize {
        self.ready.len()
    }

    /// Returns the number of entries waiting for their parent directory.
    #[must_use]
    pub fn pending_count(&self) -> usize {
        self.entries_pending
    }

    /// Returns the total number of entries yielded so far.
    #[must_use]
    pub const fn entries_yielded(&self) -> usize {
        self.entries_yielded
    }

    /// Returns `true` if there are no ready entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.ready.is_empty()
    }

    /// Returns `true` if there are entries waiting for their parent directory.
    #[must_use]
    pub fn has_pending(&self) -> bool {
        self.entries_pending > 0
    }

    /// Drains all ready entries into a vector.
    ///
    /// This is useful for batch processing ready entries.
    pub fn drain_ready(&mut self) -> Vec<FileEntry> {
        let count = self.ready.len();
        self.entries_yielded += count;
        self.ready.drain(..).collect()
    }

    /// Marks a directory as created externally.
    ///
    /// This is useful when directories are created outside the incremental
    /// processing flow (e.g., destination directories that already exist).
    pub fn mark_directory_created(&mut self, path: &str) {
        self.created_dirs.insert(path.to_string());
        self.release_pending_children(path);
    }

    /// Finishes incremental processing and returns any remaining pending entries.
    ///
    /// This should be called after all entries have been pushed. It handles edge
    /// cases where entries might be orphaned (parent directory was not in the list).
    ///
    /// # Returns
    ///
    /// Returns a vector of entries that could not be processed due to missing
    /// parent directories. In normal operation, this should be empty.
    pub fn finish(mut self) -> Vec<FileEntry> {
        // Drain any remaining ready entries (caller should have processed these)
        self.ready.clear();

        // Collect all orphaned entries
        let mut orphans = Vec::new();
        for (_, entries) in self.pending.drain() {
            orphans.extend(entries);
        }
        orphans
    }

    /// Creates an iterator that yields entries as they become ready.
    ///
    /// This is a convenience method for consuming the incremental list.
    #[must_use]
    pub fn into_iter(self) -> IncrementalFileListIter {
        IncrementalFileListIter { inner: self }
    }
}

/// Iterator over incrementally available file entries.
pub struct IncrementalFileListIter {
    inner: IncrementalFileList,
}

impl Iterator for IncrementalFileListIter {
    type Item = FileEntry;

    fn next(&mut self) -> Option<Self::Item> {
        self.inner.pop()
    }
}

/// Builder for configuring incremental file list processing.
#[derive(Debug, Clone)]
pub struct IncrementalFileListBuilder {
    incremental_recursion: bool,
    pre_created_dirs: Vec<String>,
}

impl Default for IncrementalFileListBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl IncrementalFileListBuilder {
    /// Creates a new builder with default settings.
    #[must_use]
    pub fn new() -> Self {
        Self {
            incremental_recursion: false,
            pre_created_dirs: Vec::new(),
        }
    }

    /// Enables incremental recursion mode.
    #[must_use]
    pub fn incremental_recursion(mut self, enabled: bool) -> Self {
        self.incremental_recursion = enabled;
        self
    }

    /// Adds a directory that already exists (no need to create).
    #[must_use]
    pub fn pre_created_dir<S: Into<String>>(mut self, path: S) -> Self {
        self.pre_created_dirs.push(path.into());
        self
    }

    /// Adds multiple directories that already exist.
    #[must_use]
    pub fn pre_created_dirs<I, S>(mut self, paths: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.pre_created_dirs
            .extend(paths.into_iter().map(Into::into));
        self
    }

    /// Builds the incremental file list.
    #[must_use]
    pub fn build(self) -> IncrementalFileList {
        let mut list = if self.incremental_recursion {
            IncrementalFileList::with_incremental_recursion()
        } else {
            IncrementalFileList::new()
        };

        for dir in self.pre_created_dirs {
            list.mark_directory_created(&dir);
        }

        list
    }
}

/// Streaming file list reader that yields entries incrementally.
///
/// This wraps a [`FileListReader`] and provides an iterator-like interface
/// that yields entries as they are read from the wire, with dependency tracking.
///
/// [`FileListReader`]: super::FileListReader
#[derive(Debug)]
pub struct StreamingFileList<R> {
    reader: super::FileListReader,
    source: R,
    incremental: IncrementalFileList,
    finished_reading: bool,
}

impl<R: std::io::Read> StreamingFileList<R> {
    /// Creates a new streaming file list reader.
    pub fn new(reader: super::FileListReader, source: R) -> Self {
        Self {
            reader,
            source,
            incremental: IncrementalFileList::new(),
            finished_reading: false,
        }
    }

    /// Creates a new streaming file list reader with incremental recursion mode.
    pub fn with_incremental_recursion(reader: super::FileListReader, source: R) -> Self {
        Self {
            reader,
            source,
            incremental: IncrementalFileList::with_incremental_recursion(),
            finished_reading: false,
        }
    }

    /// Reads the next batch of entries from the wire.
    ///
    /// This reads entries until either:
    /// - An entry becomes ready for processing
    /// - The end of the file list is reached
    /// - An I/O error occurs
    ///
    /// # Returns
    ///
    /// - `Ok(Some(entry))` - An entry is ready for processing
    /// - `Ok(None)` - No more entries (end of list reached)
    /// - `Err(e)` - An I/O error occurred
    pub fn next_ready(&mut self) -> std::io::Result<Option<FileEntry>> {
        // First, check if we have any ready entries
        if let Some(entry) = self.incremental.pop() {
            return Ok(Some(entry));
        }

        // If we've finished reading, there's nothing more
        if self.finished_reading {
            return Ok(None);
        }

        // Read entries until we get one that's ready or hit end of list
        loop {
            match self.reader.read_entry(&mut self.source)? {
                Some(entry) => {
                    self.incremental.push(entry);
                    // Check if this or any other entry is now ready
                    if let Some(ready) = self.incremental.pop() {
                        return Ok(Some(ready));
                    }
                    // No entry ready yet, keep reading
                }
                None => {
                    // End of file list
                    self.finished_reading = true;
                    // Return any remaining ready entry
                    return Ok(self.incremental.pop());
                }
            }
        }
    }

    /// Reads all entries from the wire and returns an iterator over ready entries.
    ///
    /// This is useful when you want to process entries in order after reading
    /// the complete list, but still want incremental dependency tracking.
    pub fn read_all(mut self) -> std::io::Result<IncrementalFileListIter> {
        while !self.finished_reading {
            match self.reader.read_entry(&mut self.source)? {
                Some(entry) => {
                    self.incremental.push(entry);
                }
                None => {
                    self.finished_reading = true;
                }
            }
        }
        Ok(self.incremental.into_iter())
    }

    /// Returns the number of entries ready for processing.
    #[must_use]
    pub fn ready_count(&self) -> usize {
        self.incremental.ready_count()
    }

    /// Returns the number of entries pending (waiting for parent).
    #[must_use]
    pub fn pending_count(&self) -> usize {
        self.incremental.pending_count()
    }

    /// Returns whether reading from the wire is complete.
    #[must_use]
    pub const fn is_finished_reading(&self) -> bool {
        self.finished_reading
    }

    /// Marks a directory as created (for external directory creation).
    pub fn mark_directory_created(&mut self, path: &str) {
        self.incremental.mark_directory_created(path);
    }

    /// Returns the file list statistics from the reader.
    #[must_use]
    pub const fn stats(&self) -> &super::FileListStats {
        self.reader.stats()
    }
}

/// Iterator adapter for streaming file list.
impl<R: std::io::Read> Iterator for StreamingFileList<R> {
    type Item = std::io::Result<FileEntry>;

    fn next(&mut self) -> Option<Self::Item> {
        match self.next_ready() {
            Ok(Some(entry)) => Some(Ok(entry)),
            Ok(None) => None,
            Err(e) => Some(Err(e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_file(name: &str) -> FileEntry {
        FileEntry::new_file(name.into(), 0, 0o644)
    }

    fn make_dir(name: &str) -> FileEntry {
        FileEntry::new_directory(name.into(), 0o755)
    }

    #[test]
    fn test_root_entries_immediately_ready() {
        let mut incremental = IncrementalFileList::new();

        // Root-level file should be immediately ready
        assert!(incremental.push(make_file("file.txt")));
        assert_eq!(incremental.ready_count(), 1);

        // Root directory should be immediately ready
        assert!(incremental.push(make_dir("subdir")));
        assert_eq!(incremental.ready_count(), 2);
    }

    #[test]
    fn test_nested_file_waits_for_parent() {
        let mut incremental = IncrementalFileList::new();

        // Nested file should not be ready (parent doesn't exist)
        assert!(!incremental.push(make_file("subdir/file.txt")));
        assert_eq!(incremental.ready_count(), 0);
        assert_eq!(incremental.pending_count(), 1);

        // Create parent directory
        assert!(incremental.push(make_dir("subdir")));
        // Now both should be ready (parent + child released)
        assert_eq!(incremental.ready_count(), 2);
        assert_eq!(incremental.pending_count(), 0);
    }

    #[test]
    fn test_deeply_nested_structure() {
        let mut incremental = IncrementalFileList::new();

        // Push entries in reverse order (child before parent)
        incremental.push(make_file("a/b/c/file.txt"));
        incremental.push(make_dir("a/b/c"));
        incremental.push(make_dir("a/b"));
        incremental.push(make_dir("a"));

        // After pushing "a", everything should cascade ready
        assert_eq!(incremental.ready_count(), 4);
        assert_eq!(incremental.pending_count(), 0);
    }

    #[test]
    fn test_pop_returns_entries_in_order() {
        let mut incremental = IncrementalFileList::new();

        incremental.push(make_dir("a"));
        incremental.push(make_file("a/file1.txt"));
        incremental.push(make_file("a/file2.txt"));

        let entry1 = incremental.pop().unwrap();
        assert_eq!(entry1.name(), "a");

        let entry2 = incremental.pop().unwrap();
        assert_eq!(entry2.name(), "a/file1.txt");

        let entry3 = incremental.pop().unwrap();
        assert_eq!(entry3.name(), "a/file2.txt");

        assert!(incremental.pop().is_none());
    }

    #[test]
    fn test_mark_directory_created() {
        let mut incremental = IncrementalFileList::new();

        // Pre-mark a directory as created
        incremental.mark_directory_created("existing");

        // File in pre-created directory should be immediately ready
        assert!(incremental.push(make_file("existing/file.txt")));
        assert_eq!(incremental.ready_count(), 1);
    }

    #[test]
    fn test_builder() {
        let incremental = IncrementalFileListBuilder::new()
            .incremental_recursion(true)
            .pre_created_dir("existing1")
            .pre_created_dir("existing2")
            .build();

        assert!(incremental.is_incremental_recursion());
    }

    #[test]
    fn test_drain_ready() {
        let mut incremental = IncrementalFileList::new();

        incremental.push(make_file("a.txt"));
        incremental.push(make_file("b.txt"));
        incremental.push(make_file("c.txt"));

        let ready = incremental.drain_ready();
        assert_eq!(ready.len(), 3);
        assert!(incremental.is_empty());
        assert_eq!(incremental.entries_yielded(), 3);
    }

    #[test]
    fn test_finish_returns_orphans() {
        let mut incremental = IncrementalFileList::new();

        // Push files with non-existent parent
        incremental.push(make_file("missing/file1.txt"));
        incremental.push(make_file("missing/file2.txt"));

        // These should be orphaned since "missing" directory was never pushed
        let orphans = incremental.finish();
        assert_eq!(orphans.len(), 2);
    }

    #[test]
    fn test_parent_path() {
        assert_eq!(IncrementalFileList::parent_path("."), "");
        assert_eq!(IncrementalFileList::parent_path(""), "");
        assert_eq!(IncrementalFileList::parent_path("file.txt"), ".");
        assert_eq!(IncrementalFileList::parent_path("dir/file.txt"), "dir");
        assert_eq!(IncrementalFileList::parent_path("a/b/c.txt"), "a/b");
    }

    #[test]
    fn test_dot_directory() {
        let mut incremental = IncrementalFileList::new();

        // "." is the root and should always be ready
        assert!(incremental.push(make_dir(".")));
        assert_eq!(incremental.ready_count(), 1);
    }

    #[test]
    fn test_entries_yielded_counter() {
        let mut incremental = IncrementalFileList::new();

        incremental.push(make_file("a.txt"));
        incremental.push(make_file("b.txt"));

        assert_eq!(incremental.entries_yielded(), 0);

        incremental.pop();
        assert_eq!(incremental.entries_yielded(), 1);

        incremental.pop();
        assert_eq!(incremental.entries_yielded(), 2);
    }

    #[test]
    fn test_peek() {
        let mut incremental = IncrementalFileList::new();

        assert!(incremental.peek().is_none());

        incremental.push(make_file("test.txt"));

        let peeked = incremental.peek().unwrap();
        assert_eq!(peeked.name(), "test.txt");

        // Peek doesn't consume
        assert_eq!(incremental.ready_count(), 1);
    }

    #[test]
    fn test_has_pending() {
        let mut incremental = IncrementalFileList::new();

        assert!(!incremental.has_pending());

        incremental.push(make_file("nonexistent/file.txt"));
        assert!(incremental.has_pending());

        incremental.push(make_dir("nonexistent"));
        assert!(!incremental.has_pending());
    }

    #[test]
    fn test_into_iter() {
        let mut incremental = IncrementalFileList::new();

        incremental.push(make_file("a.txt"));
        incremental.push(make_file("b.txt"));

        let entries: Vec<_> = incremental.into_iter().collect();
        assert_eq!(entries.len(), 2);
    }

    #[test]
    fn test_multiple_pending_directories() {
        let mut incremental = IncrementalFileList::new();

        // Create structure with multiple pending branches
        incremental.push(make_file("a/file1.txt"));
        incremental.push(make_file("b/file2.txt"));
        incremental.push(make_file("c/file3.txt"));

        assert_eq!(incremental.pending_count(), 3);
        assert_eq!(incremental.ready_count(), 0);

        // Create one parent
        incremental.push(make_dir("a"));
        assert_eq!(incremental.ready_count(), 2); // dir + file
        assert_eq!(incremental.pending_count(), 2);

        // Create another parent
        incremental.push(make_dir("b"));
        assert_eq!(incremental.ready_count(), 4);
        assert_eq!(incremental.pending_count(), 1);
    }
}
