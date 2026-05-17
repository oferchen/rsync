//! Deletion helpers for extraneous or source entries.

use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use logging::{debug_log, info_log};

#[cfg(not(feature = "legacy-batched-delete"))]
use crate::delete::RealDeleteFs;
use crate::delete::{
    DeleteContext, DeleteEntry, DeleteEntryKind, DeleteFs, DeletePlan, EmitterTiming,
};
use crate::local_copy::{CopyContext, LocalCopyAction, LocalCopyError, LocalCopyRecord};

/// Normalizes a filename for cross-platform comparison.
///
/// On macOS, converts NFD (decomposed) filenames to NFC (composed) so that
/// names from `read_dir` (which returns NFD on HFS+/APFS) match names from
/// the source file list (typically NFC from Linux). On all other platforms
/// this returns the input as-is.
#[cfg(target_os = "macos")]
fn normalize_filename_for_compare(name: &OsStr) -> OsString {
    apple_fs::normalize_filename(name)
}

/// No-op on non-macOS platforms - direct byte comparison is correct.
#[cfg(not(target_os = "macos"))]
fn normalize_filename_for_compare(name: &OsStr) -> OsString {
    name.to_os_string()
}

/// Deletes entries in `destination` that are not in `source_entries`.
///
/// The `source_entries` parameter accepts any slice of types convertible to `&OsStr`,
/// including `&[OsString]` (owned) and `&[&OsString]` (borrowed), avoiding allocation
/// when borrowing from an existing data structure.
///
/// # Implementation path
///
/// Routes through [`delete_extraneous_entries_via_emitter`] by default so
/// the [`crate::delete::DeleteEmitter`] is the live unlink path for every
/// `--delete-*` timing mode (DDP-E1..E5, #2265-#2269). With the
/// `legacy-batched-delete` cargo feature on, falls back to the pre-DDP-E
/// batched sweep retained for emergency rollback; that path is removed
/// in DDP-F3.
pub(crate) fn delete_extraneous_entries<S: AsRef<OsStr>>(
    context: &mut CopyContext,
    destination: &Path,
    relative: Option<&Path>,
    source_entries: &[S],
) -> Result<(), LocalCopyError> {
    #[cfg(feature = "legacy-batched-delete")]
    {
        delete_extraneous_entries_batched(context, destination, relative, source_entries)
    }
    #[cfg(not(feature = "legacy-batched-delete"))]
    {
        delete_extraneous_entries_via_emitter(
            context,
            destination,
            relative,
            source_entries,
            &RealDeleteFs,
        )
    }
}

/// Emitter-backed deletion: computes the per-directory plan, hands it to
/// a fresh [`crate::delete::DeleteEmitter`], and threads the existing
/// per-entry bookkeeping (filter checks, `--max-delete`, partial-dir
/// protection, backup, summary, [`LocalCopyRecord`]) around the emitter
/// dispatch.
///
/// The emitter is the SOLE caller of [`DeleteFs`] methods, satisfying the
/// single-emitter invariant from
/// `docs/design/parallel-deterministic-delete.md` section 2.3. The
/// surrounding bookkeeping must never call `fs::remove_*` directly.
///
/// `fs` is parameterised so tests can substitute
/// [`crate::delete::RecordingDeleteFs`] and observe the unlink sequence
/// without touching the filesystem.
#[cfg_attr(feature = "legacy-batched-delete", allow(dead_code))]
pub(crate) fn delete_extraneous_entries_via_emitter<S: AsRef<OsStr>, F: DeleteFs>(
    context: &mut CopyContext,
    destination: &Path,
    relative: Option<&Path>,
    source_entries: &[S],
    fs: &F,
) -> Result<(), LocalCopyError> {
    // Phase 1: scan the destination directory, compute extras, and
    // produce a single-directory DeletePlan in upstream emission order.
    // The plan respects partial-dir protection, allows_deletion filter
    // rules, and the max-delete limit before publication so the emitter
    // sees only the entries that should actually unlink.
    let mut skipped_due_to_limit = 0u64;
    let plan = match build_plan_for_directory(
        context,
        destination,
        relative,
        source_entries,
        &mut skipped_due_to_limit,
    )? {
        Some(plan) => plan,
        None => return Ok(()),
    };

    // Phase 2: build a single-directory DeleteContext, observe the
    // segment so the cursor yields the directory, publish the plan
    // directly into the context's map, and drain via emit_one.
    let ctx = DeleteContext::new(destination.to_path_buf(), EmitterTiming::During);
    // TODO: wire DDP-B3 - replace `observe_directory` with the segment
    // observation hook so the receiver pipeline drives the cursor.
    ctx.observe_directory(destination.to_path_buf(), &[]);
    ctx.plans.insert(plan.clone());

    // Run side-effects (records, summary, backup) BEFORE dispatch so the
    // dry-run path is observably identical to the legacy sweep. Then
    // (unless dry-run) hand the plan to the emitter for the actual
    // syscalls.
    apply_delete_side_effects(context, destination, relative, &plan)?;

    if !context.mode().is_dry_run() {
        let _outcome = ctx.emit_one(fs).map_err(|error| {
            LocalCopyError::io("emit delete plan", destination.to_path_buf(), error)
        })?;
    }

    if skipped_due_to_limit > 0 {
        info_log!(
            Del,
            1,
            "max deletions reached, skipping {} remaining",
            skipped_due_to_limit
        );
        return Err(LocalCopyError::delete_limit_exceeded(skipped_due_to_limit));
    }

    Ok(())
}

