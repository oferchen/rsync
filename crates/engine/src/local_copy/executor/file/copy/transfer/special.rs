//! Handles non-regular files that are copied as empty regular files.
//!
//! When rsync encounters a non-regular file (device, FIFO, socket) that must
//! be transferred as a regular file, this module creates an empty destination
//! file, records the transfer, and applies metadata. This mirrors upstream
//! rsync behavior where special files are represented as zero-length regular
//! files when the receiver cannot create the special type.

use std::fs;
use std::path::Path;
use std::time::Instant;

use ::metadata::MetadataOptions;

use crate::local_copy::{
    CopyContext, CreatedEntryKind, LocalCopyAction, LocalCopyChangeSet, LocalCopyError,
    LocalCopyExecution, LocalCopyMetadata, LocalCopyRecord,
};

use super::super::super::guard::{DestinationWriteGuard, remove_incomplete_destination};
use super::TransferFlags;
use super::finalize::finalize_guard_and_metadata;

/// Copies a non-regular file as an empty regular destination file.
///
/// Creates the destination (via temp file or inplace), records the transfer
/// with zero literal bytes, and applies metadata. Used for devices, FIFOs,
/// and other special file types that cannot be reproduced on the receiver.
#[allow(clippy::too_many_arguments)]
pub(in crate::local_copy) fn copy_special_as_regular_file(
    context: &mut CopyContext,
    source: &Path,
    destination: &Path,
    metadata: &fs::Metadata,
    metadata_options: MetadataOptions,
    record_path: &Path,
    existing_metadata: Option<&fs::Metadata>,
    destination_previously_existed: bool,
    file_type: fs::FileType,
    relative: Option<&Path>,
    mode: LocalCopyExecution,
    flags: TransferFlags,
) -> Result<(), LocalCopyError> {
    #[cfg(not(all(unix, any(feature = "xattr", feature = "acl"))))]
    let _ = mode;

    let start = Instant::now();
    let partial_enabled = context.partial_enabled();
    let inplace_enabled = context.inplace_enabled();
    let delay_updates_enabled = context.delay_updates_enabled();
    let mut guard: Option<DestinationWriteGuard> = None;

    if inplace_enabled {
        let _file = fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(destination)
            .map_err(|error| LocalCopyError::io("copy file", destination, error))?;
    } else {
        let (new_guard, _file) = DestinationWriteGuard::new(
            destination,
            partial_enabled,
            context.partial_directory_path(),
            context.temp_directory_path(),
        )?;
        guard = Some(new_guard);
    }

    context.register_created_path(
        destination,
        CreatedEntryKind::File,
        destination_previously_existed,
    );

    let hard_link_path = if delay_updates_enabled {
        guard
            .as_ref()
            .map_or(destination, |existing_guard| existing_guard.staging_path())
    } else {
        destination
    };
    context.record_hard_link(metadata, hard_link_path);

    let elapsed = start.elapsed();
    context.summary_mut().record_file(metadata.len(), 0, None);
    context.summary_mut().record_elapsed(elapsed);
    let metadata_snapshot = LocalCopyMetadata::from_metadata(metadata, None);
    let total_bytes = Some(metadata_snapshot.len());
    let change_set = LocalCopyChangeSet::for_file(
        metadata,
        existing_metadata,
        &metadata_options,
        destination_previously_existed,
        false,
        flags.xattrs_enabled(),
        flags.acls_enabled(),
    );
    context.record(
        LocalCopyRecord::new(
            record_path.to_path_buf(),
            LocalCopyAction::DataCopied,
            0,
            total_bytes,
            elapsed,
            Some(metadata_snapshot),
        )
        .with_change_set(change_set)
        .with_creation(!destination_previously_existed),
    );

    if let Err(timeout_error) = context.enforce_timeout() {
        if let Some(existing_guard) = guard {
            existing_guard.discard();
        }

        if existing_metadata.is_none() {
            remove_incomplete_destination(destination);
        }

        return Err(timeout_error);
    }

    let mut no_writer: Option<fs::File> = None;
    finalize_guard_and_metadata(
        context,
        guard,
        destination,
        metadata,
        metadata_options,
        mode,
        source,
        record_path,
        relative,
        file_type,
        destination_previously_existed,
        delay_updates_enabled,
        &mut no_writer,
        #[cfg(all(unix, feature = "xattr"))]
        flags.preserve_xattrs,
        #[cfg(all(any(unix, windows), feature = "acl"))]
        flags.preserve_acls,
    )?;

    Ok(())
}
