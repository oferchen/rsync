//! Incremental file list processing for rsync protocol.
//!
//! Provides streaming access to file list entries as they arrive from the
//! sender, rather than waiting for the complete list before processing.
//! This significantly reduces startup latency for large directory transfers.
//!
//! # Architecture
//!
//! - [`IncrementalFileList`] - Dependency-tracking state machine that yields
//!   entries when their parent directories are available.
//! - [`ready_entry`] - Dispatch logic that determines what action to take for
//!   each ready entry based on type, filters, and failed directories.
//! - [`streaming`] - Wire-level reader that feeds entries into the incremental
//!   processor as they arrive from the network.
//!
//! # Usage
//!
//! ```ignore
//! use protocol::flist::{IncrementalFileList, process_ready_entry, ReadyEntryAction};
//!
//! let mut incremental = IncrementalFileList::new();
//!
//! // Feed entries as they arrive from the wire
//! while let Some(entry) = reader.read_entry(&mut stream)? {
//!     incremental.push(entry);
//!
//!     // Process any entries that became ready
//!     while let Some(ready) = incremental.pop() {
//!         let action = process_ready_entry(
//!             ready,
//!             |name, is_dir| filter_set.allows(name, is_dir),
//!             |name| failed_dirs.failed_ancestor(name).map(|s| s.to_string()),
//!         );
//!         match action {
//!             ReadyEntryAction::CreateDirectory(e) => create_dir(e),
//!             ReadyEntryAction::TransferFile(e) => transfer_file(e),
//!             ReadyEntryAction::CreateSymlink(e) => create_symlink(e),
//!             ReadyEntryAction::CreateDevice(e) => create_device(e),
//!             ReadyEntryAction::CreateSpecial(e) => create_special(e),
//!             ReadyEntryAction::SkipFiltered(_) => { /* excluded by filter */ },
//!             ReadyEntryAction::SkipFailedParent { .. } => { /* parent failed */ },
//!         }
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

mod ready_entry;
mod streaming;
#[cfg(test)]
mod tests;

use std::collections::{HashMap, HashSet, VecDeque};

use logging::debug_log;

use super::FileEntry;

