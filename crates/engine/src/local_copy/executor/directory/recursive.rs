use std::cell::Cell;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, UNIX_EPOCH};

#[cfg(all(unix, any(feature = "acl", feature = "xattr")))]
use crate::local_copy::LocalCopyExecution;
use crate::local_copy::overrides::device_identifier;
#[cfg(all(unix, feature = "acl"))]
use crate::local_copy::sync_acls_if_requested;
#[cfg(all(unix, feature = "xattr"))]
use crate::local_copy::sync_xattrs_if_requested;
use crate::local_copy::{
    CopyContext, CreatedEntryKind, DeleteTiming, LocalCopyAction, LocalCopyArgumentError,
    LocalCopyError, LocalCopyMetadata, LocalCopyRecord, copy_device, copy_fifo, copy_file,
    copy_symlink, delete_extraneous_entries, follow_symlink_metadata, map_metadata_error,
};
use ::metadata::apply_directory_metadata_with_options;

use super::super::non_empty_path;
use super::planner::DirectoryPlan;
use super::planner::{
    EntryAction, PlannedEntry, apply_pre_transfer_deletions, plan_directory_entries,
};
use super::support::read_directory_entries_sorted;

use super::parallel_checksum::{ChecksumCache, FilePair};

/// Result of checking destination directory state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DestinationState {
    /// Destination directory already exists and is ready.
    Ready,
    /// Destination is missing and needs to be created.
    Missing,
}

