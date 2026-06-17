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
use std::path::Path;
use std::time::{Duration, Instant};

use crate::local_copy::overrides::device_identifier;
use crate::local_copy::{
    CopyContext, CreatedEntryKind, LocalCopyAction, LocalCopyChangeSet, LocalCopyError,
    LocalCopyMetadata, LocalCopyRecord,
};

pub(crate) use batch::capture_batch_file_entry;
pub(crate) use checksum::prefetch_directory_checksums;
use deletion::{handle_empty_directory_pruning, handle_post_transfer_deletions};
use destination::{check_destination_state, record_skipped_missing_destination};
use dir_metadata::{apply_final_directory_metadata, record_directory_completion};
use entry::process_planned_entry;

use super::planner::{apply_pre_transfer_deletions, plan_directory_entries};
use super::support::read_directory_entries_sorted_reuse;

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
    let existing_destination_metadata: Option<fs::Metadata> =
        destination_state.existing_metadata().cloned();

    if destination_missing && context.existing_only_enabled() {
        record_skipped_missing_destination(context, metadata, relative);
        return Ok(false);
    }

    let list_start = Instant::now();
    let entries = read_directory_entries_sorted_reuse(source, context.readdir_buf())?;
    context.record_file_list_generation(list_start.elapsed());
    context.reserve_event_capacity(entries.len());
    context.register_progress();

    let dir_merge_guard = context.enter_directory(source)?;
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
    // returns None upstream of this call. Synthesize a "." record when the
    // sources orchestrator already mkdir'd the destination root in this run
    // (signalled via `summary.destination_root_created()`), so the itemize
    // stream emits the root row ahead of its children. Subsequent runs see
    // the flag stay clear and skip the row, matching upstream's `-i` output
    // when the destination already exists (test line 74-79).
    let root_was_just_created = relative.is_none() && context.summary().destination_root_created();
    let metadata_record = if let Some(rel) = relative {
        Some((
            rel.to_path_buf(),
            LocalCopyMetadata::from_metadata(metadata, None),
        ))
    } else if destination_missing || root_was_just_created {
        if destination_missing {
            context.summary_mut().mark_destination_root_created();
        }
        Some((
            std::path::PathBuf::from("."),
            LocalCopyMetadata::from_metadata(metadata, None),
        ))
    } else {
        None
    };

    // upstream: main.c:794-796 + generator.c:566-572 - the root directory
    // entry (".") is itemized as `cd+++++++++ ./` when the pre-flight mkdir
    // materialised the destination root. When `ensure_destination_directory`
    // already created the root above this call frame, the per-frame
    // `ensure_directory` closure exits early (directory_ready=true) and the
    // closure's `record(...)` site never fires. Emit the synthetic "."
    // record up-front so it precedes child entries in the event stream.
    if root_was_just_created && let Some((ref rel_path, ref snapshot)) = metadata_record {
        context.record(
            LocalCopyRecord::new(
                rel_path.clone(),
                LocalCopyAction::DirectoryCreated,
                0,
                Some(snapshot.len()),
                Duration::default(),
                Some(snapshot.clone()),
            )
            .with_creation(true),
        );
        record_emitted = true;
    }

    // upstream: generator.c:1480-1483 - when the destination directory already
    // exists (`statret == 0`), the generator still calls `itemize()` with
    // `iflags=0`; `itemize()` then ORs in `ITEM_REPORT_TIME|PERMS|...` based
    // on the existing-vs-source metadata drift. The line is emitted only when
    // the resulting `iflags` carries `SIGNIFICANT_ITEM_FLAGS`
    // (`generator.c:582-583`); otherwise it is suppressed.
    //
    // Mirror that here: when the directory already exists and any attribute
    // differs from the source, emit a `MetadataReused` record carrying the
    // computed change-set so the renderer produces lines like
    // `.d..t...... foo/`. When nothing differs, the record stays unemitted
    // and the renderer skips the directory row entirely.
    if !record_emitted
        && let Some((ref rel_path, ref snapshot)) = metadata_record
        && let Some(existing_meta) = existing_destination_metadata.as_ref()
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
        );
        if change_set.has_any_change() {
            context.record(
                LocalCopyRecord::new(
                    rel_path.clone(),
                    LocalCopyAction::MetadataReused,
                    0,
                    Some(snapshot.len()),
                    Duration::default(),
                    Some(snapshot.clone()),
                )
                .with_change_set(change_set),
            );
            record_emitted = true;
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
            context.record(
                LocalCopyRecord::new(
                    rel_path.clone(),
                    LocalCopyAction::DirectoryCreated,
                    0,
                    Some(snapshot.len()),
                    Duration::default(),
                    Some(snapshot.clone()),
                )
                .with_creation(true),
            );
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

    let plan = plan_directory_entries(context, &entries, relative, root_device)?;
    apply_pre_transfer_deletions(context, destination, relative, &plan)?;

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

        let pairs = collect_file_pairs_for_checksum(&plan, &dest_dir, None);

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

        let pairs = collect_file_pairs_for_checksum(&plan, &dest_dir, None);

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

        let pairs = collect_file_pairs_for_checksum(&plan, &dest_dir, None);

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

        let pairs = collect_file_pairs_for_checksum(&plan, &dest_dir, None);

        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].source_size, 100);
        assert_eq!(pairs[0].destination_size, 100);
    }
}