/// Builds the per-directory [`DeletePlan`] used by
/// [`delete_extraneous_entries_via_emitter`]. Returns `None` when
/// `destination` does not exist (e.g. removed between traversal and
/// dispatch); the caller treats that as a no-op, matching upstream's
/// continue-on-vanished behaviour.
#[cfg_attr(feature = "legacy-batched-delete", allow(dead_code))]
fn build_plan_for_directory<S: AsRef<OsStr>>(
    context: &mut CopyContext,
    destination: &Path,
    relative: Option<&Path>,
    source_entries: &[S],
    skipped_due_to_limit: &mut u64,
) -> Result<Option<DeletePlan>, LocalCopyError> {
    let keep: HashSet<OsString> = source_entries
        .iter()
        .map(|s| normalize_filename_for_compare(s.as_ref()))
        .collect();

    let read_dir = match fs::read_dir(destination) {
        Ok(iter) => iter,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(LocalCopyError::io(
                "read destination directory",
                destination.to_path_buf(),
                error,
            ));
        }
    };

    let protected_partial_dir_name: Option<OsString> = context
        .partial_directory_path()
        .filter(|p| p.is_relative())
        .and_then(|p| p.file_name())
        .map(OsStr::to_os_string);

    let mut plan = DeletePlan::new(destination.to_path_buf());
    for entry in read_dir {
        context.enforce_timeout()?;
        let entry = entry
            .map_err(|error| LocalCopyError::io("read destination entry", destination, error))?;
        let name = entry.file_name();
        let normalized_name = normalize_filename_for_compare(&name);

        if keep.contains(&normalized_name) {
            continue;
        }
        if let Some(ref protected) = protected_partial_dir_name
            && normalized_name == *protected
        {
            continue;
        }

        let name_path = PathBuf::from(name.as_os_str());
        let entry_relative = match relative {
            Some(base) => base.join(&name_path),
            None => name_path.clone(),
        };
        let file_type = entry.file_type().map_err(|error| {
            LocalCopyError::io(
                "inspect extraneous destination entry",
                destination.join(&name_path),
                error,
            )
        })?;

        if !context.allows_deletion(entry_relative.as_path(), file_type.is_dir()) {
            debug_log!(
                Filter,
                2,
                "filter protected {} from deletion",
                entry_relative.display()
            );
            continue;
        }

        if let Some(limit) = context.options().max_deletion_limit()
            && context
                .summary()
                .items_deleted()
                .saturating_add(plan.len() as u64)
                >= limit
        {
            *skipped_due_to_limit = skipped_due_to_limit.saturating_add(1);
            continue;
        }

        plan.push(DeleteEntry::new(name, classify_kind(file_type)));
    }
    plan.sort_by_name();
    Ok(Some(plan))
}

/// Mirrors the upstream classification table used by
/// [`crate::delete::extras::compute_extras`]: regular files, dirs,
/// symlinks, devices, FIFOs/sockets each fall into their own
/// [`DeleteEntryKind`] bucket; anything else collapses to `File`.
#[cfg_attr(feature = "legacy-batched-delete", allow(dead_code))]
fn classify_kind(file_type: fs::FileType) -> DeleteEntryKind {
    if file_type.is_symlink() {
        return DeleteEntryKind::Symlink;
    }
    if file_type.is_dir() {
        return DeleteEntryKind::Dir;
    }
    if file_type.is_file() {
        return DeleteEntryKind::File;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileTypeExt;
        if file_type.is_block_device() || file_type.is_char_device() {
            return DeleteEntryKind::Device;
        }
        if file_type.is_fifo() || file_type.is_socket() {
            return DeleteEntryKind::Special;
        }
    }
    DeleteEntryKind::File
}

