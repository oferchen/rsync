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
use super::super::super::guard::{DestinationWriteGuard, remove_incomplete_destination};
use super::super::super::preallocate::maybe_preallocate_destination;
use super::TransferFlags;
use super::finalize::finalize_guard_and_metadata;
use super::open::open_source_file;

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

    // Fast-path: check if destination is already in-sync
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

    // Back up existing destination before overwriting
    if let Some(existing) = existing_metadata {
        context.backup_existing_entry(destination, relative, existing.file_type())?;
    }

    // Non-regular files use the special-file path
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

    // Open source and determine append mode
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

    // Re-open if copying from a reference path (e.g., --copy-dest)
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

    // Choose write strategy
    let delay_updates_enabled = context.delay_updates_enabled();
    let mut guard = None;
    let mut staging_path: Option<PathBuf> = None;

    let mut writer = open_destination_writer(
        context,
        destination,
        record_path,
        &delta_signature,
        append_offset,
        inplace_enabled,
        partial_enabled,
        delay_updates_enabled,
        existing_metadata,
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

    // Allocate transfer buffer - pool or direct
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
            context.finalize_batch_file_delta()?;

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

    // Record transfer results
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
    #[allow(unused_variables)] mode: LocalCopyExecution,
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

/// The write strategy for transferring a file to disk.
///
/// Mirrors upstream `receiver.c` logic which selects among four paths based on
/// transfer mode flags and destination state. The strategy is determined purely
/// from flags - no I/O - then executed by `open_destination_writer`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WriteStrategy {
    /// Open existing file and seek to append offset.
    Append,
    /// Write directly to destination without temp file.
    /// Truncates when no delta signature exists.
    Inplace,
    /// Create new file directly - no existing destination to protect.
    /// Uses `create_new(true)` to prevent races with concurrent writers.
    Direct,
    /// Create a staging temp file then rename atomically.
    /// Used when an existing destination must be protected, or when
    /// `--partial`, `--delay-updates`, or `--temp-dir` is active.
    TempFileRename,
}

/// Determines the write strategy from transfer flags and destination state.
///
/// This is a pure function with no I/O - it only inspects flags to decide
/// which strategy `open_destination_writer` should execute.
///
/// # Strategy selection (upstream: receiver.c)
///
/// 1. **Append** - `append_offset > 0`: resume writing at end of existing file.
/// 2. **Inplace** - `--inplace`: write directly, truncating only when no delta.
/// 3. **Direct** - no existing destination AND none of `--partial`,
///    `--delay-updates`, `--temp-dir`: create file directly.
/// 4. **TempFileRename** - all other cases: temp file + atomic rename.
fn select_write_strategy(
    append_offset: u64,
    inplace_enabled: bool,
    partial_enabled: bool,
    delay_updates_enabled: bool,
    has_existing_destination: bool,
    has_temp_directory: bool,
) -> WriteStrategy {
    if append_offset > 0 {
        WriteStrategy::Append
    } else if inplace_enabled {
        WriteStrategy::Inplace
    } else if !has_existing_destination
        && !partial_enabled
        && !delay_updates_enabled
        && !has_temp_directory
    {
        WriteStrategy::Direct
    } else {
        WriteStrategy::TempFileRename
    }
}

