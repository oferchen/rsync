//! Deletion helpers for extraneous or source entries.

use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use logging::{debug_log, info_log};

use crate::local_copy::{CopyContext, LocalCopyAction, LocalCopyError, LocalCopyRecord};

/// Deletes entries in `destination` that are not in `source_entries`.
///
/// The `source_entries` parameter accepts any slice of types convertible to `&OsStr`,
/// including `&[OsString]` (owned) and `&[&OsString]` (borrowed), avoiding allocation
/// when borrowing from an existing data structure.
pub(crate) fn delete_extraneous_entries<S: AsRef<OsStr>>(
    context: &mut CopyContext,
    destination: &Path,
    relative: Option<&Path>,
    source_entries: &[S],
) -> Result<(), LocalCopyError> {
    let mut skipped_due_to_limit = 0u64;
    // Build HashSet from references without cloning - use OsStr for comparison
    let keep: HashSet<&OsStr> = source_entries.iter().map(|s| s.as_ref()).collect();

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

        if keep.contains(name.as_os_str()) {
            continue;
        }

        // Protect relative partial-dir from deletion (upstream rsync behavior).
        if let Some(ref protected) = protected_partial_dir_name {
            if name.as_os_str() == protected.as_os_str() {
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
            info_log!(Del, 1, "deleting directory {}", entry_relative.display());
        } else {
            info_log!(Del, 1, "deleting {}", entry_relative.display());
        }
        remove_extraneous_path(&path, file_type)?;
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