/// Runs the per-entry side effects (backup, summary counter, itemize
/// log, [`LocalCopyRecord`]) for every entry in `plan`. Runs BEFORE the
/// emitter dispatches so dry-run output and event ordering match the
/// legacy sweep byte-for-byte.
///
/// Directories recurse into their contents so per-entry counters match
/// upstream's `delete_in_dir` walk (one count per leaf, mirroring
/// `target/interop/upstream-src/rsync-3.4.1/generator.c:272-347`).
#[cfg_attr(feature = "legacy-batched-delete", allow(dead_code))]
fn apply_delete_side_effects(
    context: &mut CopyContext,
    destination: &Path,
    relative: Option<&Path>,
    plan: &DeletePlan,
) -> Result<(), LocalCopyError> {
    for entry in &plan.extras {
        let name_path = PathBuf::from(entry.name.as_os_str());
        let path = destination.join(&name_path);
        let entry_relative = match relative {
            Some(base) => base.join(&name_path),
            None => name_path.clone(),
        };
        let is_dir = matches!(entry.kind, DeleteEntryKind::Dir);

        let file_type = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata.file_type(),
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(LocalCopyError::io(
                    "inspect extraneous destination entry",
                    path.clone(),
                    error,
                ));
            }
        };

        if is_dir {
            record_directory_subtree(context, &path, &entry_relative)?;
        }

        if !context.mode().is_dry_run() {
            context.backup_existing_entry(&path, Some(entry_relative.as_path()), file_type)?;
        }
        if is_dir {
            info_log!(Del, 1, "deleting directory {}", entry_relative.display());
        } else {
            info_log!(Del, 1, "deleting {}", entry_relative.display());
        }
        context.summary_mut().record_deletion();
        context.record(LocalCopyRecord::new(
            entry_relative,
            LocalCopyAction::EntryDeleted,
            0,
            None,
            Duration::default(),
            None,
        ));
        context.register_progress();
    }
    Ok(())
}

/// Recurses through `dir_path`'s contents, emitting per-leaf records and
/// summary counters before the emitter wipes the directory via
/// `remove_dir_all`. Matches upstream's per-entry counting in
/// `delete_in_dir`.
#[cfg_attr(feature = "legacy-batched-delete", allow(dead_code))]
fn record_directory_subtree(
    context: &mut CopyContext,
    dir_path: &Path,
    dir_relative: &Path,
) -> Result<(), LocalCopyError> {
    let read_dir = match fs::read_dir(dir_path) {
        Ok(iter) => iter,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(LocalCopyError::io(
                "read extraneous directory",
                dir_path.to_path_buf(),
                error,
            ));
        }
    };
    for entry in read_dir {
        context.enforce_timeout()?;
        let entry = entry.map_err(|error| {
            LocalCopyError::io("read extraneous directory entry", dir_path, error)
        })?;
        let name = entry.file_name();
        let child_path = dir_path.join(&name);
        let child_relative = dir_relative.join(&name);
        let file_type = entry.file_type().map_err(|error| {
            LocalCopyError::io(
                "inspect extraneous directory entry",
                child_path.clone(),
                error,
            )
        })?;
        if file_type.is_dir() {
            record_directory_subtree(context, &child_path, &child_relative)?;
            info_log!(Del, 1, "deleting directory {}", child_relative.display());
        } else {
            info_log!(Del, 1, "deleting {}", child_relative.display());
        }
        context.summary_mut().record_deletion();
        context.record(LocalCopyRecord::new(
            child_relative,
            LocalCopyAction::EntryDeleted,
            0,
            None,
            Duration::default(),
            None,
        ));
        context.register_progress();
    }
    Ok(())
}

