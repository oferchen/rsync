use std::cell::Cell;
use std::fs;
use std::io;
use std::path::Path;
use std::time::{Duration, Instant, UNIX_EPOCH};

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
use super::planner::{EntryAction, apply_pre_transfer_deletions, plan_directory_entries};
use super::support::read_directory_entries_sorted;

/// Helper to capture a file entry to the batch file if batch mode is active.
fn capture_batch_file_entry(
    context: &CopyContext,
    relative_path: &Path,
    metadata: &fs::Metadata,
) -> Result<(), LocalCopyError> {
    if let Some(batch_writer_arc) = context.batch_writer() {
        // Extract metadata for the file entry
        let path_str = relative_path.to_string_lossy().into_owned();

        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;

        #[cfg(unix)]
        let mode = metadata.mode();

        #[cfg(not(unix))]
        let mode = if metadata.is_dir() {
            0o040755 // Directory
        } else if metadata.file_type().is_symlink() {
            0o120777 // Symlink
        } else {
            0o100644 // Regular file
        };

        let size = metadata.len();

        let mtime = metadata
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);

        // Create file entry
        let mut entry = crate::batch::FileEntry::new(path_str, mode, size, mtime);

        // Add uid/gid if preserving ownership
        #[cfg(unix)]
        {
            entry.uid = Some(metadata.uid());
            entry.gid = Some(metadata.gid());
        }

        // Write entry to batch file
        let mut writer = batch_writer_arc.lock().unwrap();
        writer.write_file_entry(&entry).map_err(|e| {
            LocalCopyError::io(
                "write batch file entry",
                relative_path.to_path_buf(),
                std::io::Error::other(e),
            )
        })?;
    }

    Ok(())
}

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

    let mut destination_missing = false;

    let keep_dirlinks = context.keep_dirlinks_enabled();

    match fs::symlink_metadata(destination) {
        Ok(existing) => {
            let file_type = existing.file_type();
            if file_type.is_dir() {
                // Directory already present; nothing to do.
            } else if file_type.is_symlink() && keep_dirlinks {
                let target_metadata = follow_symlink_metadata(destination)?;
                if !target_metadata.file_type().is_dir() {
                    if context.force_replacements_enabled() {
                        context.force_remove_destination(destination, relative, &existing)?;
                        destination_missing = true;
                    } else {
                        return Err(LocalCopyError::invalid_argument(
                            LocalCopyArgumentError::ReplaceNonDirectoryWithDirectory,
                        ));
                    }
                }
            } else if context.force_replacements_enabled() {
                context.force_remove_destination(destination, relative, &existing)?;
                destination_missing = true;
            } else {
                return Err(LocalCopyError::invalid_argument(
                    LocalCopyArgumentError::ReplaceNonDirectoryWithDirectory,
                ));
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            destination_missing = true;
        }
        Err(error) => {
            return Err(LocalCopyError::io(
                "inspect destination directory",
                destination.to_path_buf(),
                error,
            ));
        }
    }

    if destination_missing && context.existing_only_enabled() {
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
        return Ok(false);
    }

    let list_start = Instant::now();
    let entries = read_directory_entries_sorted(source)?;
    context.record_file_list_generation(list_start.elapsed());
    context.register_progress();

    let dir_merge_guard = context.enter_directory(source)?;
    if dir_merge_guard.is_excluded() {
        return Ok(false);
    }
    let _dir_merge_guard = dir_merge_guard;

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

    let mut ensure_directory = |context: &mut CopyContext| -> Result<(), LocalCopyError> {
        if directory_ready.get() {
            return Ok(());
        }

        if context.mode().is_dry_run() {
            if !context.implied_dirs_enabled() {
                if let Some(parent) = destination.parent() {
                    context.prepare_parent_directory(parent)?;
                }
            }
            directory_ready.set(true);
        } else {
            if let Some(parent) = destination.parent() {
                context.prepare_parent_directory(parent)?;
            }
            if context.implied_dirs_enabled() {
                fs::create_dir_all(destination).map_err(|error| {
                    LocalCopyError::io("create directory", destination.to_path_buf(), error)
                })?;
            } else {
                fs::create_dir(destination).map_err(|error| {
                    LocalCopyError::io("create directory", destination.to_path_buf(), error)
                })?;
            }
            context.register_progress();
            context.register_created_path(destination, CreatedEntryKind::Directory, false);
            directory_ready.set(true);
            created_directory_on_disk = true;
        }

        if pending_record.is_none() {
            if let Some((ref rel_path, ref snapshot)) = metadata_record {
                pending_record = Some(LocalCopyRecord::new(
                    rel_path.clone(),
                    LocalCopyAction::DirectoryCreated,
                    0,
                    Some(snapshot.len()),
                    Duration::default(),
                    Some(snapshot.clone()),
                ));
            }
        }

        Ok(())
    };

    if !context.recursive_enabled() {
        ensure_directory(context)?;
        context.summary_mut().record_directory_total();
        if creation_record_pending {
            context.summary_mut().record_directory();
        }
        if let Some(record) = pending_record.take() {
            context.record(record);
        }
        if !context.mode().is_dry_run() {
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
        }
        return Ok(true);
    }

    if !directory_ready.get() && !prune_enabled {
        ensure_directory(context)?;
    }

    let plan = plan_directory_entries(context, &entries, relative, root_device)?;
    apply_pre_transfer_deletions(context, destination, relative, &plan)?;

    for planned in plan.planned_entries {
        let file_name = &planned.entry.file_name;
        let target_path = destination.join(Path::new(file_name));
        let entry_metadata = planned.metadata();
        let record_relative = non_empty_path(planned.relative.as_path());

        match planned.action {
            EntryAction::SkipExcluded => {}
            EntryAction::SkipNonRegular => {
                if entry_metadata.file_type().is_symlink() {
                    context.summary_mut().record_symlink_total();
                }
                context.record_skipped_non_regular(record_relative);
            }
            EntryAction::SkipMountPoint => {
                context.record_skipped_mount_point(record_relative);
            }
            EntryAction::CopyDirectory => {
                ensure_directory(context)?;
                // Capture directory entry to batch file
                if let Some(rel_path) = record_relative {
                    capture_batch_file_entry(context, rel_path, entry_metadata)?;
                }
                let child_kept = copy_directory_recursive(
                    context,
                    planned.entry.path.as_path(),
                    &target_path,
                    entry_metadata,
                    Some(planned.relative.as_path()),
                    root_device,
                )?;
                if child_kept {
                    kept_any = true;
                }
            }
            EntryAction::CopyFile => {
                ensure_directory(context)?;
                // Capture file entry to batch file
                if let Some(rel_path) = record_relative {
                    capture_batch_file_entry(context, rel_path, entry_metadata)?;
                }
                copy_file(
                    context,
                    planned.entry.path.as_path(),
                    &target_path,
                    entry_metadata,
                    Some(planned.relative.as_path()),
                )?;
                kept_any = true;
            }
            EntryAction::CopySymlink => {
                ensure_directory(context)?;
                // Capture symlink entry to batch file
                if let Some(rel_path) = record_relative {
                    capture_batch_file_entry(context, rel_path, entry_metadata)?;
                }
                let metadata_options = context.metadata_options();
                copy_symlink(
                    context,
                    planned.entry.path.as_path(),
                    &target_path,
                    entry_metadata,
                    &metadata_options,
                    Some(planned.relative.as_path()),
                )?;
                kept_any = true;
            }
            EntryAction::CopyFifo => {
                ensure_directory(context)?;
                // Capture FIFO entry to batch file
                if let Some(rel_path) = record_relative {
                    capture_batch_file_entry(context, rel_path, entry_metadata)?;
                }
                let metadata_options = context.metadata_options();
                copy_fifo(
                    context,
                    planned.entry.path.as_path(),
                    &target_path,
                    entry_metadata,
                    &metadata_options,
                    Some(planned.relative.as_path()),
                )?;
                kept_any = true;
            }
            EntryAction::CopyDevice => {
                ensure_directory(context)?;
                // Capture device entry to batch file
                if let Some(rel_path) = record_relative {
                    capture_batch_file_entry(context, rel_path, entry_metadata)?;
                }
                let metadata_options = context.metadata_options();
                copy_device(
                    context,
                    planned.entry.path.as_path(),
                    &target_path,
                    entry_metadata,
                    &metadata_options,
                    Some(planned.relative.as_path()),
                )?;
                kept_any = true;
            }
            EntryAction::CopyDeviceAsFile => {
                ensure_directory(context)?;
                // Capture device-as-file entry to batch file
                if let Some(rel_path) = record_relative {
                    capture_batch_file_entry(context, rel_path, entry_metadata)?;
                }
                copy_file(
                    context,
                    planned.entry.path.as_path(),
                    &target_path,
                    entry_metadata,
                    Some(planned.relative.as_path()),
                )?;
                kept_any = true;
            }
        }
    }

    if plan.deletion_enabled {
        match plan.delete_timing.unwrap_or(DeleteTiming::During) {
            DeleteTiming::Before => {}
            DeleteTiming::During => {
                delete_extraneous_entries(context, destination, relative, &plan.keep_names)?;
            }
            DeleteTiming::Delay | DeleteTiming::After => {
                let relative_owned = relative.map(Path::to_path_buf);
                context.defer_deletion(destination.to_path_buf(), relative_owned, plan.keep_names);
            }
        }
    }

    if prune_enabled && !kept_any {
        if created_directory_on_disk {
            fs::remove_dir(destination).map_err(|error| {
                LocalCopyError::io("remove empty directory", destination.to_path_buf(), error)
            })?;
            if context
                .last_created_entry_path()
                .is_some_and(|path| path == destination)
            {
                context.pop_last_created_entry();
            }
        }
        return Ok(false);
    }

    context.summary_mut().record_directory_total();
    if creation_record_pending {
        context.summary_mut().record_directory();
    }
    if let Some(record) = pending_record {
        context.record(record);
    }

    if !context.mode().is_dry_run() {
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
    }

    Ok(true)
}