pub use ready_entry::{process_ready_entries, process_ready_entry, ReadyEntryAction};
pub use streaming::StreamingFileList;

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
    /// Returns `true` if the entry was immediately ready (parent exists),
    /// `false` if it was queued for later.
    pub fn push(&mut self, entry: FileEntry) -> bool {
        let parent = Self::parent_path(entry.name());

        if self.created_dirs.contains(&parent) {
            if entry.is_dir() {
                self.created_dirs.insert(entry.name().to_string());
                self.release_pending_children(entry.name());
            }
            self.ready.push_back(entry);
            true
        } else {
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
                if child.is_dir() {
                    self.created_dirs.insert(child.name().to_string());
                    self.release_pending_children(child.name());
                }
                self.ready.push_back(child);
            }
            self.entries_pending = self.entries_pending.saturating_sub(released_count);
        }
    }

    /// Pops the next ready entry from the queue.
    ///
    /// The entry's parent directory is guaranteed to have been yielded previously
    /// (or is the root directory).
    pub fn pop(&mut self) -> Option<FileEntry> {
        self.ready.pop_front().inspect(|_entry| {
            self.entries_yielded += 1;
        })
    }

    /// Returns a reference to the next ready entry without removing it.
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
    pub fn drain_ready(&mut self) -> Vec<FileEntry> {
        let count = self.ready.len();
        self.entries_yielded += count;
        self.ready.drain(..).collect()
    }

    /// Marks a directory as created externally.
    ///
    /// Useful when directories are created outside the incremental processing
    /// flow (e.g., destination directories that already exist).
    pub fn mark_directory_created(&mut self, path: &str) {
        self.created_dirs.insert(path.to_string());
        self.release_pending_children(path);
    }

    /// Finishes incremental processing and returns any remaining pending entries.
    ///
    /// Should be called after all entries have been pushed. Returns entries
    /// that could not be processed due to missing parent directories.
    /// In normal operation, this should be empty.
    pub fn finish(mut self) -> Vec<FileEntry> {
        self.ready.clear();

        let mut orphans = Vec::new();
        for (_, entries) in self.pending.drain() {
            orphans.extend(entries);
        }
        orphans
    }

    /// Synthesizes a placeholder directory and releases its pending children.
    ///
    /// Returns 1 (the number of placeholders created) for counting.
    fn synthesize_placeholder(&mut self, dir_path: &str) -> usize {
        debug_log!(
            Flist,
            2,
            "finalize: synthesizing placeholder directory \"{}\"",
            dir_path
        );
        self.created_dirs.insert(dir_path.to_string());
        self.release_pending_children(dir_path);
        1
    }

    /// Finalizes incremental processing with full orphan resolution.
    ///
    /// Unlike [`finish()`](Self::finish), this method attempts to resolve orphaned
    /// entries by synthesizing placeholder parent directories for entries whose
    /// parents were never received. This allows as many entries as possible to be
    /// processed, even when the sender did not include all intermediate directories.
    ///
    /// The resolution strategy works bottom-up: for each orphaned entry, all missing
    /// ancestor directories up to the root are synthesized as placeholder directories
    /// (mode 0o755). This handles deeply nested orphans where multiple levels of
    /// parent directories are missing.
    ///
    /// Entries that cannot be resolved (e.g., due to invalid paths) remain as
    /// unresolved orphans in the result.
    pub fn finalize(mut self) -> FinalizationResult {
        let unprocessed_ready: Vec<FileEntry> = self.ready.drain(..).collect();
        let unprocessed_ready_count = unprocessed_ready.len();

        if self.pending.is_empty() {
            debug_log!(Flist, 2, "finalize: no orphaned entries to resolve");
            return FinalizationResult {
                resolved_entries: unprocessed_ready,
                unresolved_orphans: Vec::new(),
                stats: FinalizationStats {
                    orphans_detected: 0,
                    orphans_resolved: 0,
                    orphans_unresolved: 0,
                    placeholder_dirs_created: 0,
                    unprocessed_ready: unprocessed_ready_count,
                },
            };
        }

        let total_orphan_count: usize = self.pending.values().map(|v| v.len()).sum();
        debug_log!(
            Flist,
            2,
            "finalize: attempting to resolve {} orphaned entries across {} missing parents",
            total_orphan_count,
            self.pending.len()
        );

        // Sort missing parents by depth so shallower paths are processed first.
        let mut missing_parents: Vec<String> = self.pending.keys().cloned().collect();
        missing_parents.sort_by_key(|p| p.matches('/').count());

        let mut placeholder_count = 0usize;

        for parent_path in &missing_parents {
            if self.created_dirs.contains(parent_path.as_str()) {
                continue;
            }

            // Synthesize all missing ancestors from root downward.
            let ancestors = Self::collect_missing_ancestors(parent_path, &self.created_dirs);

            for ancestor in &ancestors {
                if !self.created_dirs.contains(ancestor.as_str()) {
                    placeholder_count += self.synthesize_placeholder(ancestor);
                }
            }

            if !self.created_dirs.contains(parent_path.as_str()) {
                placeholder_count += self.synthesize_placeholder(parent_path);
            }
        }

        let resolved_count = self.ready.len();

        let mut unresolved = Vec::new();
        for (missing_parent, entries) in self.pending.drain() {
            for entry in entries {
                debug_log!(
                    Flist,
                    1,
                    "finalize: unresolved orphan \"{}\" (missing parent: \"{}\")",
                    entry.name(),
                    missing_parent
                );
                unresolved.push(OrphanEntry {
                    entry,
                    missing_parent: missing_parent.clone(),
                });
            }
        }

        let mut resolved_entries = unprocessed_ready;
        resolved_entries.extend(self.ready.drain(..));

        let unresolved_count = unresolved.len();

        debug_log!(
            Flist,
            2,
            "finalize: resolved={}, unresolved={}, placeholders_created={}",
            resolved_count,
            unresolved_count,
            placeholder_count
        );

        FinalizationResult {
            resolved_entries,
            unresolved_orphans: unresolved,
            stats: FinalizationStats {
                orphans_detected: total_orphan_count,
                orphans_resolved: resolved_count,
                orphans_unresolved: unresolved_count,
                placeholder_dirs_created: placeholder_count,
                unprocessed_ready: unprocessed_ready_count,
            },
        }
    }

    /// Collects all ancestor paths that are missing from the created_dirs set.
    ///
    /// For path "a/b/c", returns ["a", "a/b"] if neither exists in `created_dirs`.
    /// Returns them in top-down order (shallowest first).
    fn collect_missing_ancestors(path: &str, created_dirs: &HashSet<String>) -> Vec<String> {
        let mut ancestors = Vec::new();
        let mut current = path.as_bytes();

        loop {
            match current.iter().rposition(|&b| b == b'/') {
                Some(pos) if pos > 0 => {
                    let parent = &path[..pos];
                    if !created_dirs.contains(parent) {
                        ancestors.push(parent.to_string());
                    }
                    current = &current[..pos];
                }
                _ => break,
            }
        }

        ancestors.reverse();
        ancestors
    }
}

