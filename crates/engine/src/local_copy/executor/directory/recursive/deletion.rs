//! Post-transfer deletion handling and empty directory pruning.
//!
//! Implements the `--delete-after` and `--delete-during` phases that remove
//! extraneous destination entries. Also handles `--prune-empty-dirs`.

// upstream: generator.c:delete_in_dir() - post-transfer deletion

use std::borrow::Cow;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::Path;

use crate::local_copy::executor::decide_and_defer_delayed_deletions;
use crate::local_copy::{CopyContext, DeleteTiming, LocalCopyError, delete_extraneous_entries};

/// Resolves the effective delete timing for the current directory.
///
/// upstream: generator.c shares a single flist across sources so
/// `delete_during` never unlinks an entry that a later source will recreate.
/// oc-rsync reads each source directory live, so when more than one source is
/// in play a `--delete-during` sweep is downgraded to a deferred (`After`)
/// sweep whose keep-lists are merged across sources in `defer_deletion`.
fn effective_delete_timing(
    context: &CopyContext,
    delete_timing: Option<DeleteTiming>,
) -> DeleteTiming {
    let timing = delete_timing.unwrap_or(DeleteTiming::During);
    if matches!(timing, DeleteTiming::During) && context.multi_source() {
        DeleteTiming::After
    } else {
        timing
    }
}

/// Returns `true` when a `--delete-delay` run should decide its deletions
/// during the transfer walk (upstream `remember_delete`) instead of falling
/// back to the deferred rescan path shared with `--delete-after`.
///
/// The rescan fallback is kept for two cases the decide-during path cannot yet
/// serve without losing existing guarantees:
/// - multi-source runs, where a later source's entries must be able to protect
///   a file a sibling source would otherwise delete (mirrors the During->After
///   downgrade in [`effective_delete_timing`]);
/// - `--max-delete`, whose leaf-granular cap is enforced only by the rescan
///   executor (`delete_extraneous_entries_capped`).
fn delay_decides_during(context: &CopyContext) -> bool {
    !context.multi_source() && context.options().max_deletion_limit().is_none()
}

/// Deletes extraneous destination entries for `--delete-during` before the
/// directory's own children are transferred.
///
/// upstream: generator.c:1532-1537 - for a non-INC_RECURSE `delete_during`
/// run, the generator calls `delete_in_dir()` while itemizing the directory
/// entry itself, i.e. immediately before it recurses into and processes that
/// directory's children. The extraneous-entry `*deleting` rows therefore
/// precede the transfer rows for surviving/new entries in the same directory.
/// Running the sweep after the child loop (as a post-transfer step) would
/// invert that order. Deferred timings (`--delete-delay`, `--delete-after`,
/// and the multi-source `During`->`After` downgrade) are handled after the
/// loop by [`handle_post_transfer_deletions`].
#[inline]
pub(super) fn apply_during_transfer_deletions(
    context: &mut CopyContext,
    destination: &Path,
    relative: Option<&Path>,
    deletion_enabled: bool,
    delete_timing: Option<DeleteTiming>,
    keep_names: &[Cow<'_, OsStr>],
) -> Result<(), LocalCopyError> {
    if !deletion_enabled {
        return Ok(());
    }

    // When I/O errors occurred and --ignore-errors is not set, suppress
    // deletions to prevent data loss (upstream rsync behavior) and emit the
    // one-shot skip notice. upstream: generator.c:298-305.
    if context.delete_pass_blocked_by_io_error() {
        return Ok(());
    }

    match effective_delete_timing(context, delete_timing) {
        DeleteTiming::During => {
            delete_extraneous_entries(context, destination, relative, keep_names)?;
        }
        // upstream: generator.c:345 `delete_during == 2` decides the delete set
        // during the walk (via `delete_in_dir`/`change_local_filter_dir`, while
        // the destination merge files are still absent) and only postpones the
        // unlink. Decide here and defer the concrete plan; the flush executes it
        // without re-consulting the by-then-present merge files.
        DeleteTiming::Delay if delay_decides_during(context) => {
            decide_and_defer_delayed_deletions(context, destination, relative, keep_names)?;
        }
        _ => {}
    }

    Ok(())
}

/// Handles the deferred deletion phases after transfer.
///
/// Only `--delete-delay`, `--delete-after`, and the multi-source
/// `During`->`After` downgrade reach a delete here; immediate `--delete-during`
/// sweeps are performed before the child loop by
/// [`apply_during_transfer_deletions`]. `--delete-before` was already handled
/// by `apply_pre_transfer_deletions`.
#[inline]
pub(super) fn handle_post_transfer_deletions(
    context: &mut CopyContext,
    destination: &Path,
    relative: Option<&Path>,
    deletion_enabled: bool,
    delete_timing: Option<DeleteTiming>,
    keep_names: &[Cow<'_, OsStr>],
) -> Result<(), LocalCopyError> {
    if !deletion_enabled {
        return Ok(());
    }

    // When I/O errors occurred and --ignore-errors is not set, suppress
    // deletions to prevent data loss (upstream rsync behavior) and emit the
    // one-shot skip notice. upstream: generator.c:298-305.
    if context.delete_pass_blocked_by_io_error() {
        return Ok(());
    }

    match effective_delete_timing(context, delete_timing) {
        DeleteTiming::Before | DeleteTiming::During => {
            // Before: already handled by apply_pre_transfer_deletions.
            // During: already handled by apply_during_transfer_deletions.
        }
        // A --delete-delay run that decided-and-deferred during the walk has
        // nothing left to defer here; one that fell back to the rescan path
        // (multi-source or --max-delete) is handled by the After arm below.
        DeleteTiming::Delay if delay_decides_during(context) => {}
        DeleteTiming::Delay | DeleteTiming::After => {
            // Clone names for deferred processing (data must outlive the plan)
            let keep_owned: Vec<OsString> =
                keep_names.iter().map(|s| OsStr::to_os_string(s)).collect();
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
