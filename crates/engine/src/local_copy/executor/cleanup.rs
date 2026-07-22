//! Deletion helpers for extraneous or source entries.

use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use logging::{debug_log, info_log};
use rayon::prelude::*;

use std::sync::Arc;

/// Minimum number of deletion candidates in a single destination directory
/// before the per-entry filter matching switches to a rayon `par_iter`.
///
/// Mirrors the codebase's `PARALLEL_STAT_THRESHOLD = 64` convention: below
/// this count the serial path avoids rayon's fork/join overhead; above it the
/// pure `allows_deletion` decisions are computed across worker threads. The
/// threshold does not affect the decision SET or emission ORDER - only WHERE
/// the matching runs.
const PARALLEL_DELETION_MATCH_THRESHOLD: usize = 64;

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

/// Whether a destination directory sits on a different filesystem than the
/// transfer root - a mount point the `--one-file-system` delete pass must
/// preserve. Pure device-id comparison, dependency-inverted from the stat call
/// site so the decision is unit-testable with synthetic device ids (mounting a
/// real filesystem in a test is impractical).
///
/// # Upstream Reference
///
/// - `flist.c:1344` - `one_file_system && st.st_dev != filesystem_dev` sets
///   `FLAG_MOUNT_DIR` on the dest dirlist entry.
/// - `generator.c:331` - `delete_in_dir()` skips a `FLAG_MOUNT_DIR` directory.
#[cfg(unix)]
fn crosses_mount_boundary(boundary_dev: u64, entry_dev: u64) -> bool {
    entry_dev != boundary_dev
}

/// The transfer-root device that bounds a `--one-file-system` delete pass, or
/// `None` when `-x` is off (or the boundary cannot be stat'd). The copy walk
/// never recurses across a mount, so every scanned destination directory sits
/// on this device and a child whose device differs is a mount point.
#[cfg(unix)]
fn delete_boundary_device(context: &CopyContext, destination: &Path) -> Option<u64> {
    if !context.one_file_system_enabled() {
        return None;
    }
    let metadata = fs::symlink_metadata(destination).ok()?;
    crate::local_copy::overrides::device_identifier(destination, &metadata)
}

/// Non-unix platforms lack POSIX device ids; `--one-file-system` mount-point
/// protection is a unix concept, so the boundary is always absent.
#[cfg(not(unix))]
fn delete_boundary_device(_context: &CopyContext, _destination: &Path) -> Option<u64> {
    None
}

/// Whether `path` (a destination entry of type `file_type`) is a mount point the
/// delete pass must preserve under `--one-file-system`.
///
/// Only directories can be mount points (upstream restricts `FLAG_MOUNT_DIR` to
/// `S_ISDIR`). A `None` boundary (no `-x`) short-circuits to `false` so the
/// common delete pass is unchanged and pays no extra `stat`.
#[cfg(unix)]
fn is_delete_mount_point(boundary_dev: Option<u64>, path: &Path, file_type: fs::FileType) -> bool {
    let Some(boundary) = boundary_dev else {
        return false;
    };
    if !file_type.is_dir() {
        return false;
    }
    match fs::symlink_metadata(path) {
        Ok(metadata) => crate::local_copy::overrides::device_identifier(path, &metadata)
            .is_some_and(|dev| crosses_mount_boundary(boundary, dev)),
        Err(_) => false,
    }
}

/// Non-unix no-op: without POSIX device ids there is no mount-point boundary to
/// enforce, so no destination entry is ever treated as a mount point.
#[cfg(not(unix))]
fn is_delete_mount_point(
    _boundary_dev: Option<u64>,
    _path: &Path,
    _file_type: fs::FileType,
) -> bool {
    false
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
    // Route through the leaf-granular serial executor when either condition
    // holds; the wholesale `remove_dir_all` emitter below cannot serve them:
    //
    // 1. `--max-delete`: the cap must count every filesystem entry actually
    //    removed, including the leaves inside an extraneous directory, and stop
    //    mid-traversal once the limit is reached. The emitter removes an
    //    extraneous directory wholesale and counts it as a single deletion,
    //    silently exceeding the cap for directory subtrees. The leaf executor
    //    matches upstream delete.c:156/181 (guard-before-delete,
    //    increment-on-success).
    // 2. `--one-file-system`: a mount point nested inside an otherwise-doomed
    //    subtree must be preserved, but `remove_dir_all` recurses across the
    //    mount boundary and destroys it (data loss). The leaf executor checks
    //    the device boundary at every recursion level (see `remove_entry_capped`
    //    / `is_delete_mount_point`), so it never removes across a mount. With no
    //    `--max-delete` the cap is absent, so `cap_reached` is always false and
    //    the pass never reports a limit hit.
    if context.options().max_deletion_limit().is_some() || context.one_file_system_enabled() {
        return delete_extraneous_entries_capped(context, destination, relative, source_entries);
    }

    // Phase 1: scan the destination directory, compute extras, and
    // produce a single-directory DeletePlan in upstream emission order.
    // The plan respects partial-dir protection and allows_deletion filter
    // rules before publication so the emitter sees only the entries that
    // should actually unlink.
    let plan = match build_plan_for_directory(context, destination, relative, source_entries)? {
        Some(plan) => plan,
        None => return Ok(()),
    };

    execute_delete_plan(context, destination, relative, plan, fs)
}

