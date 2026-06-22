//! Deletion helpers for extraneous or source entries.

use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use logging::{debug_log, info_log};

use std::sync::Arc;

use crate::delete::{
    DeleteContext, DeleteEntry, DeleteEntryKind, DeleteFs, DeletePlan, DeletePlanMap, RealDeleteFs,
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
/// Routes through [`delete_extraneous_entries_via_emitter`] so the
/// [`crate::delete::DeleteEmitter`] is the live unlink path for every
/// `--delete-*` timing mode (DDP-E1..E5, #2265-#2269). The legacy
/// pre-DDP-E batched sweep was removed in DDP-F3 (#2272).
pub(crate) fn delete_extraneous_entries<S: AsRef<OsStr>>(
    context: &mut CopyContext,
    destination: &Path,
    relative: Option<&Path>,
    source_entries: &[S],
) -> Result<(), LocalCopyError> {
    // DEL-2.d: feature-gated dispatch, parallel-delete-consumer opt-in.
    // `RealDeleteFs` is passed by value so the parallel consumer's
    // `Sync + Send + 'static` bound is satisfied; the sequential path is
    // unaffected because it takes the dispatcher generically by value too.
    delete_extraneous_entries_via_emitter(
        context,
        destination,
        relative,
        source_entries,
        RealDeleteFs,
    )
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
fn delete_extraneous_entries_via_emitter<S: AsRef<OsStr>, F>(
    context: &mut CopyContext,
    destination: &Path,
    relative: Option<&Path>,
    source_entries: &[S],
    fs: F,
) -> Result<(), LocalCopyError>
where
    F: DeleteFs + Sync + Send + 'static,
{
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
    //
    // The cursor root must equal the plan's directory key so
    // `cursor.next_ready()` yields a path that `plans.take()` resolves
    // to the published plan. `DeleteContext::new` would leave the
    // cursor at the empty relative path; use `with_cursor_root` to seat
    // it on the absolute destination the plan is keyed on.
    let plans_map = Arc::new(DeletePlanMap::new());
    plans_map.insert(plan.clone());
    let ctx = DeleteContext::with_cursor_root(
        plans_map,
        destination.to_path_buf(),
        destination.to_path_buf(),
        true,
    );
    // `observe_directory` is called with an empty child list because the
    // single-directory cleanup path has no parent segment to enumerate;
    // the receiver pipeline drives the segment-observation hook in the
    // multi-segment delete flow.
    ctx.observe_directory(destination.to_path_buf(), &[]);

    // Run side-effects (records, summary, backup) BEFORE dispatch so the
    // dry-run path is observably identical to a live unlink. Then
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
fn build_plan_for_directory<S: AsRef<OsStr>>(
    context: &mut CopyContext,
    destination: &Path,
    relative: Option<&Path>,
    source_entries: &[S],
    skipped_due_to_limit: &mut u64,
) -> Result<Option<DeletePlan>, LocalCopyError> {
    // upstream: delete.c:63 - `delete_dir_contents()` calls
    // `push_local_filters(fname, dlen)` with the destination directory so the
    // receiver applies any `: filter` rules found in the directory being
    // scanned. The guard pops the loaded rules at end of scope, mirroring
    // upstream's matching `pop_local_filters()` on delete.c:115.
    let _destination_dir_merge_guard =
        context.enter_destination_for_deletion(destination, relative)?;

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
/// emitter dispatches so dry-run output and event ordering match upstream
/// rsync byte-for-byte.
///
/// Directories recurse into their contents so per-entry counters match
/// upstream's `delete_in_dir` walk (one count per leaf, mirroring
/// `target/interop/upstream-src/rsync-3.4.1/generator.c:272-347`).
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
            // Reusable buffers for recursive subtree traversal - avoids
            // per-entry Path::join allocations inside the recursion.
            let mut subtree_path = path.clone();
            let mut subtree_relative = entry_relative.clone();
            record_directory_subtree(context, &mut subtree_path, &mut subtree_relative)?;
        }

        if !context.mode().is_dry_run() {
            context.backup_existing_entry(&path, Some(entry_relative.as_path()), file_type)?;
        }
        // upstream: log.c:log_delete emits exactly ONE line - the itemize
        // format when stdout_format_has_o_or_i, otherwise "deleting %n".
        // Suppress the plain line when itemize is active so only the
        // `*deleting` record renders; f_name appends a trailing slash for
        // directories and never inserts the word "directory".
        if !context.options().is_itemize_active() {
            if is_dir {
                info_log!(Del, 1, "deleting {}/", entry_relative.display());
            } else {
                info_log!(Del, 1, "deleting {}", entry_relative.display());
            }
        }
        context.summary_mut().record_deletion();
        context.record(
            LocalCopyRecord::new(
                entry_relative,
                LocalCopyAction::EntryDeleted,
                0,
                None,
                Duration::default(),
                None,
            )
            .with_directory(is_dir),
        );
        context.register_progress();
    }
    Ok(())
}

/// Recurses through `dir_path`'s contents, emitting per-leaf records and
/// summary counters before the emitter wipes the directory via
/// `remove_dir_all`. Matches upstream's per-entry counting in
/// `delete_in_dir`.
///
/// Uses `PathBuf::push`/`pop` on reusable buffers to avoid per-entry
/// allocations from `Path::join` in the recursive traversal.
fn record_directory_subtree(
    context: &mut CopyContext,
    path_buf: &mut PathBuf,
    relative_buf: &mut PathBuf,
) -> Result<(), LocalCopyError> {
    let read_dir = match fs::read_dir(path_buf.as_path()) {
        Ok(iter) => iter,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(LocalCopyError::io(
                "read extraneous directory",
                path_buf.clone(),
                error,
            ));
        }
    };
    for entry in read_dir {
        context.enforce_timeout()?;
        let entry = entry.map_err(|error| {
            LocalCopyError::io("read extraneous directory entry", path_buf.as_path(), error)
        })?;
        let name = entry.file_name();
        path_buf.push(&name);
        relative_buf.push(&name);
        let file_type = entry.file_type().map_err(|error| {
            LocalCopyError::io(
                "inspect extraneous directory entry",
                path_buf.clone(),
                error,
            )
        })?;
        let is_dir = file_type.is_dir();
        if is_dir {
            record_directory_subtree(context, path_buf, relative_buf)?;
        }
        // upstream: log.c:log_delete emits one line; suppress the plain
        // "deleting %n" when itemize is active. f_name appends a trailing
        // slash for dirs and never inserts the word "directory".
        if !context.options().is_itemize_active() {
            if is_dir {
                info_log!(Del, 1, "deleting {}/", relative_buf.display());
            } else {
                info_log!(Del, 1, "deleting {}", relative_buf.display());
            }
        }
        context.summary_mut().record_deletion();
        context.record(
            LocalCopyRecord::new(
                relative_buf.clone(),
                LocalCopyAction::EntryDeleted,
                0,
                None,
                Duration::default(),
                None,
            )
            .with_directory(is_dir),
        );
        context.register_progress();
        relative_buf.pop();
        path_buf.pop();
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
