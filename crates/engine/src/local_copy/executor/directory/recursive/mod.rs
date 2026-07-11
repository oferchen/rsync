//! Recursive directory copying - orchestrates destination preparation, entry
//! processing, deletion, checksum prefetching, batch capture, and metadata
//! finalization.
//!
//! Mirrors the receiver-side file processing loop in upstream
//! `receiver.c:recv_files()` and the generator-side directory traversal
//! in `generator.c:recv_generator()`.

mod batch;
mod checksum;
mod deletion;
mod destination;
mod dir_metadata;
mod entry;

use std::cell::Cell;
use std::fs;
use std::io;
use std::path::Path;
use std::time::{Duration, Instant};

use crate::local_copy::overrides::device_identifier;
use crate::local_copy::{
    CopyContext, CreatedEntryKind, DeleteTiming, LocalCopyAction, LocalCopyChangeSet,
    LocalCopyError, LocalCopyMetadata, LocalCopyRecord,
};

pub(crate) use batch::capture_batch_file_entry;
pub(crate) use checksum::prefetch_directory_checksums;
use deletion::{
    apply_during_transfer_deletions, handle_empty_directory_pruning, handle_post_transfer_deletions,
};
use destination::{check_destination_state, record_skipped_missing_destination};
use dir_metadata::{apply_final_directory_metadata, record_directory_completion};
use entry::process_planned_entry;

use super::planner::{
    apply_pre_transfer_deletions, plan_directory_entries, reorder_hardlink_group_holders,
};
use super::support::read_directory_entries_sorted_reuse;

/// Maximum directory nesting depth the recursive executor descends before
/// returning an error instead of risking a thread stack overflow.
///
/// `copy_directory_recursive_inner` recurses once per directory level with a
/// large per-frame footprint (per-entry closures, the directory plan, metadata
/// snapshots). The safe ceiling is governed by the thread stack size: Windows
/// threads default to 1 MB, which a sufficiently deep tree overflows in debug
/// builds, while Unix threads default to 8 MB. The caps stay far above any
/// realistic directory tree while bounding the worst case below each platform's
/// overflow threshold, mirroring upstream rsync rejecting paths beyond
/// `MAXPATHLEN` (util1.c) rather than recursing unboundedly.
#[cfg(windows)]
const MAX_DIRECTORY_DEPTH: usize = 100;
#[cfg(not(windows))]
const MAX_DIRECTORY_DEPTH: usize = 1000;

/// Recursively copies a directory and its contents from source to destination.
///
/// This is the main entry point for recursive directory copying. It handles:
/// - Destination state checking and preparation
/// - Directory entry planning and filtering
/// - Parallel checksum prefetching (when enabled)
/// - Processing each entry (files, directories, symlinks, etc.)
/// - Post-transfer deletions
/// - Empty directory pruning
/// - Final metadata application
pub(crate) fn copy_directory_recursive(
    context: &mut CopyContext,
    source: &Path,
    destination: &Path,
    metadata: &fs::Metadata,
    relative: Option<&Path>,
    root_device: Option<u64>,
) -> Result<bool, LocalCopyError> {
    copy_directory_recursive_inner(
        context,
        source,
        destination,
        metadata,
        relative,
        root_device,
        false,
    )
}

/// Walks a directory's immediate children even when global recursion is off.
///
/// Mirrors upstream `flist.c:2442` which honours `(xfer_dirs && name_type != NORMAL_NAME)`
/// to walk one level beneath SLASH_ENDING_NAME / DOTDIR_NAME source arguments
/// (and `--files-from` entries with the corresponding markers). Subdirectories
/// encountered during this one-level walk are NOT recursed into further,
/// matching upstream's `recurse=0` semantics inside `send_directory()`.
pub(crate) fn copy_directory_walk_one_level(
    context: &mut CopyContext,
    source: &Path,
    destination: &Path,
    metadata: &fs::Metadata,
    relative: Option<&Path>,
    root_device: Option<u64>,
) -> Result<bool, LocalCopyError> {
    copy_directory_recursive_inner(
        context,
        source,
        destination,
        metadata,
        relative,
        root_device,
        true,
    )
}

