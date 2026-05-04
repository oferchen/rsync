//! Post-transfer deletion handling and empty directory pruning.
//!
//! Implements the `--delete-after` and `--delete-during` phases that remove
//! extraneous destination entries. Also handles `--prune-empty-dirs`.
//!
//! upstream: generator.c:delete_in_dir() - post-transfer deletion

use std::ffi::OsString;
use std::fs;
use std::path::Path;

use crate::local_copy::{CopyContext, DeleteTiming, LocalCopyError, delete_extraneous_entries};

/// Handles the deletion phase after transfer, based on the configured timing.
#[inline]
pub(super) fn handle_post_transfer_deletions(
    context: &mut CopyContext,
    destination: &Path,
    relative: Option<&Path>,
    deletion_enabled: bool,
    delete_timing: Option<DeleteTiming>,
    keep_names: &[&OsString],
) -> Result<(), LocalCopyError> {
    if !deletion_enabled {
        return Ok(());
    }

    // When I/O errors occurred and --ignore-errors is not set, suppress
    // deletions to prevent data loss (upstream rsync behavior).
    if !context.deletions_allowed() {
        return Ok(());
    }

    match delete_timing.unwrap_or(DeleteTiming::During) {
        DeleteTiming::Before => {
            // Already handled by apply_pre_transfer_deletions
        }
        DeleteTiming::During => {
            delete_extraneous_entries(context, destination, relative, keep_names)?;
        }
        DeleteTiming::Delay | DeleteTiming::After => {
            // Clone names for deferred processing (data must outlive the plan)
            let keep_owned: Vec<OsString> = keep_names.iter().map(|&s| s.clone()).collect();
            let relative_owned = relative.map(Path::to_path_buf);
            context.defer_deletion(destination.to_path_buf(), relative_owned, keep_owned);
        }
    }

    Ok(())
}

/// Handles cleanup when an empty directory should be pruned.
///
/// Returns `true` if the directory was removed, `false` if it should be kept.
#[inline]
pub(super) fn handle_empty_directory_pruning(
    context: &mut CopyContext,
    destination: &Path,
    created_directory_on_disk: bool,
) -> Result<bool, LocalCopyError> {
    if created_directory_on_disk {
        fs::remove_dir(destination)
            .map_err(|error| LocalCopyError::io("remove empty directory", destination, error))?;
        if context
            .last_created_entry_path()
            .is_some_and(|path| path == destination)
        {
            context.pop_last_created_entry();
        }
        Ok(true)
    } else {
        Ok(false)
    }
}
