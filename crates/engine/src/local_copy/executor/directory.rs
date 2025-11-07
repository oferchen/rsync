//! Directory traversal and recursive copy logic.

use std::cell::Cell;
use std::cmp::Ordering;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::local_copy::overrides::device_identifier;
#[cfg(feature = "acl")]
use crate::local_copy::sync_acls_if_requested;
#[cfg(feature = "xattr")]
use crate::local_copy::sync_xattrs_if_requested;
use crate::local_copy::{
    CopyContext, CreatedEntryKind, DeleteTiming, LocalCopyAction, LocalCopyArgumentError,
    LocalCopyError, LocalCopyMetadata, LocalCopyRecord, copy_device, copy_fifo, copy_file,
    copy_symlink, delete_extraneous_entries, follow_symlink_metadata, map_metadata_error,
};
use rsync_meta::apply_directory_metadata_with_options;

use super::{non_empty_path, symlink_target_is_safe};

#[derive(Debug)]
pub(crate) struct DirectoryEntry {
    file_name: OsString,
    path: PathBuf,
    metadata: fs::Metadata,
}

pub(crate) fn copy_directory_recursive(
    context: &mut CopyContext,
    source: &Path,
    destination: &Path,
    metadata: &fs::Metadata,
    relative: Option<&Path>,
    root_device: Option<u64>,
) -> Result<bool, LocalCopyError> {
    #[cfg(any(feature = "acl", feature = "xattr"))]
    let mode = context.mode();
    #[cfg(not(any(feature = "acl", feature = "xattr")))]
    let _mode = context.mode();
    #[cfg(feature = "xattr")]
    let preserve_xattrs = context.xattrs_enabled();
    #[cfg(feature = "acl")]
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
                    return Err(LocalCopyError::invalid_argument(
                        LocalCopyArgumentError::ReplaceNonDirectoryWithDirectory,
                    ));
                }
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
            #[cfg(feature = "xattr")]
            sync_xattrs_if_requested(
                preserve_xattrs,
                mode,
                source,
                destination,
                true,
                context.filter_program(),
            )?;
            #[cfg(feature = "acl")]
            sync_acls_if_requested(preserve_acls, mode, source, destination, true)?;
        }
        return Ok(true);
    }

    if !directory_ready.get() && !prune_enabled {
        ensure_directory(context)?;
    }

    #[derive(Clone, Copy)]
    enum EntryAction {
        SkipExcluded,
        SkipNonRegular,
        SkipMountPoint,
        CopyDirectory,
        CopyFile,
        CopySymlink,
        CopyFifo,
        CopyDevice,
    }

    struct PlannedEntry<'a> {
        entry: &'a DirectoryEntry,
        relative: PathBuf,
        action: EntryAction,
        metadata_override: Option<fs::Metadata>,
    }

    impl<'a> PlannedEntry<'a> {
        fn metadata(&self) -> &fs::Metadata {
            self.metadata_override
                .as_ref()
                .unwrap_or(&self.entry.metadata)
        }
    }

    let deletion_enabled = context.options().delete_extraneous();
    let delete_timing = context.delete_timing();
    let mut keep_names = if deletion_enabled {
        Vec::with_capacity(entries.len())
    } else {
        Vec::new()
    };
    let mut planned_entries = Vec::with_capacity(entries.len());

    for entry in entries.iter() {
        context.enforce_timeout()?;
        context.register_progress();

        let file_name = entry.file_name.clone();
        let entry_metadata = &entry.metadata;
        let entry_type = entry_metadata.file_type();
        let mut metadata_override = None;
        let mut effective_type = entry_type;
        if entry_type.is_symlink()
            && (context.copy_links_enabled() || context.copy_dirlinks_enabled())
        {
            match follow_symlink_metadata(entry.path.as_path()) {
                Ok(target_metadata) => {
                    let target_type = target_metadata.file_type();
                    if context.copy_links_enabled()
                        || (context.copy_dirlinks_enabled() && target_type.is_dir())
                    {
                        effective_type = target_type;
                        metadata_override = Some(target_metadata);
                    }
                }
                Err(error) => {
                    if context.copy_links_enabled() {
                        return Err(error);
                    }
                }
            }
        }
        let relative_path = match relative {
            Some(base) => base.join(Path::new(&file_name)),
            None => PathBuf::from(Path::new(&file_name)),
        };

        let mut keep_name = true;

        let mut action = if !context.allows(&relative_path, effective_type.is_dir()) {
            // Skip excluded entries while optionally allowing deletion sweeps to remove them.
            if context.options().delete_excluded_enabled() {
                keep_name = false;
            }
            EntryAction::SkipExcluded
        } else if entry_type.is_dir() {
            EntryAction::CopyDirectory
        } else if effective_type.is_file() {
            EntryAction::CopyFile
        } else if effective_type.is_dir() {
            EntryAction::CopyDirectory
        } else if entry_type.is_symlink() && !context.copy_links_enabled() {
            EntryAction::CopySymlink
        } else if is_fifo(&effective_type) {
            if context.specials_enabled() {
                EntryAction::CopyFifo
            } else {
                keep_name = false;
                EntryAction::SkipNonRegular
            }
        } else if is_device(&effective_type) {
            if context.devices_enabled() {
                EntryAction::CopyDevice
            } else {
                keep_name = false;
                EntryAction::SkipNonRegular
            }
        } else {
            return Err(LocalCopyError::invalid_argument(
                LocalCopyArgumentError::UnsupportedFileType,
            ));
        };

        if matches!(action, EntryAction::CopySymlink)
            && context.safe_links_enabled()
            && context.copy_unsafe_links_enabled()
        {
            match fs::read_link(entry.path.as_path()) {
                Ok(target) => {
                    if !symlink_target_is_safe(&target, relative_path.as_path()) {
                        match follow_symlink_metadata(entry.path.as_path()) {
                            Ok(target_metadata) => {
                                let target_type = target_metadata.file_type();
                                if target_type.is_dir() {
                                    action = EntryAction::CopyDirectory;
                                    metadata_override = Some(target_metadata);
                                } else if target_type.is_file() {
                                    action = EntryAction::CopyFile;
                                    metadata_override = Some(target_metadata);
                                } else if is_fifo(&target_type) {
                                    if context.specials_enabled() {
                                        action = EntryAction::CopyFifo;
                                        metadata_override = Some(target_metadata);
                                    } else {
                                        keep_name = false;
                                        action = EntryAction::SkipNonRegular;
                                        metadata_override = None;
                                    }
                                } else if is_device(&target_type) {
                                    if context.devices_enabled() {
                                        action = EntryAction::CopyDevice;
                                        metadata_override = Some(target_metadata);
                                    } else {
                                        keep_name = false;
                                        action = EntryAction::SkipNonRegular;
                                        metadata_override = None;
                                    }
                                } else {
                                    return Err(LocalCopyError::invalid_argument(
                                        LocalCopyArgumentError::UnsupportedFileType,
                                    ));
                                }
                            }
                            Err(error) => {
                                return Err(error);
                            }
                        }
                    }
                }
                Err(error) => {
                    return Err(LocalCopyError::io(
                        "read symbolic link",
                        entry.path.to_path_buf(),
                        error,
                    ));
                }
            }
        }

        if matches!(action, EntryAction::CopyDirectory) && context.one_file_system_enabled() {
            if let Some(root) = root_device {
                if let Some(entry_device) = device_identifier(
                    entry.path.as_path(),
                    metadata_override.as_ref().unwrap_or(entry_metadata),
                ) {
                    if entry_device != root {
                        action = EntryAction::SkipMountPoint;
                    }
                }
            }
        }

        if deletion_enabled && keep_name {
            let preserve_name = match delete_timing {
                Some(DeleteTiming::Before) => matches!(
                    action,
                    EntryAction::CopyDirectory
                        | EntryAction::SkipExcluded
                        | EntryAction::SkipMountPoint
                ),
                _ => true,
            };

            if preserve_name {
                keep_names.push(file_name.clone());
            }
        }

        planned_entries.push(PlannedEntry {
            entry,
            relative: relative_path,
            action,
            metadata_override,
        });
    }

    if deletion_enabled && matches!(delete_timing, Some(DeleteTiming::Before)) {
        delete_extraneous_entries(context, destination, relative, &keep_names)?;
    }

    for planned in planned_entries {
        let file_name = &planned.entry.file_name;
        let target_path = destination.join(Path::new(file_name));
        let entry_metadata = planned.metadata();
        let record_relative = non_empty_path(planned.relative.as_path());

        match planned.action {
            EntryAction::SkipExcluded => {}
            EntryAction::SkipNonRegular => {
                context.record_skipped_non_regular(record_relative);
            }
            EntryAction::SkipMountPoint => {
                context.record_skipped_mount_point(record_relative);
            }
            EntryAction::CopyDirectory => {
                ensure_directory(context)?;
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
        }
    }

    if deletion_enabled {
        match delete_timing.unwrap_or(DeleteTiming::During) {
            DeleteTiming::Before => {}
            DeleteTiming::During => {
                delete_extraneous_entries(context, destination, relative, &keep_names)?;
            }
            DeleteTiming::Delay | DeleteTiming::After => {
                let relative_owned = relative.map(Path::to_path_buf);
                context.defer_deletion(destination.to_path_buf(), relative_owned, keep_names);
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
        #[cfg(feature = "xattr")]
        sync_xattrs_if_requested(
            preserve_xattrs,
            mode,
            source,
            destination,
            true,
            context.filter_program(),
        )?;
        #[cfg(feature = "acl")]
        sync_acls_if_requested(preserve_acls, mode, source, destination, true)?;
    }

    Ok(true)
}

pub(crate) fn read_directory_entries_sorted(
    path: &Path,
) -> Result<Vec<DirectoryEntry>, LocalCopyError> {
    let mut entries = Vec::new();
    let read_dir = fs::read_dir(path)
        .map_err(|error| LocalCopyError::io("read directory", path.to_path_buf(), error))?;

    for entry in read_dir {
        let entry = entry.map_err(|error| {
            LocalCopyError::io("read directory entry", path.to_path_buf(), error)
        })?;
        let entry_path = entry.path();
        let metadata = fs::symlink_metadata(&entry_path).map_err(|error| {
            LocalCopyError::io("inspect directory entry", entry_path.to_path_buf(), error)
        })?;
        entries.push(DirectoryEntry {
            file_name: entry.file_name(),
            path: entry_path,
            metadata,
        });
    }

    entries.sort_by(|a, b| compare_file_names(&a.file_name, &b.file_name));
    Ok(entries)
}

fn compare_file_names(left: &OsStr, right: &OsStr) -> Ordering {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;

        left.as_bytes().cmp(right.as_bytes())
    }

    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;

        let left_wide: Vec<u16> = left.encode_wide().collect();
        let right_wide: Vec<u16> = right.encode_wide().collect();
        left_wide.cmp(&right_wide)
    }

    #[cfg(not(any(unix, windows)))]
    {
        left.to_string_lossy().cmp(&right.to_string_lossy())
    }
}

pub(crate) fn is_fifo(file_type: &fs::FileType) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileTypeExt;

        file_type.is_fifo()
    }

    #[cfg(not(unix))]
    {
        let _ = file_type;
        false
    }
}

pub(crate) fn is_device(file_type: &fs::FileType) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileTypeExt;

        file_type.is_char_device() || file_type.is_block_device()
    }

    #[cfg(not(unix))]
    {
        let _ = file_type;
        false
    }
}