fn copy_directory_recursive_inner(
    context: &mut CopyContext,
    source: &Path,
    destination: &Path,
    metadata: &fs::Metadata,
    relative: Option<&Path>,
    root_device: Option<u64>,
    force_walk_one_level: bool,
) -> Result<bool, LocalCopyError> {
    // Bound recursion depth before allocating this frame's locals or recursing
    // further. The planner builds each child entry's `relative` as the parent's
    // relative path plus the child name, so the component count of `relative`
    // equals the current directory depth - no separate counter need be threaded
    // through the call chain. A too-deep tree returns a typed I/O error
    // (ENAMETOOLONG-equivalent, exit 23 RERR_PARTIAL) which the per-entry loop
    // records and continues past, mirroring upstream rsync's handling of paths
    // that exceed `MAXPATHLEN`, instead of overflowing the thread stack.
    if relative.map_or(0, |rel| rel.components().count()) > MAX_DIRECTORY_DEPTH {
        return Err(LocalCopyError::io(
            "recurse into directory exceeding the nesting limit",
            destination,
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("directory nesting exceeds {MAX_DIRECTORY_DEPTH} levels"),
            ),
        ));
    }

    #[cfg(any(
        all(unix, any(feature = "acl", feature = "xattr")),
        all(windows, feature = "acl")
    ))]
    let mode = context.mode();
    #[cfg(not(any(
        all(unix, any(feature = "acl", feature = "xattr")),
        all(windows, feature = "acl")
    )))]
    let _mode = context.mode();

    #[cfg(all(unix, feature = "xattr"))]
    let preserve_xattrs = context.xattrs_enabled();
    #[cfg(all(any(unix, windows), feature = "acl"))]
    let preserve_acls = context.acls_enabled();

    let prune_enabled = context.prune_empty_dirs_enabled();

    let root_device = if context.one_file_system_enabled() {
        root_device.or_else(|| device_identifier(source, metadata))
    } else {
        None
    };

    let destination_state = check_destination_state(context, destination, relative)?;
    let destination_missing = destination_state.is_missing();
    // Box the owned metadata so it lives on the heap, not on this stack frame.
    // `copy_directory_recursive_inner` recurses once per directory level; an
    // inline `fs::Metadata` (~80 bytes on Windows) multiplied across a deep
    // tree overflows Windows's 1 MB default thread stack. The boxed pointer is
    // 8 bytes per frame instead.
    let existing_destination_metadata: Option<Box<fs::Metadata>> =
        destination_state.existing_metadata().cloned().map(Box::new);

    // upstream: generator.c:1469-1483 - when a directory is freshly created in
    // the destination but present in a `--copy-dest` basis, the generator
    // itemizes it as a local change (`cd` + blank) against the basis instead of
    // a new entry (`cd+++++++++`). The transfer root is materialised by the
    // sources orchestrator before this frame runs, so `destination_missing` is
    // false there even though the row is a creation; gate on whether a creation
    // record will be emitted (`destination_missing` for children, the root's
    // just-created marker for the root).
    let root_just_created = relative.is_none() && context.summary().destination_root_created();
    // Boxed for the same reason as `existing_destination_metadata` above: keep
    // the owned `fs::Metadata` off the per-directory recursion frame.
    let copy_dest_basis: Option<Box<fs::Metadata>> = if destination_missing || root_just_created {
        // The transfer root has `relative == None`; use an empty path so the
        // basis lookup resolves the copy-dest directory itself.
        let lookup_relative = relative.unwrap_or_else(|| Path::new(""));
        super::super::find_copy_dest_basis(context, destination, lookup_relative)?.map(Box::new)
    } else {
        None
    };

    if destination_missing && context.existing_only_enabled() {
        record_skipped_missing_destination(context, metadata, relative);
        return Ok(false);
    }

    let list_start = Instant::now();
    let mut entries = read_directory_entries_sorted_reuse(source, context.readdir_buf())?;
    // upstream: the file list is built once before the receiver mkdir's the
    // destination root, so a destination created inside the source tree (e.g.
    // `rsync -a src src/child`) never appears in the list. oc reads each source
    // directory live, after the destination root is created, so drop the entry
    // that IS the destination root to avoid descending into our own output.
    let destination_root = context.destination_root().to_path_buf();
    entries.retain(|entry| entry.path != destination_root);
    context.record_file_list_generation(list_start.elapsed());
    context.reserve_event_capacity(entries.len());
    context.register_progress();

    let dir_merge_guard = context.enter_directory(source, relative)?;
    if dir_merge_guard.is_excluded() {
        return Ok(false);
    }
    let _dir_merge_guard = dir_merge_guard;

    let directory_ready = Cell::new(!destination_missing);
    let mut created_directory_on_disk = false;
    let creation_record_pending = destination_missing && relative.is_some();
    let mut record_emitted = false;
    // upstream: main.c:794-796 + generator.c:566-572 - when the receiver
    // mkdirs the destination root, `flist->files[0]->flags |= FLAG_DIR_CREATED`
    // and `itemize()` emits `cd+++++++++ ./` for the synthesized "." entry.
    // Synthesize a "." relative path for the root when the destination root
    // was just created so the itemize stream emits `cd+++++++++ ./` ahead of
    // its children, matching upstream's `testsuite/itemize.test` golden.
    // Subsequent runs against an existing destination still see relative=None
    // here, so no record is synthesized and the `-i` output omits the `./`
    // entry as upstream does (test line 74-79).
    // upstream: main.c:794-796 + generator.c:566-572 - when the receiver
    // mkdirs the destination root, `flist->files[0]->flags |= FLAG_DIR_CREATED`
    // and `itemize()` emits `cd+++++++++ ./` for the synthesized "." entry.
    // The root flist entry has relative=None here because `non_empty_path("")`
    // returns None upstream of this call. `root_was_just_created` is true when
    // the sources orchestrator already mkdir'd the destination root in this run
    // (signalled via `summary.destination_root_created()`), driving the
    // `cd+++++++++ ./` creation row below.
    let root_was_just_created = root_just_created;
    let metadata_record = if let Some(rel) = relative {
        Some((
            rel.to_path_buf(),
            LocalCopyMetadata::from_metadata(metadata, None),
        ))
    } else {
        // Root frame (relative=None): always carry a "." record so the
        // existing-directory `MetadataReused` emission below fires for the
        // transfer root exactly as it does for child directories. Upstream
        // generator.c:1480-1483 itemizes the "." entry whether or not it
        // changed; the emit gate at generator.c:582-583 prints it when a
        // significant flag is set OR under `INFO_GTE(NAME, 2)`. The CLI
        // renderer suppresses the unchanged "." record unless `-vv`
        // (`emit_unchanged`) is set, so `-i` output still omits the `./` row
        // when the destination already exists (test line 74-79) while `-vv`
        // surfaces it as `.d ./`.
        if destination_missing {
            context.summary_mut().mark_destination_root_created();
        }
        Some((
            std::path::PathBuf::from("."),
            LocalCopyMetadata::from_metadata(metadata, None),
        ))
    };

    // upstream: main.c:794-796 + generator.c:566-572 - the root directory
    // entry (".") is itemized as `cd+++++++++ ./` when the pre-flight mkdir
    // materialised the destination root. When `ensure_destination_directory`
    // already created the root above this call frame, the per-frame
    // `ensure_directory` closure exits early (directory_ready=true) and the
    // closure's `record(...)` site never fires. Emit the synthetic "."
    // record up-front so it precedes child entries in the event stream.
    if root_was_just_created && let Some((ref rel_path, ref snapshot)) = metadata_record {
        let record = LocalCopyRecord::new(
            rel_path.clone(),
            LocalCopyAction::DirectoryCreated,
            0,
            Some(snapshot.len()),
            Duration::default(),
            Some(snapshot.clone()),
        );
        // upstream: generator.c:1480-1482 - a copy-dest match itemizes the
        // directory against the basis (ITEM_LOCAL_CHANGE, no ITEM_IS_NEW).
        let record = if let Some(basis_meta) = copy_dest_basis.as_deref() {
            record.with_change_set(LocalCopyChangeSet::for_existing_directory(
                metadata,
                basis_meta,
                &context.metadata_options(),
                context.omit_dir_times_enabled(),
                false,
                false,
                context.options().modify_window(),
            ))
        } else {
            record.with_creation(true)
        };
        context.record(record);
        record_emitted = true;
    }

    // upstream: generator.c:1480-1483 - when the destination directory already
    // exists (`statret == 0`), the generator still calls `itemize()` with
    // `iflags=0`; `itemize()` then ORs in `ITEM_REPORT_TIME|PERMS|...` based
    // on the existing-vs-source metadata drift. The emit gate at
    // `generator.c:582-583` then prints the row when any significant flag is
    // set OR when `INFO_GTE(NAME, 2)` is in effect, so unchanged dirs still
    // appear as all-dot `.d` rows under `-vv`.
    //
    // Mirror that here: always emit a `MetadataReused` record for an existing
    // destination directory so the event stream carries the entry. The CLI
    // renderer suppresses records whose `change_set` reports no change unless
    // the context flags `emit_unchanged` (mirroring `INFO_GTE(NAME, 2)`), so
    // non-verbose runs continue to omit unchanged dirs while `-vv` surfaces
    // them as `.d` rows.
    // upstream: generator.c:1480-1535 - for an existing directory the itemize
    // row precedes the child transfer rows, but a `--delete-before`/`during`
    // sweep of that directory's extraneous entries is emitted BEFORE the
    // directory's own row (do_delete_pass runs first for `before`;
    // delete_in_dir() prints `*deleting` ahead of the `.d` row for `during`).
    // A deferred sweep (`--delete-after`/`--delete-delay`, or the multi-source
    // `during`->`after` downgrade) runs at cleanup, so its row order is
    // unaffected. Stash the existing-directory record here and emit it after
    // the pre/during delete passes when the sweep is immediate; emit inline
    // otherwise so ordering relative to children is unchanged.
    let mut pending_existing_dir_record: Option<LocalCopyRecord> = None;
    let defer_dir_record_for_delete = context.options().delete_extraneous()
        && match context.delete_timing() {
            Some(DeleteTiming::Before) => true,
            Some(DeleteTiming::During) => !context.multi_source(),
            _ => false,
        };
    if !record_emitted
        && let Some((ref rel_path, ref snapshot)) = metadata_record
        && let Some(existing_meta) = existing_destination_metadata.as_deref()
    {
        // upstream: generator.c:557-571 - ACL/xattr drift is computed by
        // `set_acl(NULL, ...)` and `xattr_diff(...)`, both of which compare
        // the actual on-disk attribute payloads. The local-copy fast path
        // does not yet thread that comparison through, so leave these flags
        // clear here; the existing-directory row stays accurate for the
        // common `-iplrt` case the upstream `itemize.test` golden exercises.
        let change_set = LocalCopyChangeSet::for_existing_directory(
            metadata,
            existing_meta,
            &context.metadata_options(),
            context.omit_dir_times_enabled(),
            false,
            false,
            context.options().modify_window(),
        );
        let record = LocalCopyRecord::new(
            rel_path.clone(),
            LocalCopyAction::MetadataReused,
            0,
            Some(snapshot.len()),
            Duration::default(),
            Some(snapshot.clone()),
        )
        .with_change_set(change_set);
        // Suppress the `ensure_directory` closure's `DirectoryCreated` emission
        // either way; defer the row past an immediate delete sweep so the
        // `*deleting` rows print first (upstream generator.c ordering).
        record_emitted = true;
        if defer_dir_record_for_delete {
            pending_existing_dir_record = Some(record);
        } else {
            context.record(record);
        }
    }

    let mut kept_any = !prune_enabled;

    let mut ensure_directory = |context: &mut CopyContext| -> Result<(), LocalCopyError> {
        if directory_ready.get() {
            return Ok(());
        }

        if context.mode().is_dry_run() {
            if !context.implied_dirs_enabled()
                && let Some(parent) = destination.parent()
            {
                context.prepare_parent_directory(parent)?;
            }
            directory_ready.set(true);
        } else {
            if let Some(parent) = destination.parent() {
                context.prepare_parent_directory(parent)?;
            }
            if context.implied_dirs_enabled() {
                fs::create_dir_all(destination)
                    .map_err(|error| LocalCopyError::io("create directory", destination, error))?;
            } else {
                fs::create_dir(destination)
                    .map_err(|error| LocalCopyError::io("create directory", destination, error))?;
            }
            context.register_progress();
            context.register_created_path(destination, CreatedEntryKind::Directory, false);
            directory_ready.set(true);
            created_directory_on_disk = true;
        }

        // upstream: generator.c:recv_generator() - directory records appear
        // BEFORE their children in the itemize output. Emit the record
        // immediately so it precedes child entries in the record stream.
        if !record_emitted && let Some((ref rel_path, ref snapshot)) = metadata_record {
            let record = LocalCopyRecord::new(
                rel_path.clone(),
                LocalCopyAction::DirectoryCreated,
                0,
                Some(snapshot.len()),
                Duration::default(),
                Some(snapshot.clone()),
            );
            // upstream: generator.c:1480-1482 - a copy-dest match itemizes the
            // directory against the basis (ITEM_LOCAL_CHANGE, no ITEM_IS_NEW).
            let record = if let Some(basis_meta) = copy_dest_basis.as_deref() {
                record.with_change_set(LocalCopyChangeSet::for_existing_directory(
                    metadata,
                    basis_meta,
                    &context.metadata_options(),
                    context.omit_dir_times_enabled(),
                    false,
                    false,
                    context.options().modify_window(),
                ))
            } else {
                record.with_creation(true)
            };
            context.record(record);
            record_emitted = true;
        }

        Ok(())
    };

    // upstream: flist.c:2442 - global recursion off AND not a trailing-slash
    // / DOTDIR source: emit the directory entry only and stop. Trailing-slash
    // sources (`force_walk_one_level`) fall through to walk one level so
    // upstream's `(xfer_dirs && name_type != NORMAL_NAME)` semantics hold.
    if !context.recursive_enabled() && !force_walk_one_level {
        ensure_directory(context)?;
        if let Some(record) = pending_existing_dir_record.take() {
            context.record(record);
        }
        record_directory_completion(context, creation_record_pending, None);
        if !context.mode().is_dry_run() {
            apply_final_directory_metadata(
                context,
                source,
                destination,
                metadata,
                relative,
                #[cfg(any(
                    all(unix, any(feature = "acl", feature = "xattr")),
                    all(windows, feature = "acl")
                ))]
                mode,
                #[cfg(all(unix, feature = "xattr"))]
                preserve_xattrs,
                #[cfg(all(any(unix, windows), feature = "acl"))]
                preserve_acls,
            )?;
        }
        return Ok(true);
    }

    if !directory_ready.get() && !prune_enabled {
        ensure_directory(context)?;
    }

    let mut plan = plan_directory_entries(context, &entries, relative, root_device)?;
    // upstream: hlink.c:match_gnums / generator.c:1803-1806 - the last name-sorted
    // member of a hard-link cohort is the transferred data-holder; earlier members
    // follow it as `hf ... => <holder>` aliases. Reorder the plan so the executor's
    // per-inode tracker records the holder first and every alias points at it.
    reorder_hardlink_group_holders(
        context.options().hard_links_enabled(),
        !context.reference_directories().is_empty(),
        context.options().fake_super_enabled(),
        destination,
        &mut plan.planned_entries,
    );
    apply_pre_transfer_deletions(context, destination, relative, &plan)?;
    // upstream: generator.c:1532-1537 - a non-INC_RECURSE `--delete-during`
    // sweep runs while the generator itemizes the directory entry, before it
    // recurses into that directory's children. Emit those `*deleting` rows
    // ahead of the child transfer rows to match upstream's per-directory
    // ordering; deferred timings are handled after the child loop.
    // upstream: main.c:1356 - a `--max-delete` limit does NOT abort the
    // transfer; the generator stops deleting, finishes every transfer, and
    // reports the limit at cleanup (exit 25). Because the during-sweep now runs
    // before this directory's child loop, capture a limit error and re-raise it
    // only after the children (and metadata) are processed, so pending copies
    // are not skipped. Any other error still propagates immediately.
    let mut deferred_delete_limit_error: Option<LocalCopyError> = None;
    match apply_during_transfer_deletions(
        context,
        destination,
        relative,
        plan.deletion_enabled,
        plan.delete_timing,
        &plan.keep_names,
    ) {
        Ok(()) => {}
        Err(error) if error.is_delete_limit_error() => {
            deferred_delete_limit_error = Some(error);
        }
        Err(error) => return Err(error),
    }

    // upstream: generator.c ordering - the existing-directory `.d` row follows
    // any immediate `--delete-before`/`during` sweep of this directory but
    // precedes its child transfer rows. Emit the deferred row now.
    if let Some(record) = pending_existing_dir_record.take() {
        context.record(record);
    }

    {
        let cache = prefetch_directory_checksums(context, &plan, destination);
        if !cache.is_empty() {
            context.set_checksum_cache(cache);
        }
    }

    // Reusable buffer for target paths. Seeded once with the destination
    // directory; each entry pushes its name and pops it after use, avoiding
    // a per-entry PathBuf allocation from Path::join.
    let mut target_buf = destination.to_path_buf();

    let mut first_entry_io_error: Option<LocalCopyError> = None;
    for planned in &plan.planned_entries {
        let result = process_planned_entry(
            context,
            planned,
            &mut target_buf,
            &mut ensure_directory,
            root_device,
        );
        match result {
            Ok(entry_kept) => {
                if entry_kept {
                    kept_any = true;
                }
            }
            Err(error) if error.is_vanished_error() => {
                // upstream: flist.c:1289 / sender.c:358 - vanished files produce
                // a warning and set IOERR_VANISHED (exit code 24).
                // full_fname() wraps the path in double quotes (util1.c:1228).
                eprintln!("file has vanished: \"{}\"", planned.entry.path.display());
                context.record_io_error();
                if first_entry_io_error.is_none() {
                    first_entry_io_error = Some(error);
                }
            }
            Err(error) if error.is_io_error() => {
                // upstream: rsync continues transferring remaining entries when
                // individual files fail with I/O errors (permission denied, etc.),
                // regardless of whether --delete is active.
                context.record_io_error();
                if first_entry_io_error.is_none() {
                    first_entry_io_error = Some(error);
                }
            }
            Err(error) if error.is_delete_limit_error() => {
                // upstream: main.c:1356 - a --max-delete limit hit while
                // recursing into a child directory must not abort the parent's
                // remaining transfers. Defer it, letting the sibling entries
                // finish, then re-raise at the end of this directory.
                if deferred_delete_limit_error.is_none() {
                    deferred_delete_limit_error = Some(error);
                }
            }
            Err(error) => return Err(error),
        }
    }

    context.clear_checksum_cache();

    handle_post_transfer_deletions(
        context,
        destination,
        relative,
        plan.deletion_enabled,
        plan.delete_timing,
        &plan.keep_names,
    )?;

    if prune_enabled && !kept_any {
        handle_empty_directory_pruning(context, destination, created_directory_on_disk)?;
        // upstream: the --max-delete limit (exit 25) outranks a partial/IO error
        // (exit 23/24); raise it first, mirroring the old post-loop ordering.
        if let Some(error) = deferred_delete_limit_error {
            return Err(error);
        }
        if let Some(error) = first_entry_io_error {
            return Err(error);
        }
        return Ok(false);
    }

    record_directory_completion(context, creation_record_pending, None);

    if !context.mode().is_dry_run() {
        apply_final_directory_metadata(
            context,
            source,
            destination,
            metadata,
            relative,
            #[cfg(any(
                all(unix, any(feature = "acl", feature = "xattr")),
                all(windows, feature = "acl")
            ))]
            mode,
            #[cfg(all(unix, feature = "xattr"))]
            preserve_xattrs,
            #[cfg(all(any(unix, windows), feature = "acl"))]
            preserve_acls,
        )?;
    }

    // upstream: the --max-delete limit (exit 25) is reported after the transfer
    // and metadata finalization complete, and outranks a partial/IO error
    // (exit 23/24). Raise the deferred limit error first to preserve the old
    // post-loop ordering now that the delete-during sweep runs before the loop.
    if let Some(error) = deferred_delete_limit_error {
        return Err(error);
    }

    // If there were I/O errors during entry processing, propagate the first
    // one now that deletions and metadata finalization have completed.
    if let Some(error) = first_entry_io_error {
        return Err(error);
    }

    Ok(true)
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::path::PathBuf;

    use super::super::planner::{DirectoryPlan, EntryAction, PlannedEntry};
    use super::super::support::DirectoryEntry;
    use super::checksum::collect_file_pairs_for_checksum;
    use test_support::create_tempdir;

    fn create_test_entry(path: PathBuf, file_name: &str, size: u64) -> DirectoryEntry {
        std::fs::write(&path, vec![0u8; size as usize]).expect("create test file");
        let metadata = std::fs::metadata(&path).expect("get metadata");
        DirectoryEntry {
            path,
            file_name: OsString::from(file_name),
            metadata,
        }
    }

    #[test]
    fn collect_file_pairs_filters_to_copyfile_actions() {
        let dir = create_tempdir();
        let source_dir = dir.path().join("src");
        let dest_dir = dir.path().join("dst");
        std::fs::create_dir_all(&source_dir).unwrap();
        std::fs::create_dir_all(&dest_dir).unwrap();

        let entry1 = create_test_entry(source_dir.join("file1.txt"), "file1.txt", 100);
        let entry2 = create_test_entry(source_dir.join("file2.txt"), "file2.txt", 200);
        let entry3 = create_test_entry(source_dir.join("dir"), "dir", 0);

        std::fs::write(dest_dir.join("file1.txt"), vec![0u8; 100]).unwrap();
        std::fs::write(dest_dir.join("file2.txt"), vec![0u8; 200]).unwrap();
        std::fs::create_dir(dest_dir.join("dir")).unwrap();

        let entries = vec![entry1, entry2, entry3];
        let planned: Vec<PlannedEntry> = vec![
            PlannedEntry {
                entry: &entries[0],
                relative: PathBuf::from("file1.txt"),
                action: EntryAction::CopyFile,
                metadata_override: None,
            },
            PlannedEntry {
                entry: &entries[1],
                relative: PathBuf::from("file2.txt"),
                action: EntryAction::CopyFile,
                metadata_override: None,
            },
            PlannedEntry {
                entry: &entries[2],
                relative: PathBuf::from("dir"),
                action: EntryAction::CopyDirectory,
                metadata_override: None,
            },
        ];

        let plan = DirectoryPlan {
            planned_entries: planned,
            keep_names: Vec::new(),
            deletion_enabled: false,
            delete_timing: None,
        };

        let (pairs, _dest_meta) = collect_file_pairs_for_checksum(&plan, &dest_dir, None);

        assert_eq!(pairs.len(), 2);
        assert!(pairs.iter().any(|p| p.source.ends_with("file1.txt")));
        assert!(pairs.iter().any(|p| p.source.ends_with("file2.txt")));
    }

    #[test]
    fn collect_file_pairs_skips_missing_destination() {
        let dir = create_tempdir();
        let source_dir = dir.path().join("src");
        let dest_dir = dir.path().join("dst");
        std::fs::create_dir_all(&source_dir).unwrap();
        std::fs::create_dir_all(&dest_dir).unwrap();

        let entry = create_test_entry(source_dir.join("file.txt"), "file.txt", 100);

        let entries = [entry];
        let planned: Vec<PlannedEntry> = vec![PlannedEntry {
            entry: &entries[0],
            relative: PathBuf::from("file.txt"),
            action: EntryAction::CopyFile,
            metadata_override: None,
        }];

        let plan = DirectoryPlan {
            planned_entries: planned,
            keep_names: Vec::new(),
            deletion_enabled: false,
            delete_timing: None,
        };

        let (pairs, _dest_meta) = collect_file_pairs_for_checksum(&plan, &dest_dir, None);

        assert!(pairs.is_empty());
    }

    #[test]
    fn collect_file_pairs_skips_size_mismatch() {
        let dir = create_tempdir();
        let source_dir = dir.path().join("src");
        let dest_dir = dir.path().join("dst");
        std::fs::create_dir_all(&source_dir).unwrap();
        std::fs::create_dir_all(&dest_dir).unwrap();

        let entry = create_test_entry(source_dir.join("file.txt"), "file.txt", 100);

        std::fs::write(dest_dir.join("file.txt"), vec![0u8; 50]).unwrap();

        let entries = [entry];
        let planned: Vec<PlannedEntry> = vec![PlannedEntry {
            entry: &entries[0],
            relative: PathBuf::from("file.txt"),
            action: EntryAction::CopyFile,
            metadata_override: None,
        }];

        let plan = DirectoryPlan {
            planned_entries: planned,
            keep_names: Vec::new(),
            deletion_enabled: false,
            delete_timing: None,
        };

        let (pairs, _dest_meta) = collect_file_pairs_for_checksum(&plan, &dest_dir, None);

        assert!(pairs.is_empty());
    }

    #[test]
    fn collect_file_pairs_includes_matching_sizes() {
        let dir = create_tempdir();
        let source_dir = dir.path().join("src");
        let dest_dir = dir.path().join("dst");
        std::fs::create_dir_all(&source_dir).unwrap();
        std::fs::create_dir_all(&dest_dir).unwrap();

        let entry = create_test_entry(source_dir.join("file.txt"), "file.txt", 100);

        std::fs::write(dest_dir.join("file.txt"), vec![0u8; 100]).unwrap();

        let entries = [entry];
        let planned: Vec<PlannedEntry> = vec![PlannedEntry {
            entry: &entries[0],
            relative: PathBuf::from("file.txt"),
            action: EntryAction::CopyFile,
            metadata_override: None,
        }];

        let plan = DirectoryPlan {
            planned_entries: planned,
            keep_names: Vec::new(),
            deletion_enabled: false,
            delete_timing: None,
        };

        let (pairs, dest_meta) = collect_file_pairs_for_checksum(&plan, &dest_dir, None);

        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].source_size, 100);
        assert_eq!(pairs[0].destination_size, 100);
        // On unix the single-link regular-file destination lstat is cached for
        // copy_file to reuse, avoiding a second lstat of the same path. There is
        // no portable nlink on non-unix, so the reuse cache is never populated
        // and copy_file fresh-stats instead (matching upstream's per-file lstat
        // and pre-dedup master behaviour).
        #[cfg(unix)]
        {
            let cached = dest_meta
                .get(&dest_dir.join("file.txt"))
                .expect("destination metadata cached");
            assert_eq!(cached.len(), 100);
        }
        #[cfg(not(unix))]
        {
            assert!(dest_meta.is_empty());
        }
    }

    // upstream: generator.c:recv_generator() lstats the destination; a symlink
    // destination is never treated as a regular-file checksum candidate. The
    // nofollow lstat must therefore exclude a symlink dest from both the pair
    // list and the reusable metadata cache, even when it points at a same-size
    // regular file (which a FOLLOW stat would have wrongly accepted).
    #[cfg(unix)]
    #[test]
    fn collect_file_pairs_excludes_symlink_destination() {
        let dir = create_tempdir();
        let source_dir = dir.path().join("src");
        let dest_dir = dir.path().join("dst");
        std::fs::create_dir_all(&source_dir).unwrap();
        std::fs::create_dir_all(&dest_dir).unwrap();

        let entry = create_test_entry(source_dir.join("file.txt"), "file.txt", 100);

        // A same-size regular file the destination symlink points at.
        let referent = dest_dir.join("referent.bin");
        std::fs::write(&referent, vec![0u8; 100]).unwrap();
        std::os::unix::fs::symlink(&referent, dest_dir.join("file.txt")).unwrap();

        let entries = [entry];
        let planned: Vec<PlannedEntry> = vec![PlannedEntry {
            entry: &entries[0],
            relative: PathBuf::from("file.txt"),
            action: EntryAction::CopyFile,
            metadata_override: None,
        }];

        let plan = DirectoryPlan {
            planned_entries: planned,
            keep_names: Vec::new(),
            deletion_enabled: false,
            delete_timing: None,
        };

        let (pairs, dest_meta) = collect_file_pairs_for_checksum(&plan, &dest_dir, None);

        assert!(pairs.is_empty(), "symlink dest is not a checksum candidate");
        assert!(
            !dest_meta.contains_key(&dest_dir.join("file.txt")),
            "symlink dest metadata must not be cached as a regular file"
        );
    }

    // A destination hardlinked to a sibling can have its shared inode's mtime
    // mutated by copy_file when that sibling is updated earlier in the same
    // directory pass, so its pre-pass lstat must NOT be cached for reuse (that
    // would replay a stale mtime and add a phantom itemized time change). The
    // destination still stays a checksum candidate; only the reuse cache skips
    // it, so copy_file performs its own fresh lstat.
    #[cfg(unix)]
    #[test]
    fn collect_file_pairs_excludes_hardlinked_destination_from_cache() {
        let dir = create_tempdir();
        let source_dir = dir.path().join("src");
        let dest_dir = dir.path().join("dst");
        std::fs::create_dir_all(&source_dir).unwrap();
        std::fs::create_dir_all(&dest_dir).unwrap();

        let entry = create_test_entry(source_dir.join("file.txt"), "file.txt", 100);

        // A same-size regular destination with two links (nlink == 2).
        std::fs::write(dest_dir.join("file.txt"), vec![0u8; 100]).unwrap();
        std::fs::hard_link(dest_dir.join("file.txt"), dest_dir.join("alias.txt")).unwrap();

        let entries = [entry];
        let planned: Vec<PlannedEntry> = vec![PlannedEntry {
            entry: &entries[0],
            relative: PathBuf::from("file.txt"),
            action: EntryAction::CopyFile,
            metadata_override: None,
        }];

        let plan = DirectoryPlan {
            planned_entries: planned,
            keep_names: Vec::new(),
            deletion_enabled: false,
            delete_timing: None,
        };

        let (pairs, dest_meta) = collect_file_pairs_for_checksum(&plan, &dest_dir, None);

        assert_eq!(
            pairs.len(),
            1,
            "hardlinked dest is still a checksum candidate"
        );
        assert!(
            !dest_meta.contains_key(&dest_dir.join("file.txt")),
            "hardlinked dest metadata must not be cached (mtime can change mid-pass)"
        );
    }
}
