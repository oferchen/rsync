use std::fs;
use std::io::{Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::local_copy::{
    CopyContext, CreatedEntryKind, DeferredUpdate, FinalizeMetadataParams, LocalCopyAction,
    LocalCopyError, LocalCopyExecution, LocalCopyMetadata, LocalCopyRecord,
};

#[cfg(feature = "acl")]
use crate::local_copy::sync_acls_if_requested;
#[cfg(feature = "xattr")]
use crate::local_copy::sync_xattrs_if_requested;

use rsync_meta::{MetadataOptions, apply_file_metadata_with_options};

use super::super::super::super::COPY_BUFFER_SIZE;
use super::super::append::{AppendMode, determine_append_mode};
use super::super::comparison::{CopyComparison, build_delta_signature, should_skip_copy};
use super::super::guard::{DestinationWriteGuard, remove_incomplete_destination};
use super::super::preallocate::maybe_preallocate_destination;

#[allow(clippy::too_many_arguments)]
pub(super) fn execute_transfer(
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
    append_allowed: bool,
    append_verify: bool,
    whole_file_enabled: bool,
    inplace_enabled: bool,
    partial_enabled: bool,
    use_sparse_writes: bool,
    compress_enabled: bool,
    size_only_enabled: bool,
    checksum_enabled: bool,
    mode: LocalCopyExecution,
    #[cfg(feature = "xattr")] preserve_xattrs: bool,
    #[cfg(feature = "acl")] preserve_acls: bool,
    copy_source_override: Option<PathBuf>,
) -> Result<(), LocalCopyError> {
    let file_size = metadata.len();

    if let Some(existing) = existing_metadata {
        if should_skip_copy(CopyComparison {
            source_path: source,
            source: metadata,
            destination_path: destination,
            destination: existing,
            options: &metadata_options,
            size_only: size_only_enabled,
            checksum: checksum_enabled,
            checksum_algorithm: context.options().checksum_algorithm(),
            modify_window: context.options().modify_window(),
        }) {
            apply_file_metadata_with_options(destination, metadata, metadata_options.clone())
                .map_err(crate::local_copy::map_metadata_error)?;
            #[cfg(feature = "xattr")]
            sync_xattrs_if_requested(preserve_xattrs, mode, source, destination, true)?;
            #[cfg(feature = "acl")]
            sync_acls_if_requested(preserve_acls, mode, source, destination, true)?;
            context.record_hard_link(metadata, destination);
            context.summary_mut().record_regular_file_matched();
            let metadata_snapshot = LocalCopyMetadata::from_metadata(metadata, None);
            let total_bytes = Some(metadata_snapshot.len());
            context.record(LocalCopyRecord::new(
                record_path.to_path_buf(),
                LocalCopyAction::MetadataReused,
                0,
                total_bytes,
                Duration::default(),
                Some(metadata_snapshot),
            ));
            return Ok(());
        }
    }

    let mut reader = fs::File::open(source)
        .map_err(|error| LocalCopyError::io("copy file", source.to_path_buf(), error))?;
    let append_mode = determine_append_mode(
        append_allowed,
        append_verify,
        &mut reader,
        source,
        destination,
        existing_metadata,
        file_size,
    )?;
    let append_offset = match append_mode {
        AppendMode::Append(offset) => offset,
        AppendMode::Disabled => 0,
    };
    reader
        .seek(SeekFrom::Start(append_offset))
        .map_err(|error| LocalCopyError::io("copy file", source.to_path_buf(), error))?;
    let delta_signature = if append_offset == 0 && !whole_file_enabled && !inplace_enabled {
        match existing_metadata {
            Some(existing) if existing.is_file() => build_delta_signature(destination, existing)?,
            _ => None,
        }
    } else {
        None
    };

    let copy_source = copy_source_override.as_deref().unwrap_or(source);
    let mut reader = fs::File::open(copy_source)
        .map_err(|error| LocalCopyError::io("copy file", copy_source.to_path_buf(), error))?;
    if append_offset > 0 {
        reader
            .seek(SeekFrom::Start(append_offset))
            .map_err(|error| LocalCopyError::io("copy file", copy_source.to_path_buf(), error))?;
    }
    let mut guard = None;
    let mut staging_path: Option<PathBuf> = None;

    let mut writer = if append_offset > 0 {
        let mut file = fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(destination)
            .map_err(|error| LocalCopyError::io("copy file", destination.to_path_buf(), error))?;
        file.seek(SeekFrom::Start(append_offset))
            .map_err(|error| LocalCopyError::io("copy file", destination.to_path_buf(), error))?;
        file
    } else if inplace_enabled {
        fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(destination)
            .map_err(|error| LocalCopyError::io("copy file", destination.to_path_buf(), error))?
    } else {
        let (new_guard, file) = DestinationWriteGuard::new(
            destination,
            partial_enabled,
            context.partial_directory_path(),
            context.temp_directory_path(),
        )?;
        staging_path = Some(new_guard.staging_path().to_path_buf());
        guard = Some(new_guard);
        file
    };
    let preallocate_target = guard
        .as_ref()
        .map(|existing_guard| existing_guard.staging_path())
        .unwrap_or(destination);
    maybe_preallocate_destination(
        &mut writer,
        preallocate_target,
        file_size,
        append_offset,
        context.preallocate_enabled(),
    )?;
    let mut buffer = vec![0u8; COPY_BUFFER_SIZE];

    let start = Instant::now();

    let copy_result = context.copy_file_contents(
        &mut reader,
        &mut writer,
        &mut buffer,
        use_sparse_writes,
        compress_enabled,
        source,
        destination,
        record_path,
        delta_signature.as_ref(),
        file_size,
        append_offset,
        start,
    );

    drop(writer);

    let staging_path_for_links = guard
        .as_ref()
        .map(|existing_guard| existing_guard.staging_path().to_path_buf())
        .or_else(|| staging_path.take());
    let delay_updates_enabled = context.delay_updates_enabled();

    let outcome = match copy_result {
        Ok(outcome) => {
            if let Err(timeout_error) = context.enforce_timeout() {
                if let Some(guard) = guard.take() {
                    guard.discard();
                }

                if existing_metadata.is_none() {
                    remove_incomplete_destination(destination);
                }

                return Err(timeout_error);
            }
            outcome
        }
        Err(error) => {
            if let Some(guard) = guard.take() {
                guard.discard();
            }

            if existing_metadata.is_none() {
                remove_incomplete_destination(destination);
            }

            return Err(error);
        }
    };

    context.register_created_path(
        destination,
        CreatedEntryKind::File,
        destination_previously_existed,
    );

    let hard_link_path = if delay_updates_enabled {
        staging_path_for_links.as_deref().unwrap_or(destination)
    } else {
        destination
    };
    context.record_hard_link(metadata, hard_link_path);
    let elapsed = start.elapsed();
    let compressed_bytes = outcome.compressed_bytes();
    context
        .summary_mut()
        .record_file(file_size, outcome.literal_bytes(), compressed_bytes);
    context.summary_mut().record_elapsed(elapsed);
    let metadata_snapshot = LocalCopyMetadata::from_metadata(metadata, None);
    let total_bytes = Some(metadata_snapshot.len());
    context.record(
        LocalCopyRecord::new(
            record_path.to_path_buf(),
            LocalCopyAction::DataCopied,
            outcome.literal_bytes(),
            total_bytes,
            elapsed,
            Some(metadata_snapshot),
        )
        .with_creation(!destination_previously_existed),
    );

    if let Err(timeout_error) = context.enforce_timeout() {
        if existing_metadata.is_none() {
            remove_incomplete_destination(destination);
        }

        return Err(timeout_error);
    }

    let relative_for_removal = Some(record_path.to_path_buf());
    if let Some(guard) = guard {
        if delay_updates_enabled {
            let destination_path = guard.final_path().to_path_buf();
            let update = DeferredUpdate::new(
                guard,
                metadata.clone(),
                metadata_options.clone(),
                mode,
                source.to_path_buf(),
                relative_for_removal.clone(),
                destination_path,
                file_type,
                destination_previously_existed,
                #[cfg(feature = "xattr")]
                preserve_xattrs,
                #[cfg(feature = "acl")]
                preserve_acls,
            );
            context.register_deferred_update(update);
        } else {
            let destination_path = guard.final_path().to_path_buf();
            guard.commit()?;
            context.apply_metadata_and_finalize(
                destination_path.as_path(),
                FinalizeMetadataParams::new(
                    metadata,
                    metadata_options.clone(),
                    mode,
                    source,
                    relative_for_removal.as_deref(),
                    file_type,
                    destination_previously_existed,
                    #[cfg(feature = "xattr")]
                    preserve_xattrs,
                    #[cfg(feature = "acl")]
                    preserve_acls,
                ),
            )?;
        }
    } else {
        context.apply_metadata_and_finalize(
            destination,
            FinalizeMetadataParams::new(
                metadata,
                metadata_options,
                mode,
                source,
                relative,
                file_type,
                destination_previously_existed,
                #[cfg(feature = "xattr")]
                preserve_xattrs,
                #[cfg(feature = "acl")]
                preserve_acls,
            ),
        )?;
    }

    Ok(())
}
