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
    CopyContext, CreatedEntryKind, LocalCopyAction, LocalCopyError, LocalCopyMetadata,
    LocalCopyRecord,
};

pub(crate) use batch::capture_batch_file_entry;
pub(crate) use checksum::prefetch_directory_checksums;
use deletion::{handle_empty_directory_pruning, handle_post_transfer_deletions};
use destination::{DestinationState, check_destination_state, record_skipped_missing_destination};
use dir_metadata::{apply_final_directory_metadata, record_directory_completion};
use entry::process_planned_entry;

use super::planner::{apply_pre_transfer_deletions, plan_directory_entries};
use super::support::read_directory_entries_sorted;

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

    // Check destination state and determine if we need to create it
    let destination_state = check_destination_state(context, destination, relative)?;
    let destination_missing = destination_state == DestinationState::Missing;

    // Handle existing_only mode early exit
    if destination_missing && context.existing_only_enabled() {
        record_skipped_missing_destination(context, metadata, relative);
        return Ok(false);
    }

    // Read and sort source directory entries
    let list_start = Instant::now();
    let entries = read_directory_entries_sorted(source)?;
    context.record_file_list_generation(list_start.elapsed());
    context.register_progress();

    // Enter directory for filter processing
    let dir_merge_guard = context.enter_directory(source)?;
    if dir_merge_guard.is_excluded() {
        return Ok(false);
    }
    let _dir_merge_guard = dir_merge_guard;

    // Setup directory creation state
    let directory_ready = Cell::new(!destination_missing);
    let mut created_directory_on_disk = false;
    let creation_record_pending = destination_missing && relative.is_some();
    let mut pending_record: Option<LocalCopyRecord> = None;
    let metadata_record = relative.map(|rel| {
        (
            rel.to_path_buf(),
            LocalCopyMetadata::from_metadata(metadata, None),
        )
    });

    let mut kept_any = !prune_enabled;

    // Closure to ensure the destination directory exists when needed
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

        if pending_record.is_none()
            && let Some((ref rel_path, ref snapshot)) = metadata_record
        {
            pending_record = Some(
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
        }

        Ok(())
    };

    // Handle non-recursive mode: just create the directory without descending
    if !context.recursive_enabled() {
        ensure_directory(context)?;
        record_directory_completion(context, creation_record_pending, pending_record.take());
        if !context.mode().is_dry_run() {
            apply_final_directory_metadata(
                context,
                source,
                destination,
                metadata,
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

    // Ensure directory exists if not pruning
    if !directory_ready.get() && !prune_enabled {
        ensure_directory(context)?;
    }

    // Plan directory entries and apply pre-transfer deletions
    let plan = plan_directory_entries(context, &entries, relative, root_device)?;
    apply_pre_transfer_deletions(context, destination, relative, &plan)?;

    // Prefetch checksums in parallel when checksum mode is enabled
    {
        let cache = prefetch_directory_checksums(context, &plan, destination);
        if !cache.is_empty() {
            context.set_checksum_cache(cache);
        }
    }

    // Process each planned entry
    let mut first_entry_io_error: Option<LocalCopyError> = None;
    for planned in &plan.planned_entries {
        let result = process_planned_entry(
            context,
            planned,
            destination,
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
                eprintln!("file has vanished: {}", planned.entry.path.display());
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

    // Clear checksum cache to free memory
    context.clear_checksum_cache();

    // Handle post-transfer deletions
    handle_post_transfer_deletions(
        context,
        destination,
        relative,
        plan.deletion_enabled,
        plan.delete_timing,
        &plan.keep_names,
    )?;

    // Handle empty directory pruning
    if prune_enabled && !kept_any {
        handle_empty_directory_pruning(context, destination, created_directory_on_disk)?;
        if let Some(error) = first_entry_io_error {
            return Err(error);
        }
        return Ok(false);
    }

    // Record completion and apply final metadata
    record_directory_completion(context, creation_record_pending, pending_record);

    if !context.mode().is_dry_run() {
        apply_final_directory_metadata(
            context,
            source,
            destination,
            metadata,
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
        // Create the actual file so we can get metadata
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

        // Create source files
        let entry1 = create_test_entry(source_dir.join("file1.txt"), "file1.txt", 100);
        let entry2 = create_test_entry(source_dir.join("file2.txt"), "file2.txt", 200);
        let entry3 = create_test_entry(source_dir.join("dir"), "dir", 0);

        // Create destination files with same sizes
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

        let pairs = collect_file_pairs_for_checksum(&plan, &dest_dir);

        // Should only include CopyFile entries (2 files, not the directory)
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

        // Create source file
        let entry = create_test_entry(source_dir.join("file.txt"), "file.txt", 100);

        // Don't create destination file

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

        let pairs = collect_file_pairs_for_checksum(&plan, &dest_dir);

        // Should be empty because destination doesn't exist
        assert!(pairs.is_empty());
    }

    #[test]
    fn collect_file_pairs_skips_size_mismatch() {
        let dir = create_tempdir();
        let source_dir = dir.path().join("src");
        let dest_dir = dir.path().join("dst");
        std::fs::create_dir_all(&source_dir).unwrap();
        std::fs::create_dir_all(&dest_dir).unwrap();

        // Create source file (100 bytes)
        let entry = create_test_entry(source_dir.join("file.txt"), "file.txt", 100);

        // Create destination file with different size (50 bytes)
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

        let pairs = collect_file_pairs_for_checksum(&plan, &dest_dir);

        // Should be empty because sizes don't match
        assert!(pairs.is_empty());
    }

    #[test]
    fn collect_file_pairs_includes_matching_sizes() {
        let dir = create_tempdir();
        let source_dir = dir.path().join("src");
        let dest_dir = dir.path().join("dst");
        std::fs::create_dir_all(&source_dir).unwrap();
        std::fs::create_dir_all(&dest_dir).unwrap();

        // Create source file (100 bytes)
        let entry = create_test_entry(source_dir.join("file.txt"), "file.txt", 100);

        // Create destination file with same size (100 bytes)
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

        let pairs = collect_file_pairs_for_checksum(&plan, &dest_dir);

        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].source_size, 100);
        assert_eq!(pairs[0].destination_size, 100);
    }
}
