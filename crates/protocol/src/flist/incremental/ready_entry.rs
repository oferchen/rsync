//! Ready entry dispatch for incremental file list processing.
//!
//! Determines the appropriate action for file entries based on their type,
//! filter rules, and failed directory state. This decouples the dispatch
//! logic from the dependency-tracking state machine.
//!
//! # Upstream Reference
//!
//! - `generator.c:recv_generator()` - Entry dispatch by type
//! - `flist.c:recv_file_list()` - File list processing with filtering

use logging::debug_log;

use crate::flist::FileEntry;

use super::IncrementalFileList;

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
    CreateDirectory(FileEntry),
    /// Transfer a regular file.
    TransferFile(FileEntry),
    /// Create a symbolic link.
    CreateSymlink(FileEntry),
    /// Create a device node (block or character device).
    CreateDevice(FileEntry),
    /// Create a special file (FIFO or socket).
    CreateSpecial(FileEntry),
    /// Skip the entry because it was excluded by filter rules.
    SkipFiltered(FileEntry),
    /// Skip the entry because a parent directory failed.
    ///
    /// The entry's ancestor directory could not be created, so this entry
    /// cannot be processed.
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
/// filter rules, and failed directory state.
///
/// # Parameters
///
/// - `entry`: The file entry to process (must have satisfied parent dependencies).
/// - `is_excluded`: A callback that returns `true` if the entry should be excluded.
///   Receives the entry's name (path) and whether it is a directory.
/// - `failed_ancestor`: A callback that checks whether the entry has a failed
///   ancestor directory. Returns `Some(ancestor_path)` if found.
///
/// # Upstream Reference
///
/// - `generator.c:recv_generator()` - Entry dispatch by type
/// - `flist.c:recv_file_list()` - File list processing with filtering
pub fn process_ready_entry<F, G>(
    entry: FileEntry,
    is_excluded: F,
    failed_ancestor: G,
) -> ReadyEntryAction
where
    F: FnOnce(&str, bool) -> bool,
    G: FnOnce(&str) -> Option<String>,
{
    let name = entry.name();
    let is_dir = entry.is_dir();

    // Check for failed ancestor directory first (cheapest check).
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

    if is_excluded(name, is_dir) {
        debug_log!(Flist, 2, "process_ready_entry: filtering out \"{}\"", name);
        return ReadyEntryAction::SkipFiltered(entry);
    }

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
        actions.push(process_ready_entry(
            entry,
            &mut is_excluded,
            &mut failed_ancestor,
        ));
    }
    actions
}