/// An orphaned file entry that could not be resolved during finalization.
///
/// Contains the original entry along with the path of the missing parent
/// directory that prevented the entry from being processed.
#[derive(Debug, Clone)]
pub struct OrphanEntry {
    /// The file entry that was orphaned.
    pub entry: FileEntry,
    /// The parent directory path that was never received.
    pub missing_parent: String,
}

impl OrphanEntry {
    /// Returns a reference to the orphaned file entry.
    #[must_use]
    pub const fn entry(&self) -> &FileEntry {
        &self.entry
    }

    /// Returns the path of the missing parent directory.
    #[must_use]
    pub fn missing_parent(&self) -> &str {
        &self.missing_parent
    }
}

/// Statistics about the finalization process.
#[derive(Debug, Clone, Default, Eq, PartialEq)]
pub struct FinalizationStats {
    /// Total number of orphaned entries detected (entries whose parents were missing).
    pub orphans_detected: usize,
    /// Number of orphans that were resolved by synthesizing placeholder parents.
    pub orphans_resolved: usize,
    /// Number of orphans that could not be resolved.
    pub orphans_unresolved: usize,
    /// Number of placeholder directories that were synthesized during resolution.
    pub placeholder_dirs_created: usize,
    /// Number of entries that were ready but not yet consumed before finalization.
    pub unprocessed_ready: usize,
}

impl FinalizationStats {
    /// Returns true if all orphans were successfully resolved.
    #[must_use]
    pub const fn all_resolved(&self) -> bool {
        self.orphans_unresolved == 0
    }

    /// Returns true if there were no orphans at all.
    #[must_use]
    pub const fn no_orphans(&self) -> bool {
        self.orphans_detected == 0
    }
}

/// Result of finalizing an incremental file list.
///
/// Contains the entries that were resolved (either already ready or resolved
/// through placeholder parent synthesis), any unresolvable orphans, and
/// statistics about the finalization process.
#[derive(Debug)]
pub struct FinalizationResult {
    /// Entries that are resolved and ready for processing.
    ///
    /// Includes both entries that were already in the ready queue and
    /// orphaned entries that were resolved by synthesizing placeholder parents.
    pub resolved_entries: Vec<FileEntry>,
    /// Entries that could not be resolved because their parent paths are invalid
    /// or could not be synthesized.
    pub unresolved_orphans: Vec<OrphanEntry>,
    /// Statistics about the finalization process.
    pub stats: FinalizationStats,
}

impl FinalizationResult {
    /// Returns true if all entries were resolved (no orphans remain).
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.unresolved_orphans.is_empty()
    }

    /// Returns the total number of resolved entries.
    #[must_use]
    pub fn resolved_count(&self) -> usize {
        self.resolved_entries.len()
    }

    /// Returns the number of unresolved orphans.
    #[must_use]
    pub fn orphan_count(&self) -> usize {
        self.unresolved_orphans.len()
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

impl IntoIterator for IncrementalFileList {
    type Item = FileEntry;
    type IntoIter = IncrementalFileListIter;

    fn into_iter(self) -> Self::IntoIter {
        IncrementalFileListIter { inner: self }
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