/// Checks the destination path and determines if it needs to be created.
///
/// Handles various cases:
/// - Destination is already a directory: returns `Ready`
/// - Destination is a symlink to a directory with `--keep-dirlinks`: returns `Ready`
/// - Destination exists but is not a directory: removes it if force is enabled
/// - Destination doesn't exist: returns `Missing`
#[inline]
fn check_destination_state(
    context: &mut CopyContext,
    destination: &Path,
    relative: Option<&Path>,
) -> Result<DestinationState, LocalCopyError> {
    match fs::symlink_metadata(destination) {
        Ok(existing) => {
            let file_type = existing.file_type();
            if file_type.is_dir() {
                // Directory already present; nothing to do.
                Ok(DestinationState::Ready)
            } else if file_type.is_symlink() && context.keep_dirlinks_enabled() {
                let target_metadata = follow_symlink_metadata(destination)?;
                if target_metadata.file_type().is_dir() {
                    Ok(DestinationState::Ready)
                } else if context.force_replacements_enabled() {
                    context.force_remove_destination(destination, relative, &existing)?;
                    Ok(DestinationState::Missing)
                } else {
                    Err(LocalCopyError::invalid_argument(
                        LocalCopyArgumentError::ReplaceNonDirectoryWithDirectory,
                    ))
                }
            } else if context.force_replacements_enabled() {
                context.force_remove_destination(destination, relative, &existing)?;
                Ok(DestinationState::Missing)
            } else {
                Err(LocalCopyError::invalid_argument(
                    LocalCopyArgumentError::ReplaceNonDirectoryWithDirectory,
                ))
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(DestinationState::Missing),
        Err(error) => Err(LocalCopyError::io(
            "inspect destination directory",
            destination.to_path_buf(),
            error,
        )),
    }
}

/// Records that a directory was skipped because existing_only mode is enabled
/// and the destination doesn't exist.
#[inline]
fn record_skipped_missing_destination(
    context: &mut CopyContext,
    metadata: &fs::Metadata,
    relative: Option<&Path>,
) {
    context.summary_mut().record_directory_total();
    if let Some(relative_path) = relative {
        let metadata_snapshot = LocalCopyMetadata::from_metadata(metadata, None);
        context.record(LocalCopyRecord::new(
            relative_path.to_path_buf(),
            LocalCopyAction::SkippedMissingDestination,
            0,
            Some(metadata_snapshot.len()),
            Duration::default(),
            Some(metadata_snapshot),
        ));
    }
}

/// Applies final metadata to a directory after all contents have been processed.
///
/// This includes permissions, timestamps (unless omit_dir_times is enabled),
/// extended attributes, and ACLs.
fn apply_final_directory_metadata(
    context: &CopyContext,
    source: &Path,
    destination: &Path,
    metadata: &fs::Metadata,
    #[cfg(all(unix, any(feature = "acl", feature = "xattr")))] mode: LocalCopyExecution,
    #[cfg(all(unix, feature = "xattr"))] preserve_xattrs: bool,
    #[cfg(all(unix, feature = "acl"))] preserve_acls: bool,
) -> Result<(), LocalCopyError> {
    let metadata_options = if context.omit_dir_times_enabled() {
        context.metadata_options().preserve_times(false)
    } else {
        context.metadata_options()
    };
    apply_directory_metadata_with_options(destination, metadata, metadata_options)
        .map_err(map_metadata_error)?;

    #[cfg(all(unix, feature = "xattr"))]
    sync_xattrs_if_requested(
        preserve_xattrs,
        mode,
        source,
        destination,
        true,
        context.filter_program(),
    )?;

    #[cfg(all(unix, feature = "acl"))]
    sync_acls_if_requested(preserve_acls, mode, source, destination, true)?;

    // Suppress unused variable warnings when features are disabled
    let _ = source;

    Ok(())
}

/// Handles the deletion phase after transfer, based on the configured timing.
#[inline]
fn handle_post_transfer_deletions(
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
fn handle_empty_directory_pruning(
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

/// Processes a single planned entry during directory recursion.
///
/// This handles all entry types: directories, files, symlinks, FIFOs, and devices.
/// Returns `true` if this entry should count as "kept" for pruning purposes.
fn process_planned_entry(
    context: &mut CopyContext,
    planned: &PlannedEntry<'_>,
    destination: &Path,
    ensure_directory: &mut impl FnMut(&mut CopyContext) -> Result<(), LocalCopyError>,
    root_device: Option<u64>,
) -> Result<bool, LocalCopyError> {
    let file_name = &planned.entry.file_name;
    let target_path = destination.join(Path::new(file_name));
    let entry_metadata = planned.metadata();
    let record_relative = non_empty_path(planned.relative.as_path());

    // Handle skip actions first (no directory creation or batch capture needed)
    match planned.action {
        EntryAction::SkipExcluded => return Ok(false),
        EntryAction::SkipNonRegular => {
            if entry_metadata.file_type().is_symlink() {
                context.summary_mut().record_symlink_total();
            }
            context.record_skipped_non_regular(record_relative);
            return Ok(false);
        }
        EntryAction::SkipMountPoint => {
            context.record_skipped_mount_point(record_relative);
            return Ok(false);
        }
        _ => {}
    }

    // All copy actions share: ensure parent directory exists + capture to batch
    ensure_directory(context)?;
    if let Some(rel_path) = record_relative {
        capture_batch_file_entry(context, rel_path, entry_metadata)?;
    }

    let source = planned.entry.path.as_path();
    let relative = Some(planned.relative.as_path());

    match planned.action {
        EntryAction::CopyDirectory => copy_directory_recursive(
            context,
            source,
            &target_path,
            entry_metadata,
            relative,
            root_device,
        ),
        EntryAction::CopyFile | EntryAction::CopyDeviceAsFile => {
            copy_file(context, source, &target_path, entry_metadata, relative)
        }
        EntryAction::CopySymlink => {
            let metadata_options = context.metadata_options();
            copy_symlink(
                context,
                source,
                &target_path,
                entry_metadata,
                &metadata_options,
                relative,
            )?;
            Ok(true)
        }
        EntryAction::CopyFifo => {
            let metadata_options = context.metadata_options();
            copy_fifo(
                context,
                source,
                &target_path,
                entry_metadata,
                &metadata_options,
                relative,
            )?;
            Ok(true)
        }
        EntryAction::CopyDevice => {
            let metadata_options = context.metadata_options();
            copy_device(
                context,
                source,
                &target_path,
                entry_metadata,
                &metadata_options,
                relative,
            )?;
            Ok(true)
        }
        // Skip variants already handled above
        EntryAction::SkipExcluded | EntryAction::SkipNonRegular | EntryAction::SkipMountPoint => {
            unreachable!()
        }
    }
}

/// Records directory completion statistics and pending records.
#[inline]
fn record_directory_completion(
    context: &mut CopyContext,
    creation_record_pending: bool,
    pending_record: Option<LocalCopyRecord>,
) {
    context.summary_mut().record_directory_total();
    if creation_record_pending {
        context.summary_mut().record_directory();
    }
    if let Some(record) = pending_record {
        context.record(record);
    }
}

/// Builds a protocol [`FileEntry`](protocol::flist::FileEntry) from filesystem metadata.
///
/// Converts the local `fs::Metadata` and relative path into the protocol crate's
/// `FileEntry` type, which can then be encoded using the protocol wire format
/// for upstream-compatible batch files.
fn build_protocol_file_entry(
    relative_path: &Path,
    metadata: &fs::Metadata,
) -> protocol::flist::FileEntry {
    #[cfg(unix)]
    use std::os::unix::fs::MetadataExt;

    #[cfg(unix)]
    let mode = metadata.mode();

    #[cfg(not(unix))]
    let mode = if metadata.is_dir() {
        0o040755
    } else if metadata.file_type().is_symlink() {
        0o120777
    } else {
        0o100644
    };

    let permissions = mode & 0o7777;
    let file_type = metadata.file_type();
    let name = PathBuf::from(relative_path);

    let mut entry = if file_type.is_dir() {
        protocol::flist::FileEntry::new_directory(name, permissions)
    } else if file_type.is_symlink() {
        // Read symlink target for the protocol entry
        protocol::flist::FileEntry::new_symlink(name, PathBuf::new())
    } else {
        protocol::flist::FileEntry::new_file(name, metadata.len(), permissions)
    };

    let mtime = metadata
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs() as i64);
    entry.set_mtime(mtime, 0);

    #[cfg(unix)]
    {
        entry.set_uid(metadata.uid());
        entry.set_gid(metadata.gid());
    }

    entry
}

/// Captures a file entry to the batch file using the protocol wire format.
///
/// When batch mode is active, encodes the file entry using the protocol
/// flist wire encoder (same format as network transfers) and writes the
/// raw bytes to the batch file via [`BatchWriter::write_data`]. This
/// produces batch files compatible with upstream rsync's `--read-batch`.
///
/// The `FileListWriter` in the context maintains cross-entry compression
/// state (name prefix sharing, same-mode/same-time flags) matching the
/// upstream flist encoding in `flist.c:send_file_entry()`.
fn capture_batch_file_entry(
    context: &mut CopyContext,
    relative_path: &Path,
    metadata: &fs::Metadata,
) -> Result<(), LocalCopyError> {
    if context.batch_writer().is_none() {
        return Ok(());
    }

    let entry = build_protocol_file_entry(relative_path, metadata);

    // Encode the entry into a buffer using the protocol wire format
    let mut buf = Vec::with_capacity(128);
    let flist_writer = context
        .batch_flist_writer_mut()
        .expect("batch_flist_writer must exist when batch_writer is set");
    flist_writer.write_entry(&mut buf, &entry).map_err(|e| {
        LocalCopyError::io("encode batch flist entry", relative_path.to_path_buf(), e)
    })?;

    // Write the raw protocol bytes to the batch file
    let batch_writer_arc = context.batch_writer().unwrap().clone();
    let mut writer_guard = batch_writer_arc.lock().unwrap();
    writer_guard.write_data(&buf).map_err(|e| {
        LocalCopyError::io(
            "write batch file entry",
            relative_path.to_path_buf(),
            std::io::Error::other(e),
        )
    })?;

    Ok(())
}

/// Collects file pairs for parallel checksum prefetching.
///
/// This function extracts source-destination file pairs from the directory plan
/// that are candidates for checksum comparison. Only files where both source and
/// destination exist with matching sizes are included, as size mismatches already
/// indicate the files differ.
///
/// # Arguments
///
/// * `plan` - The directory plan containing planned entries
/// * `destination` - The destination directory path
///
/// # Returns
///
/// A vector of file pairs suitable for parallel checksum computation.
pub(crate) fn collect_file_pairs_for_checksum(
    plan: &DirectoryPlan<'_>,
    destination: &Path,
) -> Vec<FilePair> {
    let mut pairs = Vec::new();

    for planned in &plan.planned_entries {
        if !matches!(planned.action, EntryAction::CopyFile) {
            continue;
        }

        let source_path = &planned.entry.path;
        let target_path = destination.join(Path::new(&planned.entry.file_name));
        let source_size = planned.metadata().len();

        // Check if destination exists and get its size
        let destination_size = match fs::metadata(&target_path) {
            Ok(meta) if meta.file_type().is_file() => meta.len(),
            _ => continue, // Skip if destination doesn't exist or isn't a file
        };

        // Only prefetch if sizes match (different sizes = guaranteed different content)
        if source_size == destination_size {
            pairs.push(FilePair {
                source: source_path.clone(),
                destination: target_path,
                source_size,
                destination_size,
            });
        }
    }

    pairs
}

/// Prefetches file checksums in parallel for a directory.
///
/// When `--checksum` mode is enabled, this function computes file checksums
/// for all eligible file pairs in parallel using rayon. The results are stored
/// in a [`ChecksumCache`] that can be used during the sequential copy phase
/// to avoid recomputing checksums.
///
/// # Arguments
///
/// * `context` - The copy context (used to get checksum algorithm)
/// * `plan` - The directory plan containing files to process
/// * `destination` - The destination directory path
///
/// # Returns
///
/// A populated [`ChecksumCache`] if checksum mode is enabled and there are
/// eligible file pairs, or an empty cache otherwise.
pub(crate) fn prefetch_directory_checksums(
    context: &CopyContext,
    plan: &DirectoryPlan<'_>,
    destination: &Path,
) -> ChecksumCache {
    // Only prefetch if checksum comparison is enabled
    if !context.checksum_enabled() {
        return ChecksumCache::new();
    }

    let pairs = collect_file_pairs_for_checksum(plan, destination);

    // Skip prefetching if no eligible pairs
    if pairs.is_empty() {
        return ChecksumCache::new();
    }

    // Compute checksums in parallel
    let algorithm = context.options().checksum_algorithm();
    ChecksumCache::from_prefetch(&pairs, algorithm)
}

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
    #[cfg(all(unix, any(feature = "acl", feature = "xattr")))]
    let mode = context.mode();
    #[cfg(not(all(unix, any(feature = "acl", feature = "xattr"))))]
    let _mode = context.mode();

    #[cfg(all(unix, feature = "xattr"))]
    let preserve_xattrs = context.xattrs_enabled();
    #[cfg(all(unix, feature = "acl"))]
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
                #[cfg(all(unix, any(feature = "acl", feature = "xattr")))]
                mode,
                #[cfg(all(unix, feature = "xattr"))]
                preserve_xattrs,
                #[cfg(all(unix, feature = "acl"))]
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
                // upstream: flist.c:1289 / sender.c:358 — vanished files produce
                // a warning and set IOERR_VANISHED (exit code 24).
                eprintln!("file has vanished: {}", planned.entry.path.display());
                context.record_io_error();
                if first_entry_io_error.is_none() {
                    first_entry_io_error = Some(error);
                }
            }
            Err(error) if error.is_io_error() && context.options().delete_extraneous() => {
                // Record the I/O error so deletions can be suppressed unless
                // --ignore-errors is set, but continue processing remaining
                // entries so the deletion phase can still execute.
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
            #[cfg(all(unix, any(feature = "acl", feature = "xattr")))]
            mode,
            #[cfg(all(unix, feature = "xattr"))]
            preserve_xattrs,
            #[cfg(all(unix, feature = "acl"))]
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
    use super::*;
    use std::ffi::OsString;
    use std::path::PathBuf;

    use super::super::planner::{DirectoryPlan, EntryAction, PlannedEntry};
    use super::super::support::DirectoryEntry;
    use tempfile::tempdir;

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
        let dir = tempdir().unwrap();
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
        let dir = tempdir().unwrap();
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
        let dir = tempdir().unwrap();
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
        let dir = tempdir().unwrap();
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