/// Legacy pre-DDP-E batched sweep. Retained behind
/// `cfg(feature = "legacy-batched-delete")` for emergency rollback; slated
/// for removal in DDP-F3. Identical behaviour to the original
/// `delete_extraneous_entries`.
#[cfg(feature = "legacy-batched-delete")]
pub(crate) fn delete_extraneous_entries_batched<S: AsRef<OsStr>>(
    context: &mut CopyContext,
    destination: &Path,
    relative: Option<&Path>,
    source_entries: &[S],
) -> Result<(), LocalCopyError> {
    let mut skipped_due_to_limit = 0u64;
    // Build HashSet of normalized filenames for cross-platform comparison.
    // On macOS, normalizes to NFC so NFD names from read_dir match NFC source names.
    let keep: HashSet<OsString> = source_entries
        .iter()
        .map(|s| normalize_filename_for_compare(s.as_ref()))
        .collect();

    let read_dir = match fs::read_dir(destination) {
        Ok(iter) => iter,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(LocalCopyError::io(
                "read destination directory",
                destination.to_path_buf(),
                error,
            ));
        }
    };

    // When --partial-dir is configured with a relative path, protect it from
    // deletion.  Upstream rsync avoids deleting the partial-dir directory so
    // that partial files survive across invocations even when --delete is
    // active.  Absolute partial-dir paths live outside the destination tree
    // and do not need protection.
    let protected_partial_dir_name: Option<OsString> = context
        .partial_directory_path()
        .filter(|p| p.is_relative())
        .and_then(|p| p.file_name())
        .map(OsStr::to_os_string);

    for entry in read_dir {
        context.enforce_timeout()?;
        let entry = entry
            .map_err(|error| LocalCopyError::io("read destination entry", destination, error))?;
        let name = entry.file_name();
        let normalized_name = normalize_filename_for_compare(&name);

        if keep.contains(&normalized_name) {
            continue;
        }

        // Protect relative partial-dir from deletion (upstream rsync behavior).
        if let Some(ref protected) = protected_partial_dir_name {
            if normalized_name == *protected {
                continue;
            }
        }

        let name_path = PathBuf::from(name.as_os_str());
        let path = destination.join(&name_path);
        let entry_relative = match relative {
            Some(base) => base.join(&name_path),
            None => name_path.clone(),
        };

        let file_type = entry.file_type().map_err(|error| {
            LocalCopyError::io("inspect extraneous destination entry", path.clone(), error)
        })?;

        if !context.allows_deletion(entry_relative.as_path(), file_type.is_dir()) {
            debug_log!(
                Filter,
                2,
                "filter protected {} from deletion",
                entry_relative.display()
            );
            continue;
        }

        if let Some(limit) = context.options().max_deletion_limit()
            && context.summary().items_deleted() >= limit
        {
            skipped_due_to_limit = skipped_due_to_limit.saturating_add(1);
            continue;
        }

        if context.mode().is_dry_run() {
            if file_type.is_dir() {
                delete_directory_tree_recursive(
                    context,
                    &path,
                    &entry_relative,
                    &mut skipped_due_to_limit,
                )?;
            }
            context.summary_mut().record_deletion();
            context.record(LocalCopyRecord::new(
                entry_relative,
                LocalCopyAction::EntryDeleted,
                0,
                None,
                Duration::default(),
                None,
            ));
            context.register_progress();
            continue;
        }

        context.backup_existing_entry(&path, Some(entry_relative.as_path()), file_type)?;
        if file_type.is_dir() {
            // upstream: generator.c:delete_in_dir() - recursively delete
            // directory contents first, counting each item individually,
            // then remove the now-empty directory.
            delete_directory_tree_recursive(
                context,
                &path,
                &entry_relative,
                &mut skipped_due_to_limit,
            )?;
            info_log!(Del, 1, "deleting directory {}", entry_relative.display());
            match fs::remove_dir(&path) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(LocalCopyError::io(
                        "remove extraneous directory",
                        path,
                        error,
                    ));
                }
            }
        } else {
            info_log!(Del, 1, "deleting {}", entry_relative.display());
            remove_extraneous_path(&path, file_type)?;
        }
        context.summary_mut().record_deletion();
        context.record(LocalCopyRecord::new(
            entry_relative,
            LocalCopyAction::EntryDeleted,
            0,
            None,
            Duration::default(),
            None,
        ));
        context.register_progress();
    }

    if skipped_due_to_limit > 0 {
        info_log!(
            Del,
            1,
            "max deletions reached, skipping {} remaining",
            skipped_due_to_limit
        );
        return Err(LocalCopyError::delete_limit_exceeded(skipped_due_to_limit));
    }

    Ok(())
}

#[cfg(feature = "legacy-batched-delete")]
fn remove_extraneous_path(path: &Path, file_type: fs::FileType) -> Result<(), LocalCopyError> {
    let context = if file_type.is_dir() {
        "remove extraneous directory"
    } else {
        "remove extraneous entry"
    };

    let result = if file_type.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    };

    match result {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(LocalCopyError::io(context, path, error)),
    }
}

