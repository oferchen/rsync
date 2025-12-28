use std::fs;
use std::io::{self, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

#[cfg(any(target_os = "linux", target_os = "android"))]
use std::os::unix::fs::OpenOptionsExt;

#[cfg(any(target_os = "linux", target_os = "android"))]
use libc::{self, EACCES, EINVAL, ENOTSUP, EPERM, EROFS};

#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::local_copy::{
    CopyContext, CreatedEntryKind, DeferredUpdate, FinalizeMetadataParams, LocalCopyAction,
    LocalCopyChangeSet, LocalCopyError, LocalCopyExecution, LocalCopyMetadata, LocalCopyRecord,
};

#[cfg(test)]
static FSYNC_CALL_COUNT: AtomicUsize = AtomicUsize::new(0);

#[cfg(all(unix, feature = "acl"))]
use crate::local_copy::sync_acls_if_requested;
#[cfg(all(unix, feature = "xattr"))]
use crate::local_copy::sync_xattrs_if_requested;

use ::metadata::{MetadataOptions, apply_file_metadata_with_options};

use super::super::super::super::COPY_BUFFER_SIZE;
use super::super::append::{AppendMode, determine_append_mode};
use super::super::comparison::{
    CopyComparison, build_delta_signature, files_checksum_match, should_skip_copy,
};
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
    ignore_times_enabled: bool,
    checksum_enabled: bool,
    mode: LocalCopyExecution,
    #[cfg(all(unix, feature = "xattr"))] preserve_xattrs: bool,
    #[cfg(all(unix, feature = "acl"))] preserve_acls: bool,
    copy_source_override: Option<PathBuf>,
) -> Result<(), LocalCopyError> {
    // keep the param used on non-unix builds to avoid warnings
    #[cfg(not(all(unix, any(feature = "xattr", feature = "acl"))))]
    let _ = mode;

    let file_size = metadata.len();

    // fast-path: see if destination is already in-sync
    if let Some(existing) = existing_metadata {
        let mut skip = should_skip_copy(CopyComparison {
            source_path: source,
            source: metadata,
            destination_path: destination,
            destination: existing,
            size_only: size_only_enabled,
            ignore_times: ignore_times_enabled,
            checksum: checksum_enabled,
            checksum_algorithm: context.options().checksum_algorithm(),
            modify_window: context.options().modify_window(),
        });

        if skip {
            // sometimes we still need to re-verify
            let requires_content_verification = existing.is_file()
                && !checksum_enabled
                && (context.options().backup_enabled() || context.delete_timing().is_some());

            if requires_content_verification {
                skip = match files_checksum_match(
                    source,
                    destination,
                    context.options().checksum_algorithm(),
                ) {
                    Ok(result) => result,
                    Err(error) => {
                        return Err(LocalCopyError::io(
                            "compare existing destination",
                            destination.to_path_buf(),
                            error,
                        ));
                    }
                };
            }
        }

        if skip {
            apply_file_metadata_with_options(destination, metadata, &metadata_options)
                .map_err(crate::local_copy::map_metadata_error)?;
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
            context.record_hard_link(metadata, destination);
            context.summary_mut().record_regular_file_matched();
            let metadata_snapshot = LocalCopyMetadata::from_metadata(metadata, None);
            let total_bytes = Some(metadata_snapshot.len());
            let xattrs_enabled = {
                #[cfg(all(unix, feature = "xattr"))]
                {
                    preserve_xattrs
                }
                #[cfg(not(all(unix, feature = "xattr")))]
                {
                    false
                }
            };
            let acls_enabled = {
                #[cfg(all(unix, feature = "acl"))]
                {
                    preserve_acls
                }
                #[cfg(not(all(unix, feature = "acl")))]
                {
                    false
                }
            };
            let change_set = LocalCopyChangeSet::for_file(
                metadata,
                existing_metadata,
                &metadata_options,
                true,
                false,
                xattrs_enabled,
                acls_enabled,
            );
            context.record(
                LocalCopyRecord::new(
                    record_path.to_path_buf(),
                    LocalCopyAction::MetadataReused,
                    0,
                    total_bytes,
                    Duration::default(),
                    Some(metadata_snapshot),
                )
                .with_change_set(change_set),
            );
            return Ok(());
        }
    }

    // we are going to overwrite / rewrite — back up if needed
    if let Some(existing) = existing_metadata {
        context.backup_existing_entry(destination, relative, existing.file_type())?;
    }

    // non-regular files get the small-path
    if !file_type.is_file() {
        return copy_special_as_regular_file(
            context,
            source,
            destination,
            metadata,
            metadata_options,
            record_path,
            existing_metadata,
            destination_previously_existed,
            file_type,
            relative,
            mode,
            #[cfg(all(unix, feature = "xattr"))]
            preserve_xattrs,
            #[cfg(all(unix, feature = "acl"))]
            preserve_acls,
        );
    }

    // regular file copy
    let mut reader = open_source_file(source, context.open_noatime_enabled())
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

    // delta signature if we can
    let delta_signature = if append_offset == 0 && !whole_file_enabled && !inplace_enabled {
        match existing_metadata {
            Some(existing) if existing.is_file() => {
                build_delta_signature(destination, existing, context.block_size_override())?
            }
            _ => None,
        }
    } else {
        None
    };

    // re-open in case we’re copying from a reference path
    let copy_source = copy_source_override.as_deref().unwrap_or(source);
    let mut reader = open_source_file(copy_source, context.open_noatime_enabled())
        .map_err(|error| LocalCopyError::io("copy file", copy_source.to_path_buf(), error))?;
    if append_offset > 0 {
        reader
            .seek(SeekFrom::Start(append_offset))
            .map_err(|error| LocalCopyError::io("copy file", copy_source.to_path_buf(), error))?;
    }

    // choose write strategy
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

    if copy_result.is_ok() && context.fsync_enabled() {
        sync_destination_file(&mut writer, preallocate_target)?;
    }

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

    // record created path
    context.register_created_path(
        destination,
        CreatedEntryKind::File,
        destination_previously_existed,
    );

    // track as potential hard-link source
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
    let xattrs_enabled = {
        #[cfg(all(unix, feature = "xattr"))]
        {
            preserve_xattrs
        }
        #[cfg(not(all(unix, feature = "xattr")))]
        {
            false
        }
    };
    let acls_enabled = {
        #[cfg(all(unix, feature = "acl"))]
        {
            preserve_acls
        }
        #[cfg(not(all(unix, feature = "acl")))]
        {
            false
        }
    };
    let wrote_data = outcome.literal_bytes() > 0 || append_offset > 0;
    let change_set = LocalCopyChangeSet::for_file(
        metadata,
        existing_metadata,
        &metadata_options,
        destination_previously_existed,
        wrote_data,
        xattrs_enabled,
        acls_enabled,
    );
    context.record(
        LocalCopyRecord::new(
            record_path.to_path_buf(),
            LocalCopyAction::DataCopied,
            outcome.literal_bytes(),
            total_bytes,
            elapsed,
            Some(metadata_snapshot),
        )
        .with_change_set(change_set)
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
                #[cfg(all(unix, feature = "xattr"))]
                preserve_xattrs,
                #[cfg(all(unix, feature = "acl"))]
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
                    #[cfg(all(unix, feature = "xattr"))]
                    preserve_xattrs,
                    #[cfg(all(unix, feature = "acl"))]
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
                #[cfg(all(unix, feature = "xattr"))]
                preserve_xattrs,
                #[cfg(all(unix, feature = "acl"))]
                preserve_acls,
            ),
        )?;
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn copy_special_as_regular_file(
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
    #[cfg(all(unix, feature = "xattr"))] preserve_xattrs: bool,
    #[cfg(all(unix, feature = "acl"))] preserve_acls: bool,
) -> Result<(), LocalCopyError> {
    #[cfg(not(all(unix, any(feature = "xattr", feature = "acl"))))]
    let _ = mode;

    let start = Instant::now();
    let partial_enabled = context.partial_enabled();
    let inplace_enabled = context.inplace_enabled();
    let delay_updates_enabled = context.delay_updates_enabled();
    let mut guard: Option<DestinationWriteGuard> = None;

    if inplace_enabled {
        let mut file = fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(destination)
            .map_err(|error| LocalCopyError::io("copy file", destination.to_path_buf(), error))?;
        if context.fsync_enabled() {
            sync_destination_file(&mut file, destination)?;
        }
    } else {
        let (new_guard, mut file) = DestinationWriteGuard::new(
            destination,
            partial_enabled,
            context.partial_directory_path(),
            context.temp_directory_path(),
        )?;
        if context.fsync_enabled() {
            let target = new_guard.staging_path();
            sync_destination_file(&mut file, target)?;
        }
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
            .map(|existing_guard| existing_guard.staging_path())
            .unwrap_or(destination)
    } else {
        destination
    };
    context.record_hard_link(metadata, hard_link_path);

    let elapsed = start.elapsed();
    context.summary_mut().record_file(metadata.len(), 0, None);
    context.summary_mut().record_elapsed(elapsed);
    let metadata_snapshot = LocalCopyMetadata::from_metadata(metadata, None);
    let total_bytes = Some(metadata_snapshot.len());
    let xattrs_enabled = {
        #[cfg(all(unix, feature = "xattr"))]
        {
            preserve_xattrs
        }
        #[cfg(not(all(unix, feature = "xattr")))]
        {
            false
        }
    };
    let acls_enabled = {
        #[cfg(all(unix, feature = "acl"))]
        {
            preserve_acls
        }
        #[cfg(not(all(unix, feature = "acl")))]
        {
            false
        }
    };
    let change_set = LocalCopyChangeSet::for_file(
        metadata,
        existing_metadata,
        &metadata_options,
        destination_previously_existed,
        false,
        xattrs_enabled,
        acls_enabled,
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

    let relative_for_removal = Some(record_path.to_path_buf());
    if let Some(existing_guard) = guard {
        if delay_updates_enabled {
            let destination_path = existing_guard.final_path().to_path_buf();
            let update = DeferredUpdate::new(
                existing_guard,
                metadata.clone(),
                metadata_options.clone(),
                mode,
                source.to_path_buf(),
                relative_for_removal.clone(),
                destination_path,
                file_type,
                destination_previously_existed,
                #[cfg(all(unix, feature = "xattr"))]
                preserve_xattrs,
                #[cfg(all(unix, feature = "acl"))]
                preserve_acls,
            );
            context.register_deferred_update(update);
        } else {
            let destination_path = existing_guard.final_path().to_path_buf();
            existing_guard.commit()?;
            context.apply_metadata_and_finalize(
                destination_path.as_path(),
                FinalizeMetadataParams::new(
                    metadata,
                    metadata_options,
                    mode,
                    source,
                    relative_for_removal.as_deref(),
                    file_type,
                    destination_previously_existed,
                    #[cfg(all(unix, feature = "xattr"))]
                    preserve_xattrs,
                    #[cfg(all(unix, feature = "acl"))]
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
                #[cfg(all(unix, feature = "xattr"))]
                preserve_xattrs,
                #[cfg(all(unix, feature = "acl"))]
                preserve_acls,
            ),
        )?;
    }

    Ok(())
}

fn sync_destination_file(writer: &mut fs::File, path: &Path) -> Result<(), LocalCopyError> {
    writer
        .sync_all()
        .map_err(|error| LocalCopyError::io("fsync destination file", path.to_path_buf(), error))?;
    record_fsync_call();
    Ok(())
}

#[cfg(test)]
fn record_fsync_call() {
    FSYNC_CALL_COUNT.fetch_add(1, Ordering::Relaxed);
}

#[cfg(not(test))]
fn record_fsync_call() {}

#[cfg(test)]
#[allow(dead_code)]
pub(crate) fn take_fsync_call_count() -> usize {
    FSYNC_CALL_COUNT.swap(0, Ordering::Relaxed)
}

fn open_source_file(path: &Path, use_noatime: bool) -> io::Result<fs::File> {
    if use_noatime && let Some(file) = try_open_noatime(path)? {
        return Ok(file);
    }
    fs::File::open(path)
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn try_open_noatime(path: &Path) -> io::Result<Option<fs::File>> {
    let mut options = fs::OpenOptions::new();
    options.read(true).custom_flags(libc::O_NOATIME);
    match options.open(path) {
        Ok(file) => Ok(Some(file)),
        Err(error) => match error.raw_os_error() {
            Some(EPERM | EACCES | EINVAL | ENOTSUP | EROFS) => Ok(None),
            _ => Err(error),
        },
    }
}

#[cfg(not(any(target_os = "linux", target_os = "android")))]
fn try_open_noatime(_path: &Path) -> io::Result<Option<fs::File>> {
    Ok(None)
}