/// Runs the per-entry side effects and the emitter unlink for an already-built
/// [`DeletePlan`]. Shared by the immediate `--delete`/`--delete-before`/
/// `--delete-after` path (which builds the plan right before executing) and the
/// `--delete-delay` deferred path (which built the plan at decision time -
/// during the transfer, while the destination merge files were still absent -
/// and only executes here).
fn execute_delete_plan<F>(
    context: &mut CopyContext,
    destination: &Path,
    relative: Option<&Path>,
    plan: DeletePlan,
    fs: F,
) -> Result<(), LocalCopyError>
where
    F: DeleteFs + Sync + Send + 'static,
{
    // Under --one-file-system the wholesale `remove_dir_all` emitter must not
    // run at all: it would recurse across a mount boundary nested inside a
    // doomed subtree and destroy the mounted filesystem. Route the already-built
    // plan through the boundary-aware leaf executor, which preserves such a
    // mount and pins its parent. The immediate `--delete*` modes reach the leaf
    // executor via the dispatch in `delete_extraneous_entries_via_emitter`; this
    // guard covers the deferred `--delete-delay` plan, whose decided extras are
    // consumed here rather than rescanned.
    if context.one_file_system_enabled() {
        let boundary_dev = delete_boundary_device(context, destination);
        let mut skipped = 0u64;
        for entry in &plan.extras {
            let name_path = PathBuf::from(entry.name.as_os_str());
            let path = destination.join(&name_path);
            let entry_relative = match relative {
                Some(base) => base.join(&name_path),
                None => name_path.clone(),
            };
            remove_entry_capped(
                context,
                &path,
                &entry_relative,
                &mut skipped,
                false,
                boundary_dev,
            )?;
        }
        return Ok(());
    }

    // Build a single-directory DeleteContext, observe the segment so the cursor
    // yields the directory, publish the plan directly into the context's map,
    // and drain via emit_one.
    //
    // The cursor root must equal the plan's directory key so
    // `cursor.next_ready()` yields a path that `plans.take()` resolves to the
    // published plan. `DeleteContext::new` would leave the cursor at the empty
    // relative path; use `with_cursor_root` to seat it on the absolute
    // destination the plan is keyed on.
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

    Ok(())
}

/// `--delete-delay` decision hook: computes the delete plan for `destination`
/// during the transfer walk (so the persistent delete-filter chain reflects the
/// DURING state - the destination's per-dir merge files have not been copied
/// yet) and defers the concrete plan for execution after the transfer.
///
/// upstream: generator.c:345 `delete_during == 2` calls `remember_delete(fp, ...)`
/// from inside `delete_in_dir` - the decision (including
/// `change_local_filter_dir`) happens during the walk, only the unlink is
/// postponed to `do_delayed_deletions()` (generator.c:2419). Deciding at flush
/// time instead would wrongly consult the by-then-present merge files.
pub(crate) fn decide_and_defer_delayed_deletions<S: AsRef<OsStr>>(
    context: &mut CopyContext,
    destination: &Path,
    relative: Option<&Path>,
    source_entries: &[S],
) -> Result<(), LocalCopyError> {
    if context.delete_pass_blocked_by_io_error() {
        return Ok(());
    }
    if let Some(plan) = build_plan_for_directory(context, destination, relative, source_entries)? {
        context.defer_decided_deletion(
            destination.to_path_buf(),
            relative.map(Path::to_path_buf),
            plan,
        );
    }
    Ok(())
}

/// Executes a `--delete-delay` plan decided during the walk. Mirrors
/// [`execute_delete_plan`] with the real filesystem dispatcher; the plan already
/// encodes the during-time filter decision, so no directory rescan or filter
/// re-evaluation happens here.
pub(crate) fn execute_decided_deletion(
    context: &mut CopyContext,
    destination: &Path,
    relative: Option<&Path>,
    plan: DeletePlan,
) -> Result<(), LocalCopyError> {
    execute_delete_plan(context, destination, relative, plan, RealDeleteFs)
}

