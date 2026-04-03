//! Single planned entry processing during directory recursion.
//!
//! Dispatches each [`PlannedEntry`] to the appropriate copy handler based on
//! its [`EntryAction`]. Mirrors the per-entry dispatch in upstream
//! `generator.c:recv_generator()`.
use std::path::Path;

use crate::local_copy::{
    CopyContext, LocalCopyError, copy_device, copy_fifo, copy_file, copy_symlink,
};

use super::super::super::non_empty_path;
use super::super::planner::{EntryAction, PlannedEntry};
use super::batch::capture_batch_file_entry;
use super::copy_directory_recursive;

/// Processes a single planned entry during directory recursion.
///
/// This handles all entry types: directories, files, symlinks, FIFOs, and devices.
/// Returns `true` if this entry should count as "kept" for pruning purposes.
pub(super) fn process_planned_entry(
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
            // Write NDX + iflags + sum_head preamble to the batch delta
            // buffer before file token data. This produces the correct
            // upstream batch format: flist entries first (already written
            // by capture_batch_file_entry above), then NDX-framed per-file
            // delta data (buffered separately and appended after flist end).
            context.begin_batch_file_delta()?;
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
