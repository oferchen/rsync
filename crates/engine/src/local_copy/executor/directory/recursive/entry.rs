//! Single planned entry processing during directory recursion.
//!
//! Dispatches each [`PlannedEntry`] to the appropriate copy handler based on
//! its [`EntryAction`]. Mirrors the per-entry dispatch in upstream
//! `generator.c:recv_generator()`.
use std::path::{Path, PathBuf};

use logging::info_log;

use crate::local_copy::{
    CopyContext, LocalCopyError, copy_device, copy_fifo, copy_file, copy_symlink,
};

use super::super::super::non_empty_path;
use super::super::super::transcode_filename_component;
use super::super::planner::{EntryAction, PlannedEntry};
use super::batch::capture_batch_file_entry;
use super::copy_directory_recursive;

/// Processes a single planned entry during directory recursion.
///
/// This handles all entry types: directories, files, symlinks, FIFOs, and devices.
/// Returns `true` if this entry should count as "kept" for pruning purposes.
///
/// `target_buf` is a reusable buffer pre-seeded with the destination directory
/// path. Each call pushes the entry name, uses the resulting path, then pops it
/// back, avoiding a per-entry `PathBuf` allocation from `Path::join`.
pub(super) fn process_planned_entry(
    context: &mut CopyContext,
    planned: &PlannedEntry<'_>,
    target_buf: &mut PathBuf,
    ensure_directory: &mut impl FnMut(&mut CopyContext) -> Result<(), LocalCopyError>,
    root_device: Option<u64>,
) -> Result<bool, LocalCopyError> {
    let entry_metadata = planned.metadata();
    let record_relative = non_empty_path(planned.relative.as_path());

    // Handle skip actions first (no directory creation, batch capture, or
    // target-path computation needed).
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
            // upstream: flist.c:1319 - INFO_GTE(MOUNT, 1) gates
            // `rprintf(FINFO, "[%s] skipping mount-point dir %s", who_am_i(), thisname)`
            // when `--one-file-system` (`-xx`) prunes a cross-device directory.
            // The role prefix (`[sender]`) is added downstream by the renderer.
            info_log!(
                Mount,
                1,
                "skipping mount-point dir {}",
                planned.entry.path.display()
            );
            context.record_skipped_mount_point(record_relative);
            return Ok(false);
        }
        _ => {}
    }

    // upstream: flist.c:1579-1603 (sender) + flist.c:738-754 (receiver) -
    // the receiver opens the file with the iconv-converted name. For
    // local-copy the two contexts compose to LOCAL -> REMOTE; apply that
    // transcoding here before joining onto the destination directory.
    let file_name = &planned.entry.file_name;
    let dest_name = transcode_filename_component(file_name, context.options().iconv());
    target_buf.push(Path::new(&*dest_name));

    let result = dispatch_copy_action(
        context,
        planned,
        target_buf,
        entry_metadata,
        record_relative,
        ensure_directory,
        root_device,
    );

    target_buf.pop();
    result
}

/// Dispatches the copy action for a planned entry after the target path
/// has been pushed onto `target_buf`. Extracted so that the caller can
/// unconditionally pop the buffer regardless of success or failure.
fn dispatch_copy_action(
    context: &mut CopyContext,
    planned: &PlannedEntry<'_>,
    target_buf: &mut PathBuf,
    entry_metadata: &std::fs::Metadata,
    record_relative: Option<&Path>,
    ensure_directory: &mut impl FnMut(&mut CopyContext) -> Result<(), LocalCopyError>,
    root_device: Option<u64>,
) -> Result<bool, LocalCopyError> {
    // All copy actions share: ensure parent directory exists + capture to batch
    ensure_directory(context)?;
    let source = planned.entry.path.as_path();
    if let Some(rel_path) = record_relative {
        capture_batch_file_entry(context, source, rel_path, entry_metadata, false)?;
    }
    let relative = Some(planned.relative.as_path());

    match planned.action {
        EntryAction::CopyDirectory => copy_directory_recursive(
            context,
            source,
            target_buf,
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
            copy_file(context, source, target_buf, entry_metadata, relative)
        }
        EntryAction::CopySymlink => {
            let metadata_options = context.metadata_options();
            copy_symlink(
                context,
                source,
                target_buf,
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
                target_buf,
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
                target_buf,
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
