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

use std::collections::{HashMap, HashSet, VecDeque};

use logging::debug_log;

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
        self.ready.pop_front().inspect(|_entry| {
            self.entries_yielded += 1;
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
    ///
    /// # Returns
    ///
    /// Returns a [`FinalizationResult`] containing:
    /// - Resolved entries (entries released by synthesizing placeholder parents)
    /// - Unresolved orphan details (entries that could not be placed)
    /// - Statistics about the finalization process
    pub fn finalize(mut self) -> FinalizationResult {
        // First, drain any remaining ready entries - these are already resolved
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

        // Collect all missing parent paths and sort them so shallower paths
        // are processed first. This ensures that when we synthesize "a",
        // entries waiting for "a" are released before we try to synthesize "a/b".
        let mut missing_parents: Vec<String> = self.pending.keys().cloned().collect();
        missing_parents.sort_by_key(|p| p.matches('/').count());

        let mut placeholder_count = 0usize;

        for parent_path in &missing_parents {
            // Skip if this parent was already resolved by a previous iteration
            // (e.g., it was a pending directory entry that got released).
            if self.created_dirs.contains(parent_path.as_str()) {
                continue;
            }

            // Synthesize all missing ancestors from root downward.
            // For "a/b/c", we need to ensure "a" and "a/b" exist first.
            let ancestors = Self::collect_missing_ancestors(parent_path, &self.created_dirs);

            for ancestor in &ancestors {
                if !self.created_dirs.contains(ancestor.as_str()) {
                    debug_log!(
                        Flist,
                        2,
                        "finalize: synthesizing placeholder directory \"{}\"",
                        ancestor
                    );
                    self.created_dirs.insert(ancestor.clone());
                    placeholder_count += 1;

                    // Release any entries waiting for this ancestor.
                    // This may cascade via release_pending_children if any
                    // released entry is itself a directory with pending children.
                    if let Some(children) = self.pending.remove(ancestor.as_str()) {
                        let count = children.len();
                        for child in children {
                            if child.is_dir() {
                                self.created_dirs.insert(child.name().to_string());
                                self.release_pending_children(child.name());
                            }
                            self.ready.push_back(child);
                        }
                        self.entries_pending = self.entries_pending.saturating_sub(count);
                    }
                }
            }

            // Now create the parent itself if still missing
            if !self.created_dirs.contains(parent_path.as_str()) {
                debug_log!(
                    Flist,
                    2,
                    "finalize: synthesizing placeholder directory \"{}\"",
                    parent_path
                );
                self.created_dirs.insert(parent_path.clone());
                placeholder_count += 1;

                if let Some(children) = self.pending.remove(parent_path.as_str()) {
                    let count = children.len();
                    for child in children {
                        if child.is_dir() {
                            self.created_dirs.insert(child.name().to_string());
                            self.release_pending_children(child.name());
                        }
                        self.ready.push_back(child);
                    }
                    self.entries_pending = self.entries_pending.saturating_sub(count);
                }
            }
        }

        // Count resolved orphans as the total number of entries in the ready queue.
        // Since the queue was drained before the resolution loop, all entries here
        // were resolved by synthesizing placeholder parents. This correctly accounts
        // for cascading releases via release_pending_children().
        let resolved_count = self.ready.len();

        // Anything still in pending is truly unresolvable
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

        // Reverse so shallowest ancestors come first
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
    /// This includes both entries that were already in the ready queue and
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

// ============================================================================
// Ready Entry Processing
// ============================================================================

/// Action to take for a ready entry during streaming transfer.
///
/// Returned by [`process_ready_entry`] to indicate what the caller should do
/// with a file entry that has become ready for processing (i.e., its parent
/// directory dependencies are satisfied).
///
/// # Upstream Reference
///
/// This mirrors the decision logic in upstream rsync's `recv_generator()`
/// (generator.c:1450) which dispatches based on entry type and filter rules.
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum ReadyEntryAction {
    /// Create a directory at the entry's path.
    ///
    /// The entry is a directory and was not filtered out.
    CreateDirectory(FileEntry),
    /// Transfer a regular file.
    ///
    /// The entry is a regular file and was not filtered out.
    TransferFile(FileEntry),
    /// Create a symbolic link.
    ///
    /// The entry is a symlink and was not filtered out.
    CreateSymlink(FileEntry),
    /// Create a device node (block or character device).
    ///
    /// The entry is a device and was not filtered out.
    CreateDevice(FileEntry),
    /// Create a special file (FIFO or socket).
    ///
    /// The entry is a special file (FIFO/socket) and was not filtered out.
    CreateSpecial(FileEntry),
    /// Skip the entry because it was excluded by filter rules.
    ///
    /// The entry matched an exclude filter and should not be transferred.
    SkipFiltered(FileEntry),
    /// Skip the entry because a parent directory failed.
    ///
    /// The entry's ancestor directory could not be created, so this entry
    /// cannot be processed. The `String` contains the failed ancestor path.
    SkipFailedParent {
        /// The entry that was skipped.
        entry: FileEntry,
        /// The path of the failed ancestor directory.
        failed_ancestor: String,
    },
}

impl ReadyEntryAction {
    /// Returns a reference to the entry regardless of action type.
    #[must_use]
    pub fn entry(&self) -> &FileEntry {
        match self {
            Self::CreateDirectory(e)
            | Self::TransferFile(e)
            | Self::CreateSymlink(e)
            | Self::CreateDevice(e)
            | Self::CreateSpecial(e)
            | Self::SkipFiltered(e) => e,
            Self::SkipFailedParent { entry, .. } => entry,
        }
    }

    /// Consumes the action and returns the inner entry.
    #[must_use]
    pub fn into_entry(self) -> FileEntry {
        match self {
            Self::CreateDirectory(e)
            | Self::TransferFile(e)
            | Self::CreateSymlink(e)
            | Self::CreateDevice(e)
            | Self::CreateSpecial(e)
            | Self::SkipFiltered(e) => e,
            Self::SkipFailedParent { entry, .. } => entry,
        }
    }

    /// Returns `true` if this action indicates the entry should be processed
    /// (not skipped).
    #[must_use]
    pub const fn is_actionable(&self) -> bool {
        matches!(
            self,
            Self::CreateDirectory(_)
                | Self::TransferFile(_)
                | Self::CreateSymlink(_)
                | Self::CreateDevice(_)
                | Self::CreateSpecial(_)
        )
    }

    /// Returns `true` if this action indicates the entry was skipped.
    #[must_use]
    pub const fn is_skipped(&self) -> bool {
        matches!(self, Self::SkipFiltered(_) | Self::SkipFailedParent { .. })
    }
}

/// Processes a single ready entry from the incremental file list.
///
/// Determines the appropriate action for a file entry based on its type,
/// filter rules, and failed directory state. This encapsulates the core
/// dispatch logic used during streaming/incremental transfers.
///
/// # Parameters
///
/// - `entry`: The file entry to process (must have satisfied parent dependencies).
/// - `is_excluded`: A callback that returns `true` if the entry should be excluded.
///   This decouples the protocol layer from specific filter implementations.
///   The callback receives the entry's name (path) and whether it is a directory.
/// - `failed_ancestor`: An optional callback that checks whether the entry has a
///   failed ancestor directory. Returns `Some(ancestor_path)` if a failed ancestor
///   exists, `None` otherwise. The callback receives the entry's name (path).
///
/// # Returns
///
/// A [`ReadyEntryAction`] indicating what the caller should do with the entry.
///
/// # Example
///
/// ```ignore
/// use protocol::flist::{IncrementalFileList, process_ready_entry};
///
/// let mut incremental = IncrementalFileList::new();
/// incremental.push(entry);
///
/// while let Some(ready) = incremental.pop() {
///     let action = process_ready_entry(
///         ready,
///         |name, is_dir| filter_set.allows(name, is_dir),
///         |name| failed_dirs.failed_ancestor(name).map(|s| s.to_string()),
///     );
///     match action {
///         ReadyEntryAction::CreateDirectory(entry) => { /* create dir */ },
///         ReadyEntryAction::TransferFile(entry) => { /* transfer file */ },
///         ReadyEntryAction::CreateSymlink(entry) => { /* create symlink */ },
///         ReadyEntryAction::CreateDevice(entry) => { /* create device */ },
///         ReadyEntryAction::CreateSpecial(entry) => { /* create special */ },
///         ReadyEntryAction::SkipFiltered(_) => { /* filtered out */ },
///         ReadyEntryAction::SkipFailedParent { .. } => { /* parent failed */ },
///     }
/// }
/// ```
///
/// # Upstream Reference
///
/// - `generator.c:recv_generator()` - Entry dispatch by type
/// - `flist.c:recv_file_list()` - File list processing with filtering
pub fn process_ready_entry<F, G>(entry: FileEntry, is_excluded: F, failed_ancestor: G) -> ReadyEntryAction
where
    F: FnOnce(&str, bool) -> bool,
    G: FnOnce(&str) -> Option<String>,
{
    let name = entry.name();
    let is_dir = entry.is_dir();

    // Check for failed ancestor directory first (cheapest check).
    // This avoids unnecessary filter evaluation for entries that cannot
    // be processed regardless.
    if let Some(ancestor) = failed_ancestor(name) {
        debug_log!(
            Flist,
            2,
            "process_ready_entry: skipping \"{}\" (failed ancestor: \"{}\")",
            name,
            ancestor
        );
        return ReadyEntryAction::SkipFailedParent {
            entry,
            failed_ancestor: ancestor,
        };
    }

    // Check filter rules.
    // The callback returns true if the entry is excluded.
    if is_excluded(name, is_dir) {
        debug_log!(
            Flist,
            2,
            "process_ready_entry: filtering out \"{}\"",
            name
        );
        return ReadyEntryAction::SkipFiltered(entry);
    }

    // Dispatch by entry type.
    if entry.is_dir() {
        ReadyEntryAction::CreateDirectory(entry)
    } else if entry.is_file() {
        ReadyEntryAction::TransferFile(entry)
    } else if entry.is_symlink() {
        ReadyEntryAction::CreateSymlink(entry)
    } else if entry.is_device() {
        ReadyEntryAction::CreateDevice(entry)
    } else if entry.is_special() {
        ReadyEntryAction::CreateSpecial(entry)
    } else {
        // Unknown type - treat as file transfer (matches upstream fallback).
        debug_log!(
            Flist,
            1,
            "process_ready_entry: unknown type for \"{}\", treating as file",
            entry.name()
        );
        ReadyEntryAction::TransferFile(entry)
    }
}

/// Processes all currently ready entries from an incremental file list.
///
/// Convenience wrapper that drains the ready queue and processes each entry
/// through [`process_ready_entry`], collecting the results.
///
/// # Parameters
///
/// - `incremental`: The incremental file list to drain ready entries from.
/// - `is_excluded`: A callback that returns `true` if an entry should be excluded.
///   Called once per entry with `(name, is_dir)`.
/// - `failed_ancestor`: A callback that checks for failed ancestor directories.
///   Called once per entry with the entry's name.
///
/// # Returns
///
/// A vector of [`ReadyEntryAction`]s, one per ready entry.
pub fn process_ready_entries<F, G>(
    incremental: &mut IncrementalFileList,
    mut is_excluded: F,
    mut failed_ancestor: G,
) -> Vec<ReadyEntryAction>
where
    F: FnMut(&str, bool) -> bool,
    G: FnMut(&str) -> Option<String>,
{
    let ready = incremental.drain_ready();
    let mut actions = Vec::with_capacity(ready.len());
    for entry in ready {
        actions.push(process_ready_entry(entry, &mut is_excluded, &mut failed_ancestor));
    }
    actions
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

    /// Creates an iterator that yields entries as they become ready.
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

    // --- Orphan finalization tests ---

    #[test]
    fn test_finalize_no_orphans() {
        // When all entries have their parents, finalize should report no orphans
        let mut incremental = IncrementalFileList::new();
        incremental.push(make_dir("a"));
        incremental.push(make_file("a/file.txt"));
        incremental.push(make_file("root.txt"));

        // Drain ready entries as caller would
        let _ = incremental.drain_ready();

        let result = incremental.finalize();
        assert!(result.is_complete());
        assert_eq!(result.orphan_count(), 0);
        assert!(result.stats.no_orphans());
        assert!(result.stats.all_resolved());
        assert_eq!(result.stats.orphans_detected, 0);
        assert_eq!(result.stats.placeholder_dirs_created, 0);
    }

    #[test]
    fn test_finalize_resolves_single_orphan() {
        // Entry arrives before its parent directory - finalize should synthesize parent
        let mut incremental = IncrementalFileList::new();
        incremental.push(make_file("missing_dir/file.txt"));

        assert_eq!(incremental.pending_count(), 1);
        assert_eq!(incremental.ready_count(), 0);

        let result = incremental.finalize();
        assert!(result.is_complete());
        assert_eq!(result.orphan_count(), 0);
        assert_eq!(result.resolved_count(), 1);
        assert_eq!(result.stats.orphans_detected, 1);
        assert_eq!(result.stats.orphans_resolved, 1);
        assert_eq!(result.stats.orphans_unresolved, 0);
        assert_eq!(result.stats.placeholder_dirs_created, 1);

        // The resolved entry should be the file
        assert_eq!(result.resolved_entries[0].name(), "missing_dir/file.txt");
    }

    #[test]
    fn test_finalize_resolves_deeply_nested_orphan() {
        // Great-grandchild arrives without any ancestor directories
        let mut incremental = IncrementalFileList::new();
        incremental.push(make_file("a/b/c/d/deep_file.txt"));

        let result = incremental.finalize();
        assert!(result.is_complete());
        assert_eq!(result.orphan_count(), 0);
        assert_eq!(result.resolved_count(), 1);

        // Should have created placeholders for a, a/b, a/b/c, a/b/c/d
        assert_eq!(result.stats.placeholder_dirs_created, 4);
        assert_eq!(result.stats.orphans_resolved, 1);
    }

    #[test]
    fn test_finalize_multiple_orphans_same_missing_parent() {
        // Multiple orphans waiting for the same missing parent
        let mut incremental = IncrementalFileList::new();
        incremental.push(make_file("missing/file1.txt"));
        incremental.push(make_file("missing/file2.txt"));
        incremental.push(make_file("missing/file3.txt"));

        let result = incremental.finalize();
        assert!(result.is_complete());
        assert_eq!(result.orphan_count(), 0);
        assert_eq!(result.resolved_count(), 3);
        assert_eq!(result.stats.orphans_detected, 3);
        assert_eq!(result.stats.orphans_resolved, 3);
        assert_eq!(result.stats.placeholder_dirs_created, 1);

        // All three files should be resolved
        let names: Vec<&str> = result.resolved_entries.iter().map(|e| e.name()).collect();
        assert!(names.contains(&"missing/file1.txt"));
        assert!(names.contains(&"missing/file2.txt"));
        assert!(names.contains(&"missing/file3.txt"));
    }

    #[test]
    fn test_finalize_multiple_missing_parents() {
        // Orphans from different missing parents
        let mut incremental = IncrementalFileList::new();
        incremental.push(make_file("alpha/file1.txt"));
        incremental.push(make_file("beta/file2.txt"));
        incremental.push(make_file("gamma/file3.txt"));

        let result = incremental.finalize();
        assert!(result.is_complete());
        assert_eq!(result.resolved_count(), 3);
        assert_eq!(result.stats.placeholder_dirs_created, 3);
    }

    #[test]
    fn test_finalize_orphan_that_eventually_gets_parent() {
        // Orphan is created, then parent arrives later (before finalization)
        let mut incremental = IncrementalFileList::new();

        // File arrives first (orphaned)
        incremental.push(make_file("later_dir/file.txt"));
        assert_eq!(incremental.pending_count(), 1);

        // Parent arrives later (resolves orphan during normal push)
        incremental.push(make_dir("later_dir"));
        assert_eq!(incremental.pending_count(), 0);
        assert_eq!(incremental.ready_count(), 2);

        // Finalization should have nothing to do
        let _ = incremental.drain_ready();
        let result = incremental.finalize();
        assert!(result.is_complete());
        assert_eq!(result.stats.orphans_detected, 0);
        assert_eq!(result.stats.placeholder_dirs_created, 0);
    }

    #[test]
    fn test_finalize_cascading_orphan_resolution() {
        // Orphan directory that itself has orphan children.
        // When "missing" is synthesized, its child dir "missing/sub" should be released,
        // which in turn releases "missing/sub/file.txt".
        let mut incremental = IncrementalFileList::new();
        incremental.push(make_file("missing/sub/file.txt"));
        incremental.push(make_dir("missing/sub"));

        // "missing/sub" is pending on "missing"
        // "missing/sub/file.txt" is pending on "missing/sub"
        assert_eq!(incremental.pending_count(), 2);

        let result = incremental.finalize();
        assert!(result.is_complete());
        assert_eq!(result.resolved_count(), 2);
        // Only "missing" needs a placeholder; "missing/sub" was a real directory entry
        assert_eq!(result.stats.placeholder_dirs_created, 1);
        assert_eq!(result.stats.orphans_resolved, 2);
    }

    #[test]
    fn test_finalize_preserves_unprocessed_ready_entries() {
        // Ready entries that weren't consumed should be returned in the result
        let mut incremental = IncrementalFileList::new();
        incremental.push(make_file("ready1.txt"));
        incremental.push(make_file("ready2.txt"));
        incremental.push(make_file("orphan_dir/orphan.txt"));

        // Don't drain ready - they should appear in finalization result
        let result = incremental.finalize();
        assert!(result.is_complete());
        assert_eq!(result.stats.unprocessed_ready, 2);
        // Total resolved = 2 ready + 1 resolved orphan = 3
        assert_eq!(result.resolved_count(), 3);

        let names: Vec<&str> = result.resolved_entries.iter().map(|e| e.name()).collect();
        assert!(names.contains(&"ready1.txt"));
        assert!(names.contains(&"ready2.txt"));
        assert!(names.contains(&"orphan_dir/orphan.txt"));
    }

    #[test]
    fn test_finalize_mixed_ready_and_orphans() {
        // Mix of normal entries (with parents) and orphaned entries
        let mut incremental = IncrementalFileList::new();

        // Normal entries
        incremental.push(make_dir("existing"));
        incremental.push(make_file("existing/normal.txt"));

        // Orphaned entries
        incremental.push(make_file("missing_a/orphan1.txt"));
        incremental.push(make_file("missing_b/orphan2.txt"));

        assert_eq!(incremental.ready_count(), 2);
        assert_eq!(incremental.pending_count(), 2);

        let result = incremental.finalize();
        assert!(result.is_complete());
        assert_eq!(result.stats.unprocessed_ready, 2);
        assert_eq!(result.stats.orphans_detected, 2);
        assert_eq!(result.stats.orphans_resolved, 2);
        assert_eq!(result.stats.placeholder_dirs_created, 2);
    }

    #[test]
    fn test_finalize_deeply_nested_multiple_branches() {
        // Multiple deeply nested orphans from different branches
        let mut incremental = IncrementalFileList::new();
        incremental.push(make_file("x/y/z/file1.txt"));
        incremental.push(make_file("a/b/c/file2.txt"));

        let result = incremental.finalize();
        assert!(result.is_complete());
        assert_eq!(result.resolved_count(), 2);
        // x, x/y, x/y/z + a, a/b, a/b/c = 6 placeholders
        assert_eq!(result.stats.placeholder_dirs_created, 6);
    }

    #[test]
    fn test_finalize_shared_ancestor_placeholders() {
        // Two orphans sharing a common ancestor that needs synthesis
        let mut incremental = IncrementalFileList::new();
        incremental.push(make_file("shared/branch_a/file1.txt"));
        incremental.push(make_file("shared/branch_b/file2.txt"));

        let result = incremental.finalize();
        assert!(result.is_complete());
        assert_eq!(result.resolved_count(), 2);
        // "shared" placeholder is shared, then "shared/branch_a" and "shared/branch_b"
        assert_eq!(result.stats.placeholder_dirs_created, 3);
    }

    #[test]
    fn test_finalize_empty_list() {
        // Finalizing an empty list should work cleanly
        let incremental = IncrementalFileList::new();
        let result = incremental.finalize();

        assert!(result.is_complete());
        assert_eq!(result.resolved_count(), 0);
        assert_eq!(result.orphan_count(), 0);
        assert!(result.stats.no_orphans());
        assert!(result.stats.all_resolved());
    }

    #[test]
    fn test_finalize_with_incremental_recursion_mode() {
        // Verify finalization works in incremental recursion mode
        let mut incremental = IncrementalFileList::with_incremental_recursion();
        incremental.push(make_file("late_dir/file.txt"));

        let result = incremental.finalize();
        assert!(result.is_complete());
        assert_eq!(result.stats.orphans_resolved, 1);
    }

    #[test]
    fn test_finalize_orphan_directory_releases_children() {
        // An orphan directory entry, when resolved, should cascade to release
        // children that were pending on it
        let mut incremental = IncrementalFileList::new();

        // Push file waiting for "missing/sub"
        incremental.push(make_file("missing/sub/file.txt"));
        // Push dir "missing/sub" waiting for "missing"
        incremental.push(make_dir("missing/sub"));
        // Push another file directly under "missing"
        incremental.push(make_file("missing/direct.txt"));

        assert_eq!(incremental.pending_count(), 3);

        let result = incremental.finalize();
        assert!(result.is_complete());
        // All 3 should be resolved: missing/sub (dir), missing/sub/file.txt, missing/direct.txt
        assert_eq!(result.resolved_count(), 3);
        assert_eq!(result.stats.placeholder_dirs_created, 1); // Only "missing"
    }

    #[test]
    fn test_collect_missing_ancestors() {
        let created = {
            let mut s = HashSet::new();
            s.insert(String::new());
            s.insert(".".to_string());
            s
        };

        // Simple case: "a/b/c" with nothing created
        let ancestors = IncrementalFileList::collect_missing_ancestors("a/b/c", &created);
        assert_eq!(ancestors, vec!["a", "a/b"]);

        // With "a" already created
        let mut created_with_a = created.clone();
        created_with_a.insert("a".to_string());
        let ancestors = IncrementalFileList::collect_missing_ancestors("a/b/c", &created_with_a);
        assert_eq!(ancestors, vec!["a/b"]);

        // Single-level path: "x" has no ancestors to collect
        let ancestors = IncrementalFileList::collect_missing_ancestors("x", &created);
        assert!(ancestors.is_empty());

        // Deep path
        let ancestors = IncrementalFileList::collect_missing_ancestors("a/b/c/d/e", &created);
        assert_eq!(ancestors, vec!["a", "a/b", "a/b/c", "a/b/c/d"]);
    }

    #[test]
    fn test_orphan_entry_accessors() {
        let entry = make_file("test/file.txt");
        let orphan = OrphanEntry {
            entry: entry.clone(),
            missing_parent: "test".to_string(),
        };

        assert_eq!(orphan.entry().name(), "test/file.txt");
        assert_eq!(orphan.missing_parent(), "test");
    }

    #[test]
    fn test_finalization_stats_predicates() {
        let stats_clean = FinalizationStats {
            orphans_detected: 0,
            orphans_resolved: 0,
            orphans_unresolved: 0,
            placeholder_dirs_created: 0,
            unprocessed_ready: 0,
        };
        assert!(stats_clean.no_orphans());
        assert!(stats_clean.all_resolved());

        let stats_resolved = FinalizationStats {
            orphans_detected: 5,
            orphans_resolved: 5,
            orphans_unresolved: 0,
            placeholder_dirs_created: 2,
            unprocessed_ready: 0,
        };
        assert!(!stats_resolved.no_orphans());
        assert!(stats_resolved.all_resolved());

        let stats_unresolved = FinalizationStats {
            orphans_detected: 3,
            orphans_resolved: 1,
            orphans_unresolved: 2,
            placeholder_dirs_created: 1,
            unprocessed_ready: 0,
        };
        assert!(!stats_unresolved.no_orphans());
        assert!(!stats_unresolved.all_resolved());
    }

    #[test]
    fn test_finalize_symlink_orphan() {
        // Symlinks should also be resolved as orphans
        let mut incremental = IncrementalFileList::new();
        let symlink = FileEntry::new_symlink("missing_dir/link".into(), "target".into());
        incremental.push(symlink);

        let result = incremental.finalize();
        assert!(result.is_complete());
        assert_eq!(result.resolved_count(), 1);
        assert!(result.resolved_entries[0].is_symlink());
        assert_eq!(result.stats.placeholder_dirs_created, 1);
    }

    #[test]
    fn test_finalize_after_partial_drain() {
        // Some entries drained, some left in ready queue, some orphaned
        let mut incremental = IncrementalFileList::new();
        incremental.push(make_file("ready1.txt"));
        incremental.push(make_file("ready2.txt"));
        incremental.push(make_file("missing/orphan.txt"));

        // Drain only one entry
        let _ = incremental.pop();
        assert_eq!(incremental.ready_count(), 1);

        let result = incremental.finalize();
        assert!(result.is_complete());
        // 1 remaining ready + 1 resolved orphan
        assert_eq!(result.resolved_count(), 2);
        assert_eq!(result.stats.unprocessed_ready, 1);
        assert_eq!(result.stats.orphans_resolved, 1);
    }

    #[test]
    fn test_finalize_with_builder_pre_created_dirs() {
        // Pre-created dirs should reduce the number of placeholders needed
        let mut incremental = IncrementalFileListBuilder::new()
            .pre_created_dir("pre_existing")
            .build();

        // This should be immediately ready since parent is pre-created
        incremental.push(make_file("pre_existing/file.txt"));
        assert_eq!(incremental.ready_count(), 1);

        // This needs "other" to be synthesized
        incremental.push(make_file("other/file.txt"));

        let _ = incremental.drain_ready();
        let result = incremental.finalize();
        assert!(result.is_complete());
        assert_eq!(result.stats.placeholder_dirs_created, 1);
    }

    // ========================================================================
    // process_ready_entry tests
    // ========================================================================

    fn make_symlink(name: &str, target: &str) -> FileEntry {
        FileEntry::new_symlink(name.into(), target.into())
    }

    fn make_block_device(name: &str) -> FileEntry {
        FileEntry::new_block_device(name.into(), 0o660, 8, 1)
    }

    fn make_fifo(name: &str) -> FileEntry {
        FileEntry::new_fifo(name.into(), 0o644)
    }

    /// No filter, no failed ancestor - always allow.
    fn no_filter(_name: &str, _is_dir: bool) -> bool {
        false
    }

    /// No failed ancestors.
    fn no_failures(_name: &str) -> Option<String> {
        None
    }

    #[test]
    fn test_process_ready_entry_regular_file() {
        let entry = make_file("src/main.rs");
        let action = process_ready_entry(entry, no_filter, no_failures);
        assert!(matches!(action, ReadyEntryAction::TransferFile(_)));
        assert!(action.is_actionable());
        assert!(!action.is_skipped());
        assert_eq!(action.entry().name(), "src/main.rs");
    }

    #[test]
    fn test_process_ready_entry_directory() {
        let entry = make_dir("src");
        let action = process_ready_entry(entry, no_filter, no_failures);
        assert!(matches!(action, ReadyEntryAction::CreateDirectory(_)));
        assert!(action.is_actionable());
        assert_eq!(action.entry().name(), "src");
    }

    #[test]
    fn test_process_ready_entry_symlink() {
        let entry = make_symlink("link", "/target");
        let action = process_ready_entry(entry, no_filter, no_failures);
        assert!(matches!(action, ReadyEntryAction::CreateSymlink(_)));
        assert!(action.is_actionable());
        assert_eq!(action.entry().name(), "link");
    }

    #[test]
    fn test_process_ready_entry_block_device() {
        let entry = make_block_device("dev/sda1");
        let action = process_ready_entry(entry, no_filter, no_failures);
        assert!(matches!(action, ReadyEntryAction::CreateDevice(_)));
        assert!(action.is_actionable());
    }

    #[test]
    fn test_process_ready_entry_fifo() {
        let entry = make_fifo("my_pipe");
        let action = process_ready_entry(entry, no_filter, no_failures);
        assert!(matches!(action, ReadyEntryAction::CreateSpecial(_)));
        assert!(action.is_actionable());
    }

    #[test]
    fn test_process_ready_entry_filtered_out() {
        let entry = make_file("build/output.o");
        // Filter excludes everything
        let action = process_ready_entry(entry, |_name, _is_dir| true, no_failures);
        assert!(matches!(action, ReadyEntryAction::SkipFiltered(_)));
        assert!(action.is_skipped());
        assert!(!action.is_actionable());
        assert_eq!(action.entry().name(), "build/output.o");
    }

    #[test]
    fn test_process_ready_entry_filtered_directory() {
        let entry = make_dir(".git");
        // Filter excludes directories named .git
        let action = process_ready_entry(
            entry,
            |name, _is_dir| name == ".git",
            no_failures,
        );
        assert!(matches!(action, ReadyEntryAction::SkipFiltered(_)));
        assert!(action.is_skipped());
    }

    #[test]
    fn test_process_ready_entry_failed_parent() {
        let entry = make_file("broken_dir/file.txt");
        let action = process_ready_entry(
            entry,
            no_filter,
            |name| {
                if name.starts_with("broken_dir") {
                    Some("broken_dir".to_string())
                } else {
                    None
                }
            },
        );
        match &action {
            ReadyEntryAction::SkipFailedParent {
                entry,
                failed_ancestor,
            } => {
                assert_eq!(entry.name(), "broken_dir/file.txt");
                assert_eq!(failed_ancestor, "broken_dir");
            }
            other => panic!("expected SkipFailedParent, got {other:?}"),
        }
        assert!(action.is_skipped());
        assert!(!action.is_actionable());
    }

    #[test]
    fn test_process_ready_entry_failed_parent_takes_priority_over_filter() {
        // When both failed parent and filter match, failed parent check is first
        let entry = make_file("bad/excluded.o");
        let action = process_ready_entry(
            entry,
            |_name, _is_dir| true, // would be filtered
            |_name| Some("bad".to_string()), // also has failed parent
        );
        // Failed parent check runs first
        assert!(matches!(action, ReadyEntryAction::SkipFailedParent { .. }));
    }

    #[test]
    fn test_process_ready_entry_filter_receives_correct_is_dir() {
        // Verify the filter callback receives correct is_dir flag
        let dir_entry = make_dir("mydir");
        let mut received_is_dir = false;
        let _ = process_ready_entry(
            dir_entry,
            |_name, is_dir| {
                received_is_dir = is_dir;
                false
            },
            no_failures,
        );
        assert!(received_is_dir, "directory entry should pass is_dir=true");

        let file_entry = make_file("myfile.txt");
        let mut received_is_dir = true;
        let _ = process_ready_entry(
            file_entry,
            |_name, is_dir| {
                received_is_dir = is_dir;
                false
            },
            no_failures,
        );
        assert!(!received_is_dir, "file entry should pass is_dir=false");
    }

    #[test]
    fn test_process_ready_entry_into_entry() {
        let entry = make_file("test.txt");
        let action = process_ready_entry(entry, no_filter, no_failures);
        let recovered = action.into_entry();
        assert_eq!(recovered.name(), "test.txt");
    }

    #[test]
    fn test_process_ready_entry_into_entry_from_skip() {
        let entry = make_file("filtered.txt");
        let action = process_ready_entry(entry, |_, _| true, no_failures);
        let recovered = action.into_entry();
        assert_eq!(recovered.name(), "filtered.txt");
    }

    #[test]
    fn test_process_ready_entry_into_entry_from_failed_parent() {
        let entry = make_file("bad/file.txt");
        let action = process_ready_entry(
            entry,
            no_filter,
            |_| Some("bad".to_string()),
        );
        let recovered = action.into_entry();
        assert_eq!(recovered.name(), "bad/file.txt");
    }

    #[test]
    fn test_process_ready_entries_multiple() {
        let mut incremental = IncrementalFileList::new();
        incremental.push(make_dir("src"));
        incremental.push(make_file("src/main.rs"));
        incremental.push(make_file("README.md"));
        incremental.push(make_symlink("latest", "v1.0"));

        let actions = process_ready_entries(
            &mut incremental,
            no_filter,
            no_failures,
        );
        assert_eq!(actions.len(), 4);
        assert!(matches!(actions[0], ReadyEntryAction::CreateDirectory(_)));
        assert!(matches!(actions[1], ReadyEntryAction::TransferFile(_)));
        assert!(matches!(actions[2], ReadyEntryAction::TransferFile(_)));
        assert!(matches!(actions[3], ReadyEntryAction::CreateSymlink(_)));
    }

    #[test]
    fn test_process_ready_entries_with_filter() {
        let mut incremental = IncrementalFileList::new();
        incremental.push(make_file("keep.txt"));
        incremental.push(make_file("skip.o"));
        incremental.push(make_file("also_keep.rs"));

        let actions = process_ready_entries(
            &mut incremental,
            |name, _is_dir| name.ends_with(".o"),
            no_failures,
        );
        assert_eq!(actions.len(), 3);
        assert!(matches!(actions[0], ReadyEntryAction::TransferFile(_)));
        assert!(matches!(actions[1], ReadyEntryAction::SkipFiltered(_)));
        assert!(matches!(actions[2], ReadyEntryAction::TransferFile(_)));
    }

    #[test]
    fn test_process_ready_entries_with_failed_dirs() {
        let mut incremental = IncrementalFileList::new();
        incremental.push(make_dir("good"));
        incremental.push(make_file("good/file.txt"));
        incremental.push(make_file("bad/orphan.txt")); // pending (parent missing)
        incremental.push(make_dir("bad")); // releases orphan
        incremental.push(make_file("ok.txt"));

        // Mark "bad" as a failed directory
        let mut failed_set: HashSet<String> = HashSet::new();
        failed_set.insert("bad".to_string());

        let actions = process_ready_entries(
            &mut incremental,
            no_filter,
            |name| {
                // Check exact match first, then walk up path ancestors.
                // This mirrors FailedDirectories::failed_ancestor() behavior.
                if failed_set.contains(name) {
                    return Some(name.to_string());
                }
                let mut check = name;
                while let Some(pos) = check.rfind('/') {
                    check = &check[..pos];
                    if failed_set.contains(check) {
                        return Some(check.to_string());
                    }
                }
                None
            },
        );

        assert_eq!(actions.len(), 5);

        // Collect names for order-independent assertions
        let action_names: Vec<(&str, bool)> = actions
            .iter()
            .map(|a| (a.entry().name(), a.is_skipped()))
            .collect();

        // good dir - should be actionable (CreateDirectory)
        let good_action = actions.iter().find(|a| a.entry().name() == "good").unwrap();
        assert!(matches!(good_action, ReadyEntryAction::CreateDirectory(_)));

        // good/file.txt - should be actionable (TransferFile)
        let good_file = actions.iter().find(|a| a.entry().name() == "good/file.txt").unwrap();
        assert!(matches!(good_file, ReadyEntryAction::TransferFile(_)));

        // bad dir - has failed ancestor "bad" (exact match)
        let bad_dir = actions.iter().find(|a| a.entry().name() == "bad").unwrap();
        match bad_dir {
            ReadyEntryAction::SkipFailedParent { failed_ancestor, .. } => {
                assert_eq!(failed_ancestor, "bad");
            }
            other => panic!("expected SkipFailedParent for 'bad', got {other:?}"),
        }

        // bad/orphan.txt - has failed ancestor "bad"
        let bad_orphan = actions.iter().find(|a| a.entry().name() == "bad/orphan.txt").unwrap();
        match bad_orphan {
            ReadyEntryAction::SkipFailedParent { failed_ancestor, .. } => {
                assert_eq!(failed_ancestor, "bad");
            }
            other => panic!("expected SkipFailedParent for 'bad/orphan.txt', got {other:?}"),
        }

        // ok.txt - should be actionable (TransferFile)
        let ok_file = actions.iter().find(|a| a.entry().name() == "ok.txt").unwrap();
        assert!(matches!(ok_file, ReadyEntryAction::TransferFile(_)));

        // Verify skip counts
        let skipped = action_names.iter().filter(|(_, s)| *s).count();
        let actionable = action_names.iter().filter(|(_, s)| !*s).count();
        assert_eq!(skipped, 2, "bad dir and bad/orphan.txt should be skipped");
        assert_eq!(actionable, 3, "good, good/file.txt, ok.txt should be actionable");
    }

    #[test]
    fn test_process_ready_entries_empty() {
        let mut incremental = IncrementalFileList::new();
        let actions = process_ready_entries(&mut incremental, no_filter, no_failures);
        assert!(actions.is_empty());
    }

    #[test]
    fn test_process_ready_entries_drains_queue() {
        let mut incremental = IncrementalFileList::new();
        incremental.push(make_file("a.txt"));
        incremental.push(make_file("b.txt"));

        let actions = process_ready_entries(&mut incremental, no_filter, no_failures);
        assert_eq!(actions.len(), 2);

        // Queue should be empty after drain
        assert!(incremental.is_empty());
        assert_eq!(incremental.ready_count(), 0);
    }

    #[test]
    fn test_process_ready_entry_char_device() {
        let entry = FileEntry::new_char_device("dev/tty0".into(), 0o666, 4, 0);
        let action = process_ready_entry(entry, no_filter, no_failures);
        assert!(matches!(action, ReadyEntryAction::CreateDevice(_)));
        assert!(action.is_actionable());
    }

    #[test]
    fn test_process_ready_entry_socket() {
        let entry = FileEntry::new_socket("run/app.sock".into(), 0o755);
        let action = process_ready_entry(entry, no_filter, no_failures);
        assert!(matches!(action, ReadyEntryAction::CreateSpecial(_)));
        assert!(action.is_actionable());
    }

    #[test]
    fn test_process_ready_entry_sequence_mixed_types() {
        // Simulate a realistic transfer with mixed entry types
        let entries = vec![
            make_dir("."),
            make_dir("src"),
            make_file("src/lib.rs"),
            make_file("src/main.rs"),
            make_dir("tests"),
            make_file("tests/integration.rs"),
            make_symlink("latest", "v1.0"),
            make_fifo("events"),
            make_file("Cargo.toml"),
        ];

        let mut dir_count = 0;
        let mut file_count = 0;
        let mut symlink_count = 0;
        let mut special_count = 0;

        for entry in entries {
            let action = process_ready_entry(entry, no_filter, no_failures);
            match action {
                ReadyEntryAction::CreateDirectory(_) => dir_count += 1,
                ReadyEntryAction::TransferFile(_) => file_count += 1,
                ReadyEntryAction::CreateSymlink(_) => symlink_count += 1,
                ReadyEntryAction::CreateSpecial(_) => special_count += 1,
                _ => panic!("unexpected skip action"),
            }
        }

        assert_eq!(dir_count, 3);
        assert_eq!(file_count, 4);
        assert_eq!(symlink_count, 1);
        assert_eq!(special_count, 1);
    }

    #[test]
    fn test_process_ready_entry_selective_filter() {
        // Filter that only excludes .o files
        let filter = |name: &str, _is_dir: bool| -> bool {
            name.ends_with(".o") || name.ends_with(".tmp")
        };

        let cases = vec![
            (make_file("main.o"), true),
            (make_file("temp.tmp"), true),
            (make_file("source.rs"), false),
            (make_dir("build"), false),
            (make_symlink("link", "target"), false),
        ];

        for (entry, should_filter) in cases {
            let name = entry.name().to_string();
            let action = process_ready_entry(entry, filter, no_failures);
            if should_filter {
                assert!(
                    action.is_skipped(),
                    "expected {name} to be filtered"
                );
            } else {
                assert!(
                    action.is_actionable(),
                    "expected {name} to be actionable"
                );
            }
        }
    }

    #[test]
    fn test_ready_entry_action_entry_accessor() {
        // Test entry() accessor for all variants
        let file_action = process_ready_entry(make_file("f.txt"), no_filter, no_failures);
        assert_eq!(file_action.entry().name(), "f.txt");

        let dir_action = process_ready_entry(make_dir("d"), no_filter, no_failures);
        assert_eq!(dir_action.entry().name(), "d");

        let sym_action = process_ready_entry(make_symlink("l", "t"), no_filter, no_failures);
        assert_eq!(sym_action.entry().name(), "l");

        let dev_action = process_ready_entry(make_block_device("dev"), no_filter, no_failures);
        assert_eq!(dev_action.entry().name(), "dev");

        let special_action = process_ready_entry(make_fifo("fifo"), no_filter, no_failures);
        assert_eq!(special_action.entry().name(), "fifo");

        let filtered_action = process_ready_entry(make_file("x"), |_, _| true, no_failures);
        assert_eq!(filtered_action.entry().name(), "x");

        let failed_action = process_ready_entry(
            make_file("bad/y"),
            no_filter,
            |_| Some("bad".to_string()),
        );
        assert_eq!(failed_action.entry().name(), "bad/y");
    }

    #[test]
    fn test_process_ready_entries_integrated_with_incremental() {
        // Full integration: push entries, let dependency tracking work, then process
        let mut incremental = IncrementalFileList::new();

        // Push entries out of order
        incremental.push(make_file("alpha/deep/file.txt")); // pending
        incremental.push(make_dir("alpha")); // releases alpha/deep/file.txt? No, alpha/deep still missing
        incremental.push(make_file("root.txt")); // ready immediately
        incremental.push(make_dir("alpha/deep")); // releases alpha/deep/file.txt

        // Process whatever is ready
        let actions = process_ready_entries(&mut incremental, no_filter, no_failures);

        // All 4 entries should be ready now (alpha dir created, alpha/deep created, file released)
        assert_eq!(actions.len(), 4);

        // Verify types
        let names: Vec<(&str, bool)> = actions
            .iter()
            .map(|a| (a.entry().name(), a.is_actionable()))
            .collect();
        assert!(names.contains(&("alpha", true)));
        assert!(names.contains(&("root.txt", true)));
        assert!(names.contains(&("alpha/deep", true)));
        assert!(names.contains(&("alpha/deep/file.txt", true)));

        // Queue should be empty
        assert!(incremental.is_empty());
        assert_eq!(incremental.pending_count(), 0);
    }
}