/// Recursively deletes an extraneous directory, recording each item
/// individually in the deletion count and event log.
///
/// Unlike `remove_dir_all` (which counts as a single deletion), this mirrors
/// upstream rsync's `delete_in_dir()` behavior: it descends into the directory
/// tree, deletes files first, then removes the empty directories bottom-up,
/// counting each item as a separate deletion.
#[cfg(feature = "legacy-batched-delete")]
fn delete_directory_tree_recursive(
    context: &mut CopyContext,
    dir_path: &Path,
    dir_relative: &Path,
    skipped_due_to_limit: &mut u64,
) -> Result<(), LocalCopyError> {
    let read_dir = match fs::read_dir(dir_path) {
        Ok(iter) => iter,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(LocalCopyError::io(
                "read extraneous directory",
                dir_path.to_path_buf(),
                error,
            ));
        }
    };

    for entry in read_dir {
        context.enforce_timeout()?;
        let entry = entry.map_err(|error| {
            LocalCopyError::io("read extraneous directory entry", dir_path, error)
        })?;
        let name = entry.file_name();
        let child_path = dir_path.join(&name);
        let child_relative = dir_relative.join(&name);

        let file_type = entry.file_type().map_err(|error| {
            LocalCopyError::io(
                "inspect extraneous directory entry",
                child_path.clone(),
                error,
            )
        })?;

        if !context.allows_deletion(child_relative.as_path(), file_type.is_dir()) {
            debug_log!(
                Filter,
                2,
                "filter protected {} from deletion",
                child_relative.display()
            );
            continue;
        }

        if let Some(limit) = context.options().max_deletion_limit()
            && context.summary().items_deleted() >= limit
        {
            *skipped_due_to_limit = skipped_due_to_limit.saturating_add(1);
            continue;
        }

        if context.mode().is_dry_run() {
            if file_type.is_dir() {
                delete_directory_tree_recursive(
                    context,
                    &child_path,
                    &child_relative,
                    skipped_due_to_limit,
                )?;
            }
            context.summary_mut().record_deletion();
            context.record(LocalCopyRecord::new(
                child_relative,
                LocalCopyAction::EntryDeleted,
                0,
                None,
                Duration::default(),
                None,
            ));
            context.register_progress();
            continue;
        }

        context.backup_existing_entry(&child_path, Some(child_relative.as_path()), file_type)?;

        if file_type.is_dir() {
            delete_directory_tree_recursive(
                context,
                &child_path,
                &child_relative,
                skipped_due_to_limit,
            )?;
            info_log!(Del, 1, "deleting directory {}", child_relative.display());
            match fs::remove_dir(&child_path) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(LocalCopyError::io(
                        "remove extraneous directory",
                        child_path,
                        error,
                    ));
                }
            }
        } else {
            info_log!(Del, 1, "deleting {}", child_relative.display());
            match fs::remove_file(&child_path) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(LocalCopyError::io(
                        "remove extraneous entry",
                        child_path,
                        error,
                    ));
                }
            }
        }
        context.summary_mut().record_deletion();
        context.record(LocalCopyRecord::new(
            child_relative,
            LocalCopyAction::EntryDeleted,
            0,
            None,
            Duration::default(),
            None,
        ));
        context.register_progress();
    }

    Ok(())
}

/// Removes the source entry after a successful copy when `--remove-source-files` is active.
///
/// Directories are never removed (upstream rsync only removes files).
/// No-ops in dry-run mode.
pub(crate) fn remove_source_entry_if_requested(
    context: &mut CopyContext,
    source: &Path,
    record_path: Option<&Path>,
    file_type: fs::FileType,
) -> Result<(), LocalCopyError> {
    if !context.remove_source_files_enabled() || context.mode().is_dry_run() {
        return Ok(());
    }

    let source_type = match fs::symlink_metadata(source) {
        Ok(metadata) => metadata.file_type(),
        Err(_) => file_type,
    };

    if source_type.is_dir() {
        return Ok(());
    }

    match fs::remove_file(source) {
        Ok(()) => {
            info_log!(Remove, 1, "removing source {}", source.display());
            context.summary_mut().record_source_removed();
            if let Some(path) = record_path {
                context.record(LocalCopyRecord::new(
                    path.to_path_buf(),
                    LocalCopyAction::SourceRemoved,
                    0,
                    None,
                    Duration::default(),
                    None,
                ));
            }
            context.register_progress();
            Ok(())
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(LocalCopyError::io(
            "remove source entry",
            source.to_path_buf(),
            error,
        )),
    }
}