/// Opens the destination file using the appropriate write strategy.
///
/// Selects among four strategies based on transfer mode:
/// - **Append**: opens existing file and seeks to append offset
/// - **Inplace**: opens for writing without temp file (truncates only when no delta)
/// - **Direct write**: creates new file directly when no existing destination
/// - **Temp file**: creates a staging file via `DestinationWriteGuard`
#[allow(clippy::too_many_arguments)]
fn open_destination_writer(
    context: &CopyContext,
    destination: &Path,
    record_path: &Path,
    delta_signature: &Option<crate::delta::DeltaSignatureIndex>,
    append_offset: u64,
    inplace_enabled: bool,
    partial_enabled: bool,
    delay_updates_enabled: bool,
    existing_metadata: Option<&fs::Metadata>,
    guard: &mut Option<DestinationWriteGuard>,
    staging_path: &mut Option<PathBuf>,
) -> Result<fs::File, LocalCopyError> {
    let strategy = select_write_strategy(
        append_offset,
        inplace_enabled,
        partial_enabled,
        delay_updates_enabled,
        existing_metadata.is_some(),
        context.temp_directory_path().is_some(),
    );

    match strategy {
        WriteStrategy::Append => {
            let mut file = fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(false)
                .open(destination)
                .map_err(|error| LocalCopyError::io("copy file", destination, error))?;
            file.seek(SeekFrom::Start(append_offset))
                .map_err(|error| LocalCopyError::io("copy file", destination, error))?;
            Ok(file)
        }
        WriteStrategy::Inplace => {
            // For inplace with delta, do NOT truncate - we read existing blocks
            let should_truncate = delta_signature.is_none();
            fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(should_truncate)
                .open(destination)
                .map_err(|error| LocalCopyError::io("copy file", destination, error))
        }
        WriteStrategy::Direct => {
            // upstream: receiver.c - direct write when no existing file to protect.
            // create_new(true) prevents races with concurrent writers (EEXIST).
            debug_log!(
                Io,
                3,
                "direct write to {} (no existing destination)",
                record_path.display()
            );
            fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(destination)
                .map_err(|error| LocalCopyError::io("copy file", destination, error))
        }
        WriteStrategy::TempFileRename => {
            let (new_guard, file) = DestinationWriteGuard::new(
                destination,
                partial_enabled,
                context.partial_directory_path(),
                context.temp_directory_path(),
            )?;
            *staging_path = Some(new_guard.staging_path().to_path_buf());
            debug_log!(
                Io,
                3,
                "created temp file {} for {}",
                new_guard.staging_path().display(),
                record_path.display()
            );
            *guard = Some(new_guard);
            Ok(file)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- Append strategy ---

    #[test]
    fn append_offset_selects_append_strategy() {
        assert_eq!(
            select_write_strategy(1024, false, false, false, false, false),
            WriteStrategy::Append,
        );
    }

    #[test]
    fn append_offset_overrides_inplace() {
        assert_eq!(
            select_write_strategy(512, true, false, false, true, false),
            WriteStrategy::Append,
        );
    }

    #[test]
    fn append_offset_overrides_partial() {
        assert_eq!(
            select_write_strategy(256, false, true, false, false, false),
            WriteStrategy::Append,
        );
    }

    // --- Inplace strategy ---

    #[test]
    fn inplace_enabled_selects_inplace_strategy() {
        assert_eq!(
            select_write_strategy(0, true, false, false, true, false),
            WriteStrategy::Inplace,
        );
    }

    #[test]
    fn inplace_without_existing_dest_still_selects_inplace() {
        assert_eq!(
            select_write_strategy(0, true, false, false, false, false),
            WriteStrategy::Inplace,
        );
    }

    #[test]
    fn inplace_overrides_partial_and_delay_updates() {
        assert_eq!(
            select_write_strategy(0, true, true, true, true, true),
            WriteStrategy::Inplace,
        );
    }

    // --- Direct strategy ---

    #[test]
    fn no_existing_dest_selects_direct_strategy() {
        assert_eq!(
            select_write_strategy(0, false, false, false, false, false),
            WriteStrategy::Direct,
        );
    }

    // --- TempFileRename strategy ---

    #[test]
    fn partial_forces_temp_file_rename() {
        assert_eq!(
            select_write_strategy(0, false, true, false, false, false),
            WriteStrategy::TempFileRename,
        );
    }

    #[test]
    fn delay_updates_forces_temp_file_rename() {
        assert_eq!(
            select_write_strategy(0, false, false, true, false, false),
            WriteStrategy::TempFileRename,
        );
    }

    #[test]
    fn temp_dir_forces_temp_file_rename() {
        assert_eq!(
            select_write_strategy(0, false, false, false, false, true),
            WriteStrategy::TempFileRename,
        );
    }

    #[test]
    fn existing_dest_forces_temp_file_rename() {
        assert_eq!(
            select_write_strategy(0, false, false, false, true, false),
            WriteStrategy::TempFileRename,
        );
    }

    #[test]
    fn existing_dest_with_partial_forces_temp_file_rename() {
        assert_eq!(
            select_write_strategy(0, false, true, false, true, false),
            WriteStrategy::TempFileRename,
        );
    }

    #[test]
    fn all_temp_file_flags_active_selects_temp_file_rename() {
        assert_eq!(
            select_write_strategy(0, false, true, true, true, true),
            WriteStrategy::TempFileRename,
        );
    }

    // --- Priority ordering ---

    #[test]
    fn append_has_highest_priority() {
        // Even with inplace, partial, delay-updates, existing dest, and temp dir
        assert_eq!(
            select_write_strategy(100, true, true, true, true, true),
            WriteStrategy::Append,
        );
    }

    #[test]
    fn inplace_has_second_highest_priority() {
        // Even with partial, delay-updates, existing dest, and temp dir
        assert_eq!(
            select_write_strategy(0, true, true, true, true, true),
            WriteStrategy::Inplace,
        );
    }

    #[test]
    fn direct_requires_all_conditions_false() {
        // Any single flag prevents direct write
        assert_eq!(
            select_write_strategy(0, false, true, false, false, false),
            WriteStrategy::TempFileRename,
        );
        assert_eq!(
            select_write_strategy(0, false, false, true, false, false),
            WriteStrategy::TempFileRename,
        );
        assert_eq!(
            select_write_strategy(0, false, false, false, true, false),
            WriteStrategy::TempFileRename,
        );
        assert_eq!(
            select_write_strategy(0, false, false, false, false, true),
            WriteStrategy::TempFileRename,
        );
        // Only when all are false do we get Direct
        assert_eq!(
            select_write_strategy(0, false, false, false, false, false),
            WriteStrategy::Direct,
        );
    }
}