/// Leaf-granular, serial deletion path used whenever `--max-delete` or
/// `--one-file-system` is active.
///
/// The emitter path counts an extraneous directory as a single deletion and
/// removes its subtree with `remove_dir_all`, so a directory holding N files
/// costs one unit against the cap even though N+1 filesystem entries vanish.
/// That undercount lets a small `--max-delete` value silently remove an
/// unbounded number of files, and the wholesale removal also recurses across a
/// mount boundary that `--one-file-system` must not cross. This path instead
/// walks every candidate depth-first, checks the cap before each individual
/// unlink, and checks the device boundary before recursing into any directory
/// (see `remove_entry_capped`), counting only successful deletions - exactly
/// mirroring upstream `delete.c:delete_item`/`delete_dir_contents` where
/// `stats.deleted_files` is compared against `max_delete` before every entry
/// and incremented only on a successful removal (`delete.c:156` guard,
/// `delete.c:181` increment), and a mount point pins its parent
/// (`delete.c:89-97`). With no `--max-delete` the cap is absent, so
/// `cap_reached` is always false and the pass never reports a limit hit.
///
/// The global running count is `context.summary().items_deleted()`, which the
/// per-entry side effects already maintain across directories, so the cap is
/// enforced consistently over the whole transfer, not just one directory.
fn delete_extraneous_entries_capped<S: AsRef<OsStr>>(
    context: &mut CopyContext,
    destination: &Path,
    relative: Option<&Path>,
    source_entries: &[S],
) -> Result<(), LocalCopyError> {
    let plan = match build_plan_for_directory(context, destination, relative, source_entries)? {
        Some(plan) => plan,
        None => return Ok(()),
    };

    // --one-file-system boundary device threaded through the recursion so a
    // mount point nested inside a doomed subtree is preserved and pins its
    // parent, matching upstream delete.c:89-97. `build_plan_for_directory`
    // already excluded direct-child mounts, so this guards the deeper levels.
    let boundary_dev = delete_boundary_device(context, destination);

    // Entries are visited in upstream `delete_in_dir` emission order (the
    // reverse-sorted order `sort_by_name` locks in) so the prefix that
    // survives when the cap trips matches upstream's traversal.
    let mut skipped = 0u64;
    for entry in &plan.extras {
        let name_path = PathBuf::from(entry.name.as_os_str());
        let path = destination.join(&name_path);
        let entry_relative = match relative {
            Some(base) => base.join(&name_path),
            None => name_path.clone(),
        };
        // `nested = false`: these are the top-level extraneous entries, reached
        // by upstream's `delete_item` WITHOUT `DEL_DIR_IS_EMPTY`, so a survived
        // top-level directory is not counted against the skipped total.
        remove_entry_capped(
            context,
            &path,
            &entry_relative,
            &mut skipped,
            false,
            boundary_dev,
        )?;
    }

    if skipped > 0 {
        // upstream: generator.c:2431 emits one warning after the pass with
        // the total number of entries skipped because of the limit; the run
        // then exits RERR_DEL_LIMIT (25).
        info_log!(
            Del,
            1,
            "max deletions reached, skipping {} remaining",
            skipped
        );
        return Err(LocalCopyError::delete_limit_exceeded(skipped));
    }

    Ok(())
}

/// Orders two directory children the way upstream `get_dirlist` sorts them
/// for a delete pass: protocol-29 `f_name_cmp` places non-directories before
/// directories (t_ITEM before t_PATH, upstream: flist.c:3223), then by name.
/// Callers `reverse()` the sorted slice to reproduce upstream's reverse
/// dirlist iteration (`for (i = dirlist->used; i--; )`, delete.c:85 /
/// generator.c:326), so directories are visited first, then files - the
/// order that decides which entries survive a `--max-delete` cap and the
/// order deletion log lines are emitted.
fn cmp_child_delete_order(a: (&OsStr, bool), b: (&OsStr, bool)) -> std::cmp::Ordering {
    a.1.cmp(&b.1).then_with(|| a.0.cmp(b.0))
}

