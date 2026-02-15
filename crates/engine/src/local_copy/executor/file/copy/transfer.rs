use std::fs;
use std::io::{self, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use logging::debug_log;

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

use ::metadata::MetadataOptions;

use super::super::append::{AppendMode, determine_append_mode};
use super::super::comparison::{
    CopyComparison, build_delta_signature, files_checksum_match, should_skip_copy,
};
use super::super::guard::{DestinationWriteGuard, DirectWriteGuard, remove_incomplete_destination};
use super::super::preallocate::maybe_preallocate_destination;

/// Boolean flags controlling file transfer behavior.
///
/// This struct groups the boolean parameters that were previously passed
/// individually to `execute_transfer`, reducing parameter count and
/// improving code clarity.
#[derive(Clone, Copy, Debug)]
pub(super) struct TransferFlags {
    /// Whether append mode is allowed for existing files.
    pub append_allowed: bool,
    /// Whether to verify appended data matches the source.
    pub append_verify: bool,
    /// Whether to always transfer the entire file (no delta).
    pub whole_file_enabled: bool,
    /// Whether to update the file in place (no temp file).
    pub inplace_enabled: bool,
    /// Whether to keep partial transfers on interruption.
    pub partial_enabled: bool,
    /// Whether to use sparse writes for zero-filled regions.
    pub use_sparse_writes: bool,
    /// Whether to compress data during transfer.
    pub compress_enabled: bool,
    /// Whether to compare files by size only.
    pub size_only_enabled: bool,
    /// Whether to ignore modification times when comparing.
    pub ignore_times_enabled: bool,
    /// Whether to use checksums for comparison.
    pub checksum_enabled: bool,
    /// Whether to preserve extended attributes (Unix only).
    #[cfg(all(unix, feature = "xattr"))]
    pub preserve_xattrs: bool,
    /// Whether to preserve ACLs (Unix only).
    #[cfg(all(unix, feature = "acl"))]
    pub preserve_acls: bool,
}

impl TransferFlags {
    /// Returns whether xattrs preservation is effectively enabled,
    /// accounting for compile-time feature flags.
    #[inline]
    pub(super) const fn xattrs_enabled(self) -> bool {
        #[cfg(all(unix, feature = "xattr"))]
        {
            self.preserve_xattrs
        }
        #[cfg(not(all(unix, feature = "xattr")))]
        {
            false
        }
    }

    /// Returns whether ACL preservation is effectively enabled,
    /// accounting for compile-time feature flags.
    #[inline]
    pub(super) const fn acls_enabled(self) -> bool {
        #[cfg(all(unix, feature = "acl"))]
        {
            self.preserve_acls
        }
        #[cfg(not(all(unix, feature = "acl")))]
        {
            false
        }
    }
}

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
    flags: TransferFlags,
    mode: LocalCopyExecution,
    copy_source_override: Option<PathBuf>,
) -> Result<(), LocalCopyError> {
    // keep the param used on non-unix builds to avoid warnings
    #[cfg(not(all(unix, any(feature = "xattr", feature = "acl"))))]
    let _ = mode;

    // Unpack flags for easier access in the function body
    let TransferFlags {
        append_allowed,
        append_verify,
        whole_file_enabled,
        inplace_enabled,
        partial_enabled,
        use_sparse_writes,
        compress_enabled,
        size_only_enabled,
        ignore_times_enabled,
        checksum_enabled,
        #[cfg(all(unix, feature = "xattr"))]
        preserve_xattrs,
        #[cfg(all(unix, feature = "acl"))]
        preserve_acls,
    } = flags;

    let file_size = metadata.len();

    // fast-path: see if destination is already in-sync
    if let Some(existing) = existing_metadata {
        // Look up prefetched checksum result from parallel checksum cache
        let prefetched_match = if checksum_enabled {
            context.lookup_checksum(source)
        } else {
            None
        };

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
            prefetched_match,
        });

        if skip {
            // sometimes we still need to re-verify
            let requires_content_verification =
                existing.is_file() && !checksum_enabled && context.options().backup_enabled();

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
            debug_log!(
                Deltasum,
                2,
                "skipping {}: already up-to-date",
                record_path.display()
            );
            ::metadata::apply_file_metadata_if_changed(
                destination,
                metadata,
                existing,
                &metadata_options,
            )
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
            let change_set = LocalCopyChangeSet::for_file(
                metadata,
                existing_metadata,
                &metadata_options,
                true,
                false,
                flags.xattrs_enabled(),
                flags.acls_enabled(),
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
            flags,
        );
    }

    // regular file copy
    let mut reader = open_source_file(source, context.open_noatime_enabled())
        .map_err(|error| LocalCopyError::io("copy file", source, error))?;
    let append_mode = determine_append_mode(
        append_allowed,
        append_verify,
        &mut reader,
        source,
        destination,
        existing_metadata,
        file_size,
    )?;
    if matches!(append_mode, AppendMode::Skip) {
        // Upstream rsync skips the file when dest >= source size in append mode.
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
    let append_offset = match append_mode {
        AppendMode::Append(offset) => {
            debug_log!(
                Send,
                2,
                "appending to {}: resuming at offset {}",
                record_path.display(),
                offset
            );
            offset
        }
        AppendMode::Disabled | AppendMode::Skip => 0,
    };
    reader
        .seek(SeekFrom::Start(append_offset))
        .map_err(|error| LocalCopyError::io("copy file", source, error))?;

    // delta signature if we can
    // For inplace mode, we can use delta transfer as long as we're careful about reading
    // from the destination file before writing to avoid clobbering data we haven't read yet.
    let delta_signature = if append_offset == 0 && !whole_file_enabled {
        match existing_metadata {
            Some(existing) if existing.is_file() => {
                build_delta_signature(destination, existing, context.block_size_override())?
            }
            _ => None,
        }
    } else {
        None
    };

    // Use existing reader or re-open if copying from a reference path
    let (mut reader, copy_source) = if let Some(ref override_path) = copy_source_override {
        // Reference path differs from source - need to open the override path
        let file = open_source_file(override_path, context.open_noatime_enabled())
            .map_err(|error| LocalCopyError::io("copy file", override_path.clone(), error))?;
        (file, override_path.as_path())
    } else {
        // Same source - reuse the existing reader (already at correct position)
        (reader, source)
    };
    // Seek to append offset if needed (for override path case, or to reset position)
    if copy_source_override.is_some() && append_offset > 0 {
        reader
            .seek(SeekFrom::Start(append_offset))
            .map_err(|error| LocalCopyError::io("copy file", copy_source, error))?;
    }

    // choose write strategy
    let delay_updates_enabled = context.delay_updates_enabled();
    let mut guard = None;
    let mut direct_guard: Option<DirectWriteGuard> = None;
    let mut staging_path: Option<PathBuf> = None;

    let mut writer = if append_offset > 0 {
        let mut file = fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(destination)
            .map_err(|error| LocalCopyError::io("copy file", destination, error))?;
        file.seek(SeekFrom::Start(append_offset))
            .map_err(|error| LocalCopyError::io("copy file", destination, error))?;
        file
    } else if inplace_enabled {
        // For inplace mode with delta transfer, we must NOT truncate the file
        // because we need to read existing blocks during delta reconstruction.
        // The file will be truncated to the final size after the copy completes.
        let should_truncate = delta_signature.is_none();
        fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(should_truncate)
            .open(destination)
            .map_err(|error| LocalCopyError::io("copy file", destination, error))?
    } else if existing_metadata.is_none()
        && !partial_enabled
        && !delay_updates_enabled
        && context.temp_directory_path().is_none()
    {
        // Direct write for new files: skip temp file + rename overhead.
        // Safe because there's no existing file to protect with atomicity.
        // DirectWriteGuard ensures cleanup on error, panic, or signal.
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(destination)
        {
            Ok(file) => {
                direct_guard = Some(DirectWriteGuard::new(destination.to_path_buf()));
                file
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                // Race: file appeared between stat and open. Fall back to guard.
                let (new_guard, file) = DestinationWriteGuard::new(
                    destination,
                    partial_enabled,
                    context.partial_directory_path(),
                    context.temp_directory_path(),
                )?;
                staging_path = Some(new_guard.staging_path().to_path_buf());
                guard = Some(new_guard);
                file
            }
            Err(error) => {
                return Err(LocalCopyError::io("copy file", destination, error));
            }
        }
    } else {
        let (new_guard, file) = DestinationWriteGuard::new(
            destination,
            partial_enabled,
            context.partial_directory_path(),
            context.temp_directory_path(),
        )?;
        staging_path = Some(new_guard.staging_path().to_path_buf());
        debug_log!(
            Io,
            3,
            "created temp file {} for {}",
            new_guard.staging_path().display(),
            record_path.display()
        );
        guard = Some(new_guard);
        file
    };

    let preallocate_target = guard
        .as_ref()
        .map_or(destination, |existing_guard| existing_guard.staging_path());
    maybe_preallocate_destination(
        &mut writer,
        preallocate_target,
        file_size,
        append_offset,
        context.preallocate_enabled(),
    )?;

    // Runtime selection: buffer pool path vs direct Vec allocation.
    // Both paths are always compiled; the context flag controls which is used.
    let mut pool_guard = if context.use_buffer_pool() {
        Some(
            super::super::super::super::BufferPool::acquire_adaptive_from(
                context.buffer_pool(),
                file_size,
            ),
        )
    } else {
        None
    };
    let adaptive_size = super::super::super::super::adaptive_buffer_size(file_size);
    let mut direct_buffer = if pool_guard.is_none() {
        vec![0u8; adaptive_size]
    } else {
        Vec::new()
    };
    let buffer: &mut [u8] = if let Some(ref mut guard) = pool_guard {
        guard.as_mut_slice()
    } else {
        &mut direct_buffer
    };

    let start = Instant::now();
    debug_log!(
        Send,
        1,
        "sending {}: {} bytes{}",
        record_path.display(),
        file_size,
        if delta_signature.is_some() {
            " (delta)"
        } else {
            ""
        }
    );

    let copy_result = context.copy_file_contents(
        &mut reader,
        &mut writer,
        buffer,
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

    // DeferredSync handles fsync batching at runtime (registered in apply_metadata_and_finalize).
    // No immediate sync needed here.
    //
    // On Linux, keep `writer` alive — the fd will be used for fd-based metadata
    // operations (fchmod/fchown/futimens) to avoid redundant path lookups.
    // On macOS/APFS, fd-based metadata shifts cost to close(), so we drop early
    // and use path-based operations instead.
    #[cfg(target_os = "linux")]
    let mut writer_for_metadata: Option<fs::File> = Some(writer);
    #[cfg(not(target_os = "linux"))]
    let mut writer_for_metadata: Option<fs::File> = {
        drop(writer);
        None
    };

    let staging_path_for_links = guard
        .as_ref()
        .map(|existing_guard| existing_guard.staging_path().to_path_buf())
        .or_else(|| staging_path.take());

    let outcome = match copy_result {
        Ok(outcome) => {
            if let Err(timeout_error) = context.enforce_timeout() {
                drop(writer_for_metadata.take());
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
            drop(writer_for_metadata.take());
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
    debug_log!(
        Deltasum,
        2,
        "transferred {}: {} literal bytes in {:.3}s",
        record_path.display(),
        outcome.literal_bytes(),
        elapsed.as_secs_f64()
    );
    let compressed_bytes = outcome.compressed_bytes();
    context
        .summary_mut()
        .record_file(file_size, outcome.literal_bytes(), compressed_bytes);
    context.summary_mut().record_elapsed(elapsed);

    let metadata_snapshot = LocalCopyMetadata::from_metadata(metadata, None);
    let total_bytes = Some(metadata_snapshot.len());
    let wrote_data = outcome.literal_bytes() > 0 || append_offset > 0;
    let change_set = LocalCopyChangeSet::for_file(
        metadata,
        existing_metadata,
        &metadata_options,
        destination_previously_existed,
        wrote_data,
        flags.xattrs_enabled(),
        flags.acls_enabled(),
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
        drop(writer_for_metadata.take());
        if existing_metadata.is_none() {
            remove_incomplete_destination(destination);
        }

        return Err(timeout_error);
    }

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
        &mut writer_for_metadata,
        #[cfg(all(unix, feature = "xattr"))]
        preserve_xattrs,
        #[cfg(all(unix, feature = "acl"))]
        preserve_acls,
    )?;

    // Direct write completed successfully — prevent cleanup on drop.
    if let Some(ref mut dg) = direct_guard {
        dg.commit();
    }

    Ok(())
}

/// Commits or defers a destination guard, applying metadata as appropriate.
///
/// When `delay_updates_enabled` is true, the guard is registered for deferred
/// commit. Otherwise the guard is committed immediately and metadata is applied.
/// If no guard is present (direct-write or inplace path), metadata is applied
/// directly to `destination`.
#[allow(clippy::too_many_arguments)]
fn finalize_guard_and_metadata(
    context: &mut CopyContext,
    guard: Option<DestinationWriteGuard>,
    destination: &Path,
    metadata: &fs::Metadata,
    metadata_options: MetadataOptions,
    mode: LocalCopyExecution,
    source: &Path,
    record_path: &Path,
    relative: Option<&Path>,
    file_type: fs::FileType,
    destination_previously_existed: bool,
    delay_updates_enabled: bool,
    writer_for_metadata: &mut Option<fs::File>,
    #[cfg(all(unix, feature = "xattr"))] preserve_xattrs: bool,
    #[cfg(all(unix, feature = "acl"))] preserve_acls: bool,
) -> Result<(), LocalCopyError> {
    let relative_for_removal = Some(record_path.to_path_buf());
    if let Some(guard) = guard {
        if delay_updates_enabled {
            drop(writer_for_metadata.take());
            let destination_path = guard.final_path().to_path_buf();
            let update = DeferredUpdate::new(
                guard,
                metadata.clone(),
                metadata_options,
                mode,
                source.to_path_buf(),
                relative_for_removal,
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
            debug_log!(
                Io,
                3,
                "renaming temp file to {}",
                destination_path.display()
            );
            guard.commit()?;
            #[allow(unused_mut)] // mut needed on unix for with_fd()
            let mut params = FinalizeMetadataParams::new(
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
            );
            #[cfg(unix)]
            if let Some(ref w) = *writer_for_metadata {
                use std::os::fd::AsFd;
                params = params.with_fd(w.as_fd());
            }
            context.apply_metadata_and_finalize(destination_path.as_path(), params)?;
            drop(writer_for_metadata.take());
        }
    } else {
        #[allow(unused_mut)] // mut needed on unix for with_fd()
        let mut params = FinalizeMetadataParams::new(
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
        );
        #[cfg(unix)]
        if let Some(ref w) = *writer_for_metadata {
            use std::os::fd::AsFd;
            params = params.with_fd(w.as_fd());
        }
        context.apply_metadata_and_finalize(destination, params)?;
        drop(writer_for_metadata.take());
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
        // DeferredSync handles fsync batching at runtime (registered in apply_metadata_and_finalize).
    } else {
        let (new_guard, _file) = DestinationWriteGuard::new(
            destination,
            partial_enabled,
            context.partial_directory_path(),
            context.temp_directory_path(),
        )?;
        // DeferredSync handles fsync batching at runtime (registered in apply_metadata_and_finalize).
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
        #[cfg(all(unix, feature = "acl"))]
        flags.preserve_acls,
    )?;

    Ok(())
}

#[cfg(test)]
#[allow(dead_code)]
fn sync_destination_file(writer: &mut fs::File, path: &Path) -> Result<(), LocalCopyError> {
    writer
        .sync_all()
        .map_err(|error| LocalCopyError::io("fsync destination file", path, error))?;
    record_fsync_call();
    Ok(())
}

#[cfg(test)]
#[allow(dead_code)]
fn record_fsync_call() {
    FSYNC_CALL_COUNT.fetch_add(1, Ordering::Relaxed);
}

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
