//! Main file transfer orchestration.
//!
//! This module contains `execute_transfer`, the central function that drives
//! a single file copy. It handles the full pipeline: skip detection, backup,
//! append-mode resume, delta signature, writer strategy selection (direct,
//! inplace, temp-file), buffer allocation, data copy, and post-transfer
//! bookkeeping.

use std::fs;
use std::io::{Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use logging::debug_log;

use ::metadata::MetadataOptions;

#[cfg(all(unix, feature = "acl"))]
use crate::local_copy::sync_acls_if_requested;
#[cfg(all(unix, feature = "xattr"))]
use crate::local_copy::sync_xattrs_if_requested;

use crate::local_copy::{
    CopyContext, CreatedEntryKind, LocalCopyAction, LocalCopyChangeSet, LocalCopyError,
    LocalCopyExecution, LocalCopyMetadata, LocalCopyRecord,
};

use super::super::super::append::{AppendMode, determine_append_mode};
use super::super::super::comparison::{
    CopyComparison, build_delta_signature, files_checksum_match, should_skip_copy,
};
use super::super::super::guard::remove_incomplete_destination;
use super::super::super::preallocate::maybe_preallocate_destination;
use super::TransferFlags;
use super::finalize::finalize_guard_and_metadata;
use super::open::open_source_file;
use super::write_strategy::{open_destination_writer, select_write_strategy};

/// Executes the data transfer for a single regular file.
///
/// This is the core transfer function that handles all write strategies
/// (append, inplace, temp-file, direct-write) and integrates delta transfer
/// when a usable basis file exists at the destination. The caller is
/// responsible for pre-checks (dry-run, size filters, link processing).
#[allow(clippy::too_many_arguments)]
pub(in crate::local_copy) fn execute_transfer(
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
    // Suppress unused-variable warning on platforms without xattr/acl features
    #[cfg(not(all(unix, any(feature = "xattr", feature = "acl"))))]
    let _ = mode;

    let TransferFlags {
        append_allowed,
        append_verify,
        whole_file_enabled,
        inplace_enabled,
        partial_enabled,
        use_sparse_writes,
        compress_enabled,
        size_only_enabled: _,
        ignore_times_enabled: _,
        checksum_enabled: _,
        #[cfg(all(unix, feature = "xattr"))]
        preserve_xattrs,
        #[cfg(all(unix, feature = "acl"))]
        preserve_acls,
    } = flags;

    let file_size = metadata.len();

    if let Some(existing) = existing_metadata {
        if try_skip_up_to_date(
            context,
            source,
            destination,
            metadata,
            &metadata_options,
            record_path,
            existing,
            &flags,
            mode,
        )? {
            return Ok(());
        }
    }

    if let Some(existing) = existing_metadata {
        context.backup_existing_entry(destination, relative, existing.file_type())?;
    }

    if !file_type.is_file() {
        return super::special::copy_special_as_regular_file(
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

    // Fast path: macOS clonefile for new whole-file copies.
    // clonefile() creates a CoW clone on APFS, avoiding all read/write I/O.
    // Because clonefile copies source metadata and extended attributes verbatim,
    // it is only safe when all metadata will either be preserved by finalize or
    // corrected by normalize_cloned_metadata. The eligibility check below
    // centralizes all conditions that would make clonefile produce incorrect
    // results, so finalize_guard_and_metadata works identically for both paths.
    #[cfg(target_os = "macos")]
    let clonefile_eligible = {
        // Transfer mode: new file, whole-file, no conflicting options
        let transfer_ok = existing_metadata.is_none()
            && whole_file_enabled
            && !inplace_enabled
            && !partial_enabled
            && !use_sparse_writes
            && !compress_enabled
            && copy_source_override.is_none()
            && !context.has_bandwidth_limiter()
            && !context.delay_updates_enabled()
            && context.temp_directory_path().is_none();

        // Extended attributes: clonefile copies all xattrs verbatim, so skip
        // when (a) xattr filters need selective copy, or (b) xattrs are disabled
        // entirely (source xattrs would leak to destination).
        let xattr_ok = {
            #[cfg(all(unix, feature = "xattr"))]
            {
                let has_filter_rules = context
                    .filter_program()
                    .is_some_and(|p| p.has_xattr_rules());
                flags.xattrs_enabled() && !has_filter_rules
            }
            #[cfg(not(all(unix, feature = "xattr")))]
            {
                true
            }
        };

        transfer_ok && xattr_ok
    };
    #[cfg(target_os = "macos")]
    if clonefile_eligible {
        // Dispatch through the configured PlatformCopy. Only commit to the
        // fast path when the strategy reported a true zero-copy reflink
        // (clonefile/FICLONE/ReFS reflink); any data-copy fallback would
        // bypass rsync's delta machinery without honouring the eligibility
        // assumptions, so on non-zero-copy results we discard and fall
        // through to the normal copy path below.
        let cloned =
            match context
                .options()
                .platform_copy()
                .copy_file(source, destination, file_size)
            {
                Ok(result) if result.is_zero_copy() => true,
                Ok(_) => {
                    let _ = std::fs::remove_file(destination);
                    false
                }
                Err(_) => {
                    let _ = std::fs::remove_file(destination);
                    false
                }
            };
        if cloned {
            let start = Instant::now();
            debug_log!(
                Send,
                1,
                "cloned {}: {} bytes (CoW)",
                record_path.display(),
                file_size
            );

            // Batch capture for whole-file clones
            context.capture_batch_whole_file(source, file_size)?;
            context.finalize_batch_file_delta(source)?;

            context.register_created_path(
                destination,
                CreatedEntryKind::File,
                destination_previously_existed,
            );
            context.record_hard_link(metadata, destination);
            context
                .summary_mut()
                .record_file(file_size, file_size, None);
            context.summary_mut().record_elapsed(start.elapsed());

            let metadata_snapshot = LocalCopyMetadata::from_metadata(metadata, None);
            let total_bytes = Some(metadata_snapshot.len());
            let change_set = LocalCopyChangeSet::for_file(
                metadata,
                existing_metadata,
                &metadata_options,
                destination_previously_existed,
                true,
                flags.xattrs_enabled(),
                flags.acls_enabled(),
            );
            context.record(
                LocalCopyRecord::new(
                    record_path.to_path_buf(),
                    LocalCopyAction::DataCopied,
                    file_size,
                    total_bytes,
                    start.elapsed(),
                    Some(metadata_snapshot),
                )
                .with_change_set(change_set)
                .with_creation(true),
            );

            // Normalize cloned metadata to match what open()-created files have.
            // clonefile() preserves source metadata verbatim. Without this,
            // finalize_guard_and_metadata skips corrections when preservation is
            // disabled (e.g. --no-perms, --no-times), leaving source metadata
            // instead of umask/current-time defaults.
            // upstream: rsync creates files via open() then applies metadata -
            // clonefile must produce identical results.
            normalize_cloned_metadata(destination, metadata, &metadata_options)?;

            finalize_guard_and_metadata(
                context,
                None,
                destination,
                metadata,
                metadata_options,
                mode,
                source,
                record_path,
                relative,
                file_type,
                destination_previously_existed,
                false,
                &mut None,
                #[cfg(all(unix, feature = "xattr"))]
                preserve_xattrs,
                #[cfg(all(unix, feature = "acl"))]
                preserve_acls,
            )?;

            return Ok(());
        }
        // clonefile failed (cross-device, non-APFS, etc.) - fall through to normal copy
    }

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

    // upstream: receiver.c - skip when dest >= source size in append mode
    if matches!(append_mode, AppendMode::Skip) {
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
    if append_offset > 0 {
        reader
            .seek(SeekFrom::Start(append_offset))
            .map_err(|error| LocalCopyError::io("copy file", source, error))?;
    }

    // Build delta signature when a basis file exists and we are not appending.
    // For inplace mode, delta transfer reads existing blocks before overwriting.
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

    let (mut reader, copy_source) = if let Some(ref override_path) = copy_source_override {
        let file = open_source_file(override_path, context.open_noatime_enabled())
            .map_err(|error| LocalCopyError::io("copy file", override_path.clone(), error))?;
        (file, override_path.as_path())
    } else {
        (reader, source)
    };
    if copy_source_override.is_some() && append_offset > 0 {
        reader
            .seek(SeekFrom::Start(append_offset))
            .map_err(|error| LocalCopyError::io("copy file", copy_source, error))?;
    }

    let delay_updates_enabled = context.delay_updates_enabled();
    let mut guard = None;
    let mut staging_path: Option<PathBuf> = None;

    let strategy = select_write_strategy(
        append_offset,
        inplace_enabled,
        partial_enabled,
        delay_updates_enabled,
        existing_metadata.is_some(),
        context.temp_directory_path().is_some(),
        destination,
    );

    let mut writer = open_destination_writer(
        context,
        destination,
        record_path,
        &delta_signature,
        append_offset,
        partial_enabled,
        strategy,
        &mut guard,
        &mut staging_path,
    )?;

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

    let mut pool_guard = if context.use_buffer_pool() {
        Some(
            super::super::super::super::super::BufferPool::acquire_adaptive_from(
                context.buffer_pool(),
                file_size,
            ),
        )
    } else {
        None
    };
    let adaptive_size = super::super::super::super::super::adaptive_buffer_size(file_size);
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

    // On Linux, keep writer alive for fd-based metadata (fchmod/fchown/futimens).
    // On macOS/APFS, fd-based metadata shifts cost to close(), so drop early.
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

            // Batch capture: for whole-file transfers (no delta), capture the
            // entire file content as token literals. Delta transfers already
            // capture ops inline in flush_literal_chunk/copy_matched_block.
            // upstream: match.c:match_sums() - whole-file path writes literals.
            if delta_signature.is_none() {
                context.capture_batch_whole_file(source, file_size)?;
            }

            // Write the token end marker to the batch file for this file.
            // upstream: token.c:simple_send_token() with token=-1 writes 0.
            context.finalize_batch_file_delta(source)?;

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
    debug_log!(
        Deltasum,
        2,
        "transferred {}: {} literal bytes in {:.3}s",
        record_path.display(),
        outcome.literal_bytes(),
        elapsed.as_secs_f64()
    );

    // Record throughput sample for EMA-based dynamic buffer sizing.
    if context.use_buffer_pool() {
        let pool = context.buffer_pool();
        pool.record_transfer(outcome.literal_bytes() as usize, elapsed);
    }

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

    Ok(())
}

/// Checks if the destination is already up-to-date and can be skipped.
///
/// When the file is in-sync, applies metadata and records a `MetadataReused`
/// action. Returns `true` if the transfer should be skipped.
#[allow(clippy::too_many_arguments)]
fn try_skip_up_to_date(
    context: &mut CopyContext,
    source: &Path,
    destination: &Path,
    metadata: &fs::Metadata,
    metadata_options: &MetadataOptions,
    record_path: &Path,
    existing: &fs::Metadata,
    flags: &TransferFlags,
    #[allow(unused_variables)] // REASON: used on unix with feature "xattr"
    mode: LocalCopyExecution,
) -> Result<bool, LocalCopyError> {
    let prefetched_match = if flags.checksum_enabled {
        context.lookup_checksum(source)
    } else {
        None
    };

    let mut skip = should_skip_copy(CopyComparison {
        source_path: source,
        source: metadata,
        destination_path: destination,
        destination: existing,
        size_only: flags.size_only_enabled,
        ignore_times: flags.ignore_times_enabled,
        checksum: flags.checksum_enabled,
        checksum_algorithm: context.options().checksum_algorithm(),
        modify_window: context.options().modify_window(),
        prefetched_match,
    });

    if skip {
        let requires_content_verification =
            existing.is_file() && !flags.checksum_enabled && context.options().backup_enabled();

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

    if !skip {
        return Ok(false);
    }

    debug_log!(
        Deltasum,
        2,
        "skipping {}: already up-to-date",
        record_path.display()
    );
    ::metadata::apply_file_metadata_if_changed(destination, metadata, existing, metadata_options)
        .map_err(crate::local_copy::map_metadata_error)?;

    #[cfg(all(unix, feature = "xattr"))]
    sync_xattrs_if_requested(
        flags.preserve_xattrs,
        mode,
        source,
        destination,
        true,
        context.filter_program(),
    )?;
    #[cfg(all(unix, feature = "acl"))]
    sync_acls_if_requested(flags.preserve_acls, mode, source, destination, true)?;

    context.record_hard_link(metadata, destination);
    context.summary_mut().record_regular_file_matched();
    let metadata_snapshot = LocalCopyMetadata::from_metadata(metadata, None);
    let total_bytes = Some(metadata_snapshot.len());
    let change_set = LocalCopyChangeSet::for_file(
        metadata,
        Some(existing),
        metadata_options,
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

    Ok(true)
}

/// Normalizes a clonefile'd destination to match open()-created file defaults.
///
/// `clonefile()` preserves the source's exact metadata (permissions, mtime).
/// When the user has not requested preservation of these attributes (e.g.
/// `--no-perms`, `--no-times`), `finalize_guard_and_metadata` will skip
/// corrections because it assumes the file already has process-default metadata.
/// This function bridges that gap by resetting metadata to what `open()` would
/// produce, so the finalize step works identically for both paths.
///
/// - Permissions: reset to `source_mode & ~umask` (matching `open()` behavior)
/// - Timestamps: reset mtime to current time (matching newly created files)
#[cfg(target_os = "macos")]
fn normalize_cloned_metadata(
    destination: &Path,
    source_metadata: &fs::Metadata,
    options: &::metadata::MetadataOptions,
) -> Result<(), LocalCopyError> {
    use std::os::unix::fs::PermissionsExt;

    // When permissions are being preserved, finalize_guard_and_metadata will set
    // them from the source - clonefile already did this, so no correction needed.
    // When NOT preserving, reset to umask-applied mode (what open() would give).
    if !options.permissions() {
        // rustix provides a safe umask API (internally wraps the syscall).
        // Read current umask by setting a dummy value, then restore.
        let current_umask = rustix::process::umask(rustix::fs::Mode::empty());
        rustix::process::umask(current_umask);
        let umask_bits = u32::from(current_umask.bits());
        // Mask to 0o777 - open() never sets special bits (setuid/setgid/sticky).
        // upstream: rsync uses open(dest, O_CREAT, mode & 0777) for new files.
        let source_mode = source_metadata.permissions().mode() & 0o777;
        let default_mode = source_mode & !umask_bits;
        fs::set_permissions(destination, PermissionsExt::from_mode(default_mode))
            .map_err(|e| LocalCopyError::io("normalize cloned permissions", destination, e))?;
    }

    // When timestamps are being preserved, finalize will apply source mtime.
    // When NOT preserving, reset to current time (what a newly created file has).
    // Use utimensat via rustix to set mtime without needing write access -
    // clonefile may produce a read-only destination (e.g. source mode 0o444).
    if !options.times() {
        let now = rustix::fs::Timestamps {
            last_access: rustix::fs::Timespec {
                tv_sec: 0,
                tv_nsec: rustix::fs::UTIME_OMIT,
            },
            last_modification: rustix::fs::Timespec {
                tv_sec: 0,
                tv_nsec: rustix::fs::UTIME_NOW,
            },
        };
        rustix::fs::utimensat(
            rustix::fs::CWD,
            destination,
            &now,
            rustix::fs::AtFlags::empty(),
        )
        .map_err(|e| {
            LocalCopyError::io(
                "normalize cloned mtime",
                destination,
                std::io::Error::from_raw_os_error(e.raw_os_error()),
            )
        })?;
    }

    Ok(())
}