/// Recursively removes one extraneous entry under the `--max-delete` cap,
/// returning `true` when the entry was fully removed and `false` when it (or
/// part of its subtree) was left in place because the cap was reached.
///
/// Mirrors upstream `delete_item`: a directory first has its contents peeled
/// depth-first (`delete_dir_contents`, reverse-sorted iteration), then the
/// now-empty directory is itself subject to the same guard before its own
/// removal. `skipped` accumulates the count reported to the user, matching
/// upstream's `skipped_deletes` (`delete.c:157`).
///
/// `nested` distinguishes a directory reached during the recursion (upstream's
/// `delete_item(..., DEL_DIR_IS_EMPTY)` call at `delete.c:107`) from a
/// top-level extraneous entry (upstream's initial `delete_item` without
/// `DEL_DIR_IS_EMPTY`). Only the former counts a cap-saturated non-empty
/// directory toward `skipped` - see the `!all_children_removed` branch below.
fn remove_entry_capped(
    context: &mut CopyContext,
    path: &Path,
    entry_relative: &Path,
    skipped: &mut u64,
    nested: bool,
    boundary_dev: Option<u64>,
) -> Result<bool, LocalCopyError> {
    context.enforce_timeout()?;

    let file_type = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata.file_type(),
        // Vanished between scan and delete: upstream treats this as a
        // successful no-op removal (nothing is left behind).
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(true),
        Err(error) => {
            return Err(LocalCopyError::io(
                "inspect extraneous destination entry",
                path.to_path_buf(),
                error,
            ));
        }
    };

    // upstream: generator.c:331 / delete.c:89-97 - a mount point is never
    // deleted and pins its enclosing directory as non-empty. Under
    // --one-file-system a directory on a different device than the transfer
    // root is that mount point: preserve it and return `false` so the caller
    // leaves the parent in place. The check runs at every recursion level, so a
    // mount nested inside a doomed subtree pins the whole ancestor chain.
    if is_delete_mount_point(boundary_dev, path, file_type) {
        info_log!(
            Mount,
            1,
            "cannot delete mount point: {}",
            entry_relative.display()
        );
        return Ok(false);
    }

    if file_type.is_dir() {
        // Peel the directory's contents depth-first in upstream reverse-sorted
        // order before considering the directory itself.
        let mut children: Vec<(OsString, bool)> = Vec::new();
        match fs::read_dir(path) {
            Ok(iter) => {
                for child in iter {
                    let child = child.map_err(|error| {
                        LocalCopyError::io("read extraneous directory entry", path, error)
                    })?;
                    // `file_type()` uses readdir's d_type (falling back to
                    // lstat) and does not follow symlinks, so a symlink to a
                    // directory is classified as a non-directory - matching
                    // upstream's S_ISDIR test on the lstat mode.
                    let is_dir = child.file_type().map(|t| t.is_dir()).unwrap_or(false);
                    children.push((child.file_name(), is_dir));
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(true),
            Err(error) => {
                return Err(LocalCopyError::io(
                    "read extraneous directory",
                    path.to_path_buf(),
                    error,
                ));
            }
        }
        // Peel contents in upstream's reverse-sorted delete order so the
        // prefix that survives when `--max-delete` trips matches upstream.
        children.sort_unstable_by(|a, b| cmp_child_delete_order((&a.0, a.1), (&b.0, b.1)));
        children.reverse();

        let mut all_children_removed = true;
        for (child_name, _) in children {
            let child_path = path.join(&child_name);
            let child_relative = entry_relative.join(&child_name);
            // Children are reached via the recursion, mirroring upstream's
            // `delete_item(..., DEL_DIR_IS_EMPTY)` at delete.c:107; mark them
            // `nested` so a cap-saturated non-empty subdir is counted.
            if !remove_entry_capped(
                context,
                &child_path,
                &child_relative,
                skipped,
                true,
                boundary_dev,
            )? {
                all_children_removed = false;
            }
        }

        if !all_children_removed {
            // Contents survived because the cap was reached, so the directory
            // cannot be removed. upstream: delete.c:117 prints this once per
            // non-empty directory without flagging an I/O error.
            info_log!(
                Nonreg,
                1,
                "cannot delete non-empty directory: {}",
                path.display().to_string().replace('\\', "/")
            );
            // A NESTED non-empty directory is still reached by upstream's
            // `delete_item(..., DEL_DIR_IS_EMPTY)` (delete.c:107): with
            // DEL_DIR_IS_EMPTY set, delete.c:144 is skipped and control falls
            // straight to the max_delete guard (delete.c:156), which does
            // `skipped_deletes++` because the cap is saturated. The TOP-LEVEL
            // directory instead enters `delete_item` WITHOUT DEL_DIR_IS_EMPTY,
            // so its survived contents take the `goto check_ret` path
            // (delete.c:151-152) and it is never counted. Count the nested case
            // to match upstream's skipped total exactly (issue #212).
            //
            // Only count it when the cap is the reason the contents survived. A
            // directory that survived solely because it pins a `--one-file-system`
            // mount point is upstream's DR_NOT_EMPTY (delete.c:95-96), which does
            // not touch `skipped_deletes`; counting it would falsely trip
            // RERR_DEL_LIMIT. `cap_reached` is true exactly when a cap skip
            // occurred, so it distinguishes the two without changing the
            // no-mount behaviour verified by issue #212.
            if nested && cap_reached(context) {
                *skipped = skipped.saturating_add(1);
            }
            return Ok(false);
        }

        // The directory is empty; it is itself a deletion subject to the cap.
        if cap_reached(context) {
            *skipped = skipped.saturating_add(1);
            return Ok(false);
        }
        delete_leaf(context, path, entry_relative, file_type)?;
        return Ok(true);
    }

    // Regular file, symlink, device, or special: a single deletion.
    if cap_reached(context) {
        *skipped = skipped.saturating_add(1);
        return Ok(false);
    }
    delete_leaf(context, path, entry_relative, file_type)?;
    Ok(true)
}

/// Returns `true` when the running deletion count has reached the configured
/// `--max-delete` limit. upstream: delete.c:156 `stats.deleted_files >= max_delete`.
fn cap_reached(context: &CopyContext) -> bool {
    context
        .options()
        .max_deletion_limit()
        .is_some_and(|limit| context.summary().items_deleted() >= limit)
}

/// Runs the per-entry side effects (backup, itemize log, [`LocalCopyRecord`],
/// summary counter) and issues the actual removal for one leaf. Increments the
/// global deletion count via `record_deletion` so the cap sees the update.
///
/// Kept in lockstep with [`apply_delete_side_effects`] for a single entry; the
/// recursion in [`remove_entry_capped`] supplies the per-leaf walk that
/// [`record_directory_subtree`] provides on the uncapped path.
fn delete_leaf(
    context: &mut CopyContext,
    path: &Path,
    entry_relative: &Path,
    file_type: fs::FileType,
) -> Result<(), LocalCopyError> {
    let is_dir = file_type.is_dir();

    // upstream: delete.c:165 - back up an extraneous file before deletion only
    // when `backup_dir || !is_backup_file(fbuf)`.
    if !context.mode().is_dry_run()
        && !is_dir
        && let Some(name) = path.file_name()
        && context.options().should_backup_before_delete(name)
    {
        // upstream: delete.c:167 - the delete pass calls `make_backup(fbuf,
        // True)`, skipping the hard-link tier since the item is unlinked
        // outright right after regardless of which strategy placed it.
        context.backup_existing_entry(path, Some(entry_relative), file_type, true)?;
    }

    if !context.options().is_itemize_active() {
        if is_dir {
            info_log!(Del, 1, "deleting {}/", entry_relative.display());
        } else {
            info_log!(Del, 1, "deleting {}", entry_relative.display());
        }
    }

    if !context.mode().is_dry_run() {
        let fs = RealDeleteFs;
        let result = if is_dir {
            fs.rmdir(path)
        } else if file_type.is_symlink() {
            fs.unlink_symlink(path)
        } else {
            fs.unlink_file(path)
        };
        if let Err(error) = result {
            // A vanished entry is a benign no-op (upstream ENOENT path); any
            // other failure surfaces so the exit code reflects it.
            if error.kind() != io::ErrorKind::NotFound {
                return Err(LocalCopyError::io(
                    "delete extraneous destination entry",
                    path.to_path_buf(),
                    error,
                ));
            }
        }
    }

    context.summary_mut().record_deletion(file_type);
    context.record(
        LocalCopyRecord::new(
            entry_relative.to_path_buf(),
            LocalCopyAction::EntryDeleted,
            0,
            None,
            Duration::default(),
            None,
        )
        .with_directory(is_dir),
    );
    context.register_progress();
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
) -> Result<Option<DeletePlan>, LocalCopyError> {
    // upstream: delete.c:63 / generator.c:308 - the delete pass calls
    // `change_local_filter_dir(fbuf, F_DEPTH(file))` with the destination
    // directory so the receiver applies any per-dir merge rules found in the
    // directory being scanned, INHERITING an ancestor directory's rules into
    // subdirectories (exclude.c:801 keeps them in `lp->head`). Advancing the
    // persistent, depth-keyed delete-filter chain keeps ancestor frames alive
    // (rather than popping each directory's rules immediately), which is what
    // lets a parent `.rsync-filter` protect a subdirectory's entries. During a
    // `--delete-during`/`--delete-before` sweep the destination merge files are
    // not present yet, so the loaded frames are empty and protect nothing -
    // matching upstream, hence the manual's `--delete-after` recommendation.
    context.sync_delete_filter_chain(destination, relative)?;

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

    // --one-file-system boundary device (see `delete_boundary_device`). `None`
    // unless `-x` is active, so the common delete pass is unaffected.
    let boundary_dev = delete_boundary_device(context, destination);

    // Phase A (serial): scan the destination in readdir order and collect the
    // candidates that survive the cheap keep / partial-dir filters. The filter
    // decision (`allows_deletion`) is deferred to Phase B so it can be batched.
    // readdir order is preserved through every phase so the max-delete limit and
    // debug logging observe the exact same sequence as the original serial loop.
    let mut candidates: Vec<DeletionCandidate> = Vec::new();
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

        // upstream: generator.c:331-336 delete_in_dir() skips a dest directory
        // flagged FLAG_MOUNT_DIR ("cannot delete mount point"). Under
        // --one-file-system a destination directory on a different device than
        // the transfer root is that mount point: exclude it from the plan so it
        // is neither unlinked nor `remove_dir_all`'d. Its parent is the
        // source-present scanned directory, which is never a deletion candidate,
        // so the parent is left in place (delete.c:89-97 parent pinning).
        if is_delete_mount_point(boundary_dev, &destination.join(&name_path), file_type) {
            info_log!(
                Mount,
                1,
                "cannot delete mount point: {}",
                entry_relative.display()
            );
            continue;
        }

        let is_dir = file_type.is_dir();
        candidates.push(DeletionCandidate {
            name,
            entry_relative,
            file_type,
            is_dir,
        });
    }

    // Phase B (parallel above threshold): compute the pure `allows_deletion`
    // decision for every candidate against an immutable, Send + Sync snapshot
    // of the directory's filter chain. The closure captures ONLY the snapshot
    // (no Rc/RefCell CopyContext), so it is safe to run on rayon workers.
    // `par_iter().map().collect()` preserves index order, keeping decisions
    // aligned with the readdir-order candidates for Phase C.
    let snapshot = context.deletion_filter_snapshot();
    let allowed: Vec<bool> = if candidates.len() >= PARALLEL_DELETION_MATCH_THRESHOLD {
        candidates
            .par_iter()
            .map(|candidate| {
                snapshot.allows_deletion(candidate.entry_relative.as_path(), candidate.is_dir)
            })
            .collect()
    } else {
        candidates
            .iter()
            .map(|candidate| {
                snapshot.allows_deletion(candidate.entry_relative.as_path(), candidate.is_dir)
            })
            .collect()
    };

    // Phase C (serial): apply the decisions in readdir order. Filter-protected
    // entries log and are dropped; the rest join the plan. The final
    // `sort_by_name` fixes the wire emission order. The `--max-delete` cap is
    // NOT applied here: it is enforced at leaf granularity during execution by
    // `delete_extraneous_entries_capped` (upstream counts every removed entry,
    // not just the top-level ones), so this plan carries the full extraneous
    // set for the directory.
    let mut plan = DeletePlan::new(destination.to_path_buf());
    for (candidate, allowed) in candidates.into_iter().zip(allowed) {
        if !allowed {
            debug_log!(
                Filter,
                2,
                "filter protected {} from deletion",
                candidate.entry_relative.display()
            );
            continue;
        }

        plan.push(DeleteEntry::new(
            candidate.name,
            classify_kind(candidate.file_type),
        ));
    }
    plan.sort_by_name();
    Ok(Some(plan))
}

/// A destination entry that survived the cheap keep / partial-dir filters and
/// awaits the `allows_deletion` decision in [`build_plan_for_directory`].
///
/// Holds only `Send` data so a slice of candidates can be matched in parallel.
struct DeletionCandidate {
    name: OsString,
    entry_relative: PathBuf,
    file_type: fs::FileType,
    is_dir: bool,
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

        // upstream: delete.c:165 - back up an extraneous file before deletion
        // only when `backup_dir || !is_backup_file(fbuf)`. A name that already
        // ends in the backup suffix is unlinked directly (no re-backup to
        // `<name><suffix><suffix>`); the emitter below performs that unlink.
        if !context.mode().is_dry_run()
            && context
                .options()
                .should_backup_before_delete(entry.name.as_os_str())
        {
            // upstream: delete.c:167 - prefer_rename=True; the item is
            // unlinked outright right after, so skip the hard-link tier.
            context.backup_existing_entry(
                &path,
                Some(entry_relative.as_path()),
                file_type,
                true,
            )?;
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
        context.summary_mut().record_deletion(file_type);
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
pub(crate) fn record_directory_subtree(
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
    // Collect the children with their type, then order them the way upstream
    // `delete_in_dir` walks a directory (reverse of `get_dirlist`'s sorted
    // order), so the emitted `*deleting` lines match upstream byte-for-byte.
    let mut children: Vec<(OsString, fs::FileType)> = Vec::new();
    for entry in read_dir {
        context.enforce_timeout()?;
        let entry = entry.map_err(|error| {
            LocalCopyError::io("read extraneous directory entry", path_buf.as_path(), error)
        })?;
        let file_type = entry.file_type().map_err(|error| {
            LocalCopyError::io(
                "inspect extraneous directory entry",
                path_buf.join(entry.file_name()),
                error,
            )
        })?;
        children.push((entry.file_name(), file_type));
    }
    children.sort_unstable_by(|a, b| {
        cmp_child_delete_order((&a.0, a.1.is_dir()), (&b.0, b.1.is_dir()))
    });
    children.reverse();

    for (name, file_type) in children {
        path_buf.push(&name);
        relative_buf.push(&name);
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
        context.summary_mut().record_deletion(file_type);
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

/// Removes the source entry after a successful copy when `--remove-source-files`
/// is active, applying upstream's `successful_send` safety guards first.
///
/// Mirrors upstream `successful_send()` (sender.c:131-182). Before unlinking the
/// source the guards run in order:
///
/// 1. **Re-stat** the source (`do_lstat`). A vanished source (`ENOENT`) is the
///    benign "already removed" no-op; any other stat failure is a soft
///    `RERR_PARTIAL` (exit 23) error that leaves the source in place.
/// 2. **Destination inode** (Unix): refuse to remove the source when it is the
///    very inode just written to the destination (a local same-file transfer),
///    which would otherwise delete the freshly written destination.
/// 3. **Changed file**: refuse to remove a source whose size or modification
///    time changed since it was copied (`recorded`), so a file the user
///    modified mid-transfer is never removed.
///
/// Directories are never removed (upstream only removes files). A guard refusal
/// or an unlink failure records a soft error so the run finishes `RERR_PARTIAL`
/// (exit 23) without aborting, mirroring upstream's `FERROR_XFER` ->
/// `got_xfer_error`. No-ops in dry-run mode.
///
/// # Upstream Reference
///
/// - `sender.c:131-182` `successful_send()`
/// - `log.c:311` / `main.c:1630` `got_xfer_error` -> `RERR_PARTIAL`
pub(crate) fn remove_source_entry_if_requested(
    context: &mut CopyContext,
    source: &Path,
    destination: &Path,
    recorded: &fs::Metadata,
    record_path: Option<&Path>,
    file_type: fs::FileType,
) -> Result<(), LocalCopyError> {
    if !context.remove_source_files_enabled() || context.mode().is_dry_run() {
        return Ok(());
    }

    // upstream: sender.c:150 - re-stat the source before removing it.
    let current = match fs::symlink_metadata(source) {
        Ok(metadata) => metadata,
        // upstream: sender.c:174-175 - ENOENT is the benign "already removed" case.
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        // upstream: sender.c:151-153,176-177 - any other re-lstat failure is an
        // FERROR_XFER: leave the source in place and finish RERR_PARTIAL (23).
        Err(error) => {
            eprintln!(
                "rsync: [sender] sender failed to re-lstat \"{}\": {}",
                source.display(),
                crate::local_copy::upstream_io_error(&error),
            );
            context.record_sender_remove_error();
            return Ok(());
        }
    };

    // upstream: sender.c:145 - directories are never removed. Prefer the
    // freshly-stat'd type; fall back to the copy-time type only if it is a dir.
    let _ = file_type;
    if current.file_type().is_dir() {
        return Ok(());
    }

    // upstream: sender.c:155-160 - refuse removal when the source is the very
    // inode just written to the destination (local_server num_dev_ino_buf).
    #[cfg(unix)]
    {
        if let Ok(destination_meta) = fs::symlink_metadata(destination) {
            if is_destination_inode(&current, &destination_meta) {
                eprintln!(
                    "ERROR: Skipping sender remove of destination file: {}",
                    source.display()
                );
                context.record_sender_remove_error();
                return Ok(());
            }
        }
    }
    #[cfg(not(unix))]
    let _ = destination;

    // upstream: sender.c:162-169 - refuse to remove a source that changed size
    // or modification time since it was copied.
    if source_identity_changed(recorded, &current) {
        eprintln!(
            "ERROR: Skipping sender remove for changed file: {}",
            source.display()
        );
        context.record_sender_remove_error();
        return Ok(());
    }

    // upstream: sender.c:171 - do_unlink(fname) once every guard passed.
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
        // upstream: sender.c:174-175 - ENOENT after the guards is still benign.
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        // upstream: sender.c:172-173,176-177 - a failed unlink is an FERROR_XFER:
        // record the soft error so the run finishes RERR_PARTIAL (23).
        Err(error) => {
            eprintln!(
                "rsync: [sender] sender failed to remove \"{}\": {}",
                source.display(),
                crate::local_copy::upstream_io_error(&error),
            );
            context.record_sender_remove_error();
            Ok(())
        }
    }
}

/// Returns true when a freshly re-stat'd source no longer matches the identity
/// recorded when it was copied, mirroring the changed-file guard in upstream
/// `successful_send`: size, whole-second mtime, and sub-second mtime compared
/// only when the recorded timestamp carried nanoseconds (upstream gates the
/// nsec compare on `NSEC_BUMP`, i.e. a transmitted `FLAG_MOD_NSEC`).
///
/// upstream: sender.c:162-169
#[cfg(unix)]
fn source_identity_changed(recorded: &fs::Metadata, current: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    recorded.len() != current.len()
        || recorded.mtime() != current.mtime()
        || (recorded.mtime_nsec() != 0 && recorded.mtime_nsec() != current.mtime_nsec())
}

/// Returns true when the re-stat'd source shares the destination's device and
/// inode - i.e. the source *is* the file just written to the destination, so
/// removing it would delete the destination.
///
/// upstream: sender.c:155-160 `(int64)st.st_dev == IVAL64(num_dev_ino_buf, 4)`
#[cfg(unix)]
fn is_destination_inode(source: &fs::Metadata, destination: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    source.dev() == destination.dev() && source.ino() == destination.ino()
}

/// Non-Unix changed-file guard: compares size and the full-precision
/// modification time. `recorded` and `current` are both the untouched source at
/// different instants, so an equality compare never spuriously fires.
///
/// upstream: sender.c:162-169
#[cfg(not(unix))]
fn source_identity_changed(recorded: &fs::Metadata, current: &fs::Metadata) -> bool {
    if recorded.len() != current.len() {
        return true;
    }
    match (recorded.modified(), current.modified()) {
        (Ok(recorded_mtime), Ok(current_mtime)) => recorded_mtime != current_mtime,
        _ => false,
    }
}

#[cfg(all(test, unix))]
mod sender_remove_guard_tests {
    use super::{is_destination_inode, source_identity_changed};
    use filetime::{FileTime, set_file_mtime};
    use std::fs;

    #[test]
    fn unchanged_source_is_removable() {
        // Data safety: a source that still matches its copy-time identity is the
        // one we transferred, so upstream successful_send unlinks it.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("f");
        fs::write(&path, b"data").expect("write");
        let recorded = fs::symlink_metadata(&path).expect("stat");
        let current = fs::symlink_metadata(&path).expect("stat");
        assert!(!source_identity_changed(&recorded, &current));
    }

    #[test]
    fn grown_source_is_not_removed() {
        // Data safety: the file grew after it was copied; removing it now would
        // destroy bytes we never sent (sender.c:162 st_size compare).
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("f");
        fs::write(&path, b"data").expect("write");
        let recorded = fs::symlink_metadata(&path).expect("stat");
        fs::write(&path, b"data-and-more").expect("grow");
        let current = fs::symlink_metadata(&path).expect("stat");
        assert!(source_identity_changed(&recorded, &current));
    }

    #[test]
    fn retouched_source_is_not_removed() {
        // Data safety: same size but a newer mtime means the file was rewritten
        // in place; upstream refuses the remove (sender.c:162 st_mtime compare).
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("f");
        fs::write(&path, b"data").expect("write");
        set_file_mtime(&path, FileTime::from_unix_time(1_600_000_000, 0)).expect("set mtime");
        let recorded = fs::symlink_metadata(&path).expect("stat");
        set_file_mtime(&path, FileTime::from_unix_time(1_700_000_000, 0)).expect("set mtime");
        let current = fs::symlink_metadata(&path).expect("stat");
        assert!(source_identity_changed(&recorded, &current));
    }

    #[test]
    fn hardlinked_source_and_destination_share_inode() {
        // Data safety: when the destination is a hard link to the source they
        // share dev/ino, so upstream refuses the sender remove that would
        // otherwise delete the destination (sender.c:155-160).
        let dir = tempfile::tempdir().expect("tempdir");
        let source = dir.path().join("src");
        let destination = dir.path().join("dst");
        fs::write(&source, b"data").expect("write");
        fs::hard_link(&source, &destination).expect("hardlink");
        let source_meta = fs::symlink_metadata(&source).expect("stat src");
        let dest_meta = fs::symlink_metadata(&destination).expect("stat dst");
        assert!(is_destination_inode(&source_meta, &dest_meta));
    }

    #[test]
    fn distinct_files_do_not_share_inode() {
        // A normal transfer writes a separate destination inode, so the guard
        // must not fire and the source removal proceeds.
        let dir = tempfile::tempdir().expect("tempdir");
        let source = dir.path().join("src");
        let destination = dir.path().join("dst");
        fs::write(&source, b"data").expect("write src");
        fs::write(&destination, b"data").expect("write dst");
        let source_meta = fs::symlink_metadata(&source).expect("stat src");
        let dest_meta = fs::symlink_metadata(&destination).expect("stat dst");
        assert!(!is_destination_inode(&source_meta, &dest_meta));
    }
}

#[cfg(all(test, unix))]
mod mount_boundary_tests {
    use super::*;
    use crate::local_copy::overrides::with_device_id_override;

    /// Mount-point data-loss protection: under `--one-file-system` the delete
    /// pass must never remove a destination directory on a different filesystem
    /// than the transfer root. Deleting one would recurse across the mount
    /// boundary and destroy a mounted filesystem absent from the source - the
    /// exact `rsync -ax --delete src/ dst/` data loss upstream guards against
    /// ("cannot delete mount point").
    ///
    /// The boundary decision is a pure `st_dev` comparison, so it is verified
    /// here with synthetic device ids: an entry on the transfer-root device is
    /// an ordinary deletion candidate; an entry on any other device is a mount
    /// point that must be preserved.
    ///
    /// upstream: flist.c:1344 (`st.st_dev != filesystem_dev` -> FLAG_MOUNT_DIR),
    /// generator.c:331 (delete_in_dir skips it).
    #[test]
    fn mount_boundary_predicate_distinguishes_devices() {
        const ROOT_DEV: u64 = 0x10;
        assert!(
            !crosses_mount_boundary(ROOT_DEV, ROOT_DEV),
            "an entry on the transfer-root device must remain deletable",
        );
        assert!(
            crosses_mount_boundary(ROOT_DEV, 0x20),
            "an entry on a foreign device is a mount point and must be preserved",
        );
        assert!(
            crosses_mount_boundary(ROOT_DEV, 0),
            "device 0 still differs from the boundary and must be preserved",
        );
    }

    /// The planner-level decision function `is_delete_mount_point` - the exact
    /// gate `build_plan_for_directory` and `remove_entry_capped` consult -
    /// exercised with injected device ids via `with_device_id_override` so no
    /// real mount is needed. A foreign-device directory is a mount point (kept);
    /// a same-device directory is an ordinary extraneous entry (deletable); a
    /// non-directory is never a mount point even on a foreign device; and with
    /// no boundary (`-x` off) nothing is treated as a mount point.
    #[test]
    fn is_delete_mount_point_excludes_only_foreign_device_dirs() {
        const ROOT_DEV: u64 = 0x1;
        const FOREIGN_DEV: u64 = 0x2;

        let tmp = tempfile::tempdir().expect("tempdir");
        let same_dir = tmp.path().join("same_dir");
        let mnt_dir = tmp.path().join("mnt");
        let foreign_file = tmp.path().join("file");
        fs::create_dir(&same_dir).expect("same_dir");
        fs::create_dir(&mnt_dir).expect("mnt_dir");
        fs::write(&foreign_file, b"x").expect("file");

        // Report `mnt` and `file` on a foreign device, everything else on root.
        with_device_id_override(
            |path, _meta| {
                let foreign = path.file_name().is_some_and(|n| n == "mnt" || n == "file");
                Some(if foreign { FOREIGN_DEV } else { ROOT_DEV })
            },
            || {
                let same_ft = fs::symlink_metadata(&same_dir).unwrap().file_type();
                let mnt_ft = fs::symlink_metadata(&mnt_dir).unwrap().file_type();
                let file_ft = fs::symlink_metadata(&foreign_file).unwrap().file_type();

                // `-x` off (no boundary): never a mount point, deletion proceeds.
                assert!(
                    !is_delete_mount_point(None, &mnt_dir, mnt_ft),
                    "with no boundary nothing is a mount point",
                );
                // Foreign-device directory: a mount point, preserved.
                assert!(
                    is_delete_mount_point(Some(ROOT_DEV), &mnt_dir, mnt_ft),
                    "a foreign-device directory must be treated as a mount point",
                );
                // Same-device directory: ordinary extraneous entry, deletable.
                assert!(
                    !is_delete_mount_point(Some(ROOT_DEV), &same_dir, same_ft),
                    "a same-device directory must remain deletable",
                );
                // A non-directory on a foreign device is not a mount point:
                // upstream restricts FLAG_MOUNT_DIR to S_ISDIR (flist.c:1341).
                assert!(
                    !is_delete_mount_point(Some(ROOT_DEV), &foreign_file, file_ft),
                    "only directories can be mount points",
                );
            },
        );
    }
}
