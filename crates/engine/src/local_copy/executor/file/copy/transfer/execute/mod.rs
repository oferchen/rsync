//! Main file transfer orchestration.
//!
//! Hosts [`execute_transfer`], the central function that drives a single file
//! copy: skip detection, backup, append-mode resume, delta signature,
//! writer-strategy selection (direct, inplace, temp-file), buffer allocation,
//! data copy, and post-transfer bookkeeping.
//!
//! # Submodules
//!
//! - `skip` - quick-check skip detection and metadata-only reuse recording
//! - `clonefile` - macOS APFS clonefile fast path (whole-file CoW)
//! - `ficlone` - Linux FICLONE fast path (whole-file CoW on Btrfs/XFS/bcachefs)
//! - `iouring` - Linux io_uring registered-buffer data-write fast path
//! - `wincopy` - Windows `CopyFileExW` / ReFS reflink fast path

#[cfg(target_os = "macos")]
mod clonefile;
#[cfg(target_os = "linux")]
mod ficlone;
#[cfg(all(target_os = "linux", feature = "iouring-data-writes"))]
mod iouring;
mod skip;
#[cfg(target_os = "windows")]
mod wincopy;

use std::fs;
use std::io::{Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use logging::{debug_log, info_log};

use ::metadata::MetadataOptions;

use crate::fuzzy::{FuzzyMatcher, trace_fuzzy_basis_selected};
use crate::local_copy::{
    CopyContext, CopyMethodKind, CreatedEntryKind, LocalCopyAction, LocalCopyChangeSet,
    LocalCopyError, LocalCopyExecution, LocalCopyMetadata, LocalCopyRecord,
};

use super::super::super::append::{AppendMode, determine_append_mode};
use super::super::super::comparison::{
    Xxh64DedupOutcome, build_delta_signature, xxh64_dedup_check,
};
use super::super::super::compute_backup_path;
use super::super::super::guard::remove_incomplete_destination;
use super::super::super::preallocate::maybe_preallocate_destination;
use super::TransferFlags;
use super::finalize::finalize_guard_and_metadata;
use super::open::open_source_file;
use super::write_strategy::{open_destination_writer, select_write_strategy};

use skip::{record_metadata_only_skip, try_skip_up_to_date};

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
    // When `Some`, the copy reconstructs the file from a `--copy-dest` basis.
    // The transfer itemizes as a local change (`c`) compared against the basis
    // rather than a network transfer (`>`).
    // upstream: generator.c:1039 - itemize(..., ITEM_LOCAL_CHANGE, ...).
    reference_basis: Option<PathBuf>,
) -> Result<(), LocalCopyError> {
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
        // Consumed by the standard-path finalize call below on Unix; on
        // Windows the `CopyFileExW` fast path reconciles ADS itself, so this
        // field is unused in the standard path there.
        #[cfg(all(unix, feature = "xattr"))]
        preserve_xattrs,
        #[cfg(all(windows, feature = "xattr"))]
            preserve_xattrs: _,
        #[cfg(all(any(unix, windows), feature = "acl"))]
        preserve_acls,
    } = flags;

    // upstream: flist.c:1419-1424 - stream a `--copy-devices` device as a
    // regular file of its readable byte length instead of the zero stat size.
    let device_as_file_size = context.copy_device_as_file_size(source, metadata);
    let file_size = device_as_file_size.unwrap_or(metadata.len());

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

    // When the exact destination is absent and --fuzzy is active, look for a
    // similarly-named file in the destination directory to serve as the delta
    // basis. upstream: generator.c:1767-1795 - when `statret != 0 &&
    // fuzzy_basis`, `find_fuzzy()` selects the closest-named candidate and the
    // generator uses it as `fnamecmp` (the delta basis) instead of sending the
    // whole file.
    let fuzzy_basis = if !whole_file_enabled && existing_metadata.is_none() {
        // upstream: generator.c:858 compares against the source file's mtime
        // (whole seconds) for the fuzzy size/modtime fast-path.
        let target_mtime = metadata.modified().ok().map(system_time_to_unix_secs);
        find_fuzzy_basis(
            context,
            destination,
            relative,
            file_type,
            file_size,
            target_mtime,
        )
    } else {
        None
    };

    // Build delta signature BEFORE backup renames the destination away.
    // upstream: receiver.c - the basis file must be read while it still exists
    // at the destination path. If backup runs first, the rename causes ENOENT
    // which is_vanished_error() misclassifies as a source vanish (exit 24).
    let delta_signature = if !whole_file_enabled {
        match existing_metadata {
            Some(existing) if existing.is_file() => {
                build_delta_signature(destination, existing, context.block_size_override())?
            }
            _ => match fuzzy_basis {
                Some((ref path, ref meta)) => {
                    build_delta_signature(path, meta, context.block_size_override())?
                }
                None => None,
            },
        }
    } else {
        None
    };

    // Track where the old destination ended up after a potential backup rename.
    // When --backup renames the basis file away, the delta transfer must read
    // matched blocks from the backup location instead of the original destination.
    // upstream: receiver.c - the basis fd is opened before make_backup() runs;
    // here we track the new path because we cannot hold the fd across the
    // temp-file/inplace writer setup.
    //
    // A fuzzy basis is likewise a file separate from the (absent) destination:
    // the writer creates a fresh destination while matched blocks are read from
    // the fuzzy candidate, so seed the override with its path.
    let mut delta_basis_override: Option<PathBuf> =
        fuzzy_basis.as_ref().map(|(path, _)| path.clone());
    if let Some(existing) = existing_metadata {
        // upstream: generator.c:1862,1898 - under --inplace, --backup COPIES the
        // pre-image aside (preserving the destination inode for the in-place
        // rewrite) rather than renaming the destination away. Only a regular-file
        // inplace update qualifies; every other entry keeps the rename path.
        let inplace_regular_backup = inplace_enabled
            && existing.is_file()
            && context.options().backup_enabled()
            && !context.mode().is_dry_run();
        if inplace_regular_backup {
            context.backup_existing_entry_copy(destination)?;
            // The destination stays in place as the true in-place basis, so no
            // basis override: copy_file_contents rewrites the existing inode via
            // the proven inplace path (which truncates to the final length).
        } else {
            // upstream: rsync.c:740 - `make_backup(fname, False)` tries a
            // hard link into the backup area before renaming.
            context.backup_existing_entry(destination, relative, existing.file_type(), false)?;
            // When a rename moved the basis file, delta transfer must read
            // matched blocks from the backup location. The delta signature is
            // only built for regular files, so this condition is sufficient.
            // upstream: receiver.c:872-876 (FNAMECMP_BACKUP).
            if delta_signature.is_some()
                && context.options().backup_enabled()
                && !context.mode().is_dry_run()
            {
                delta_basis_override = Some(compute_backup_path(
                    context.destination_root(),
                    destination,
                    None,
                    context.options().backup_directory(),
                    context.options().backup_suffix(),
                ));
            }
        }
    }

    // A non-regular source that is NOT a `--copy-devices` device (a FIFO,
    // socket, or a device without `--copy-devices`) is materialised as an empty
    // placeholder. A `--copy-devices` device (`device_as_file_size.is_some()`)
    // instead falls through to the generic read/write loop below, which streams
    // its `file_size` bytes just like a regular file (upstream sender.c:410-418).
    if !file_type.is_file() && device_as_file_size.is_none() {
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

    // Fast path: macOS clonefile for new whole-file copies. Skipped for
    // `--copy-devices` devices, whose contents must be read/streamed (clonefile
    // clones extents by stat length, which is 0 for a device).
    #[cfg(target_os = "macos")]
    if device_as_file_size.is_none()
        && clonefile::eligible(
            context,
            existing_metadata,
            flags,
            copy_source_override.is_some(),
        )
        && clonefile::try_clone(
            context,
            source,
            destination,
            metadata,
            metadata_options.clone(),
            record_path,
            existing_metadata,
            destination_previously_existed,
            file_type,
            relative,
            mode,
            flags,
        )?
    {
        return Ok(());
    }

    // Fast path: Windows CopyFileExW / ReFS reflink for new whole-file copies.
    // Without this branch the executor falls into the generic read/write loop
    // which on Windows degenerates into a synchronous 256 KiB ReadFile/WriteFile
    // copy. The dispatcher hands large files COPY_FILE_NO_BUFFERING and
    // attempts FSCTL_DUPLICATE_EXTENTS_TO_FILE on ReFS volumes first.
    #[cfg(target_os = "windows")]
    if device_as_file_size.is_none()
        && wincopy::eligible(
            context,
            existing_metadata,
            flags,
            copy_source_override.is_some(),
        )
        && wincopy::try_copy(
            context,
            source,
            destination,
            metadata,
            metadata_options.clone(),
            record_path,
            existing_metadata,
            destination_previously_existed,
            file_type,
            relative,
            mode,
            flags,
        )?
    {
        return Ok(());
    }

    // Fast path: Linux FICLONE reflink for new whole-file copies on Btrfs,
    // XFS (reflink enabled), and bcachefs. Cross-filesystem / unsupported-fs
    // failures degrade to the generic read/write loop transparently.
    #[cfg(target_os = "linux")]
    if device_as_file_size.is_none()
        && ficlone::eligible(
            context,
            existing_metadata,
            flags,
            copy_source_override.is_some(),
        )
        && ficlone::try_clone(
            context,
            source,
            destination,
            metadata,
            metadata_options.clone(),
            record_path,
            existing_metadata,
            destination_previously_existed,
            file_type,
            relative,
            mode,
            flags,
        )?
    {
        return Ok(());
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
        let metadata_snapshot = LocalCopyMetadata::from_metadata(metadata, None)
            .virtualize_fake_super(source, metadata_options.fake_super_enabled());
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

    // Internal-only xxh64 file-dedup heuristic. Runs only when explicitly
    // opted in via `enable_xxh64_dedup`. When source and destination produce
    // identical xxh64 digests, treat the transfer as a metadata-only sync.
    // The heuristic is local-only and never affects the wire protocol.
    if append_offset == 0 && context.xxh64_dedup_enabled() && copy_source_override.is_none() {
        if let Some(existing) = existing_metadata {
            if existing.is_file() && metadata.is_file() {
                let outcome = xxh64_dedup_check(
                    source,
                    destination,
                    file_size,
                    existing.len(),
                    context.xxh64_dedup_size_limit(),
                )
                .map_err(|error| {
                    LocalCopyError::io("xxh64 dedup check", destination.to_path_buf(), error)
                })?;
                if matches!(outcome, Xxh64DedupOutcome::Match) {
                    record_metadata_only_skip(
                        context,
                        source,
                        destination,
                        metadata,
                        &metadata_options,
                        record_path,
                        existing,
                        &flags,
                        mode,
                        "xxh64 dedup match",
                    )?;
                    return Ok(());
                }
            }
        }
    }

    // Discard the pre-computed delta signature when appending - delta transfer
    // is not applicable in append mode.
    let delta_signature = if append_offset > 0 {
        None
    } else {
        delta_signature
    };

    let (mut reader, copy_source) = if let Some(ref override_path) = copy_source_override {
        let file = match open_source_file(override_path, context.open_noatime_enabled()) {
            Ok(file) => file,
            Err(error) => {
                // upstream: generator.c:919 - rsyserr(FINFO, errno,
                // "copy_file %s => %s", full_fname(src), copy_to) under
                // INFO_GTE(COPY, 1). The override path is the alt-base
                // (`--copy-dest` / `--link-dest` after cross-device degrade)
                // candidate; failing to open it is the local-copy analog of
                // upstream's `copy_file()` failure. Wording mirrors
                // `rsyserr`'s `copy_file SRC => DST: STRERROR (ERRNO)` form.
                let errno = error.raw_os_error().unwrap_or(0);
                let display = error.to_string();
                let suffix = format!(" (os error {errno})");
                let trimmed = display.strip_suffix(&suffix).unwrap_or(&display);
                info_log!(
                    Copy,
                    1,
                    "copy_file {} => {}: {} ({})",
                    override_path.display(),
                    destination.display(),
                    trimmed,
                    errno
                );
                return Err(LocalCopyError::io(
                    "copy file",
                    override_path.clone(),
                    error,
                ));
            }
        };
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

    // IUD-5 opt-in: route eligible whole-file writes through the io_uring
    // registered-buffer path.
    #[cfg(all(target_os = "linux", feature = "iouring-data-writes"))]
    if iouring::eligible(
        context,
        strategy,
        delta_signature.is_some(),
        use_sparse_writes,
        compress_enabled,
        append_offset,
        file_size,
    ) && iouring::try_dispatch(
        context,
        &mut reader,
        source,
        copy_source,
        destination,
        metadata,
        metadata_options.clone(),
        record_path,
        existing_metadata,
        destination_previously_existed,
        file_type,
        relative,
        mode,
        flags,
    )? {
        return Ok(());
    }

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
    // upstream: receiver.c:319 - preallocated_len records how much of the file
    // has reserved blocks so a later sparse zero run inside that extent is
    // punched into a hole rather than seeked over (which would leave the
    // preallocated blocks allocated).
    let mut preallocated_len = maybe_preallocate_destination(
        &mut writer,
        preallocate_target,
        file_size,
        append_offset,
        context.preallocate_enabled(),
    )?;
    // upstream: receiver.c:326-336 - the inplace branch (`preallocated_len = size_r`)
    // is an `else if` reached only when the preallocate branch (receiver.c:320) did
    // NOT run. That branch runs for `--preallocate` whenever the file grows
    // (`total_size > size_r`), leaving preallocated_len at do_fallocate()'s 0 so the
    // reserved tail is seeked, not punched - don't overwrite it. Otherwise the
    // existing destination extent is already allocated, so an interior zero run must
    // be punched. A whole-file inplace copy truncates to zero first (0).
    if preallocated_len == 0 && inplace_enabled && !whole_file_enabled {
        if let Some(existing) = existing_metadata {
            let size_r = existing.len();
            let preallocate_branch_ran = context.preallocate_enabled() && file_size > size_r;
            if !preallocate_branch_ran {
                preallocated_len = size_r;
            }
        }
    }

    // Acquire the copy buffer lazily: paths that move no bytes through it
    // (empty files, fully-resumed append) never allocate. Files that do read
    // and write take a pool-default-sized buffer via `acquire_from`, whose
    // thread-local cache reuses one buffer across sequential files on a worker
    // thread. This keeps copy-buffer allocations O(worker threads) rather than
    // O(files), the churn a per-file `acquire_controlled_from` produced by
    // requesting a file-size-adaptive size that never matched the pool default.
    let copy_bytes_remaining = file_size.saturating_sub(append_offset);
    let needs_buffer = copy_bytes_remaining > 0 || delta_signature.is_some();
    let mut pool_guard = if needs_buffer && context.use_buffer_pool() {
        Some(super::super::super::super::super::BufferPool::acquire_from(
            context.buffer_pool(),
        ))
    } else {
        None
    };
    let mut direct_buffer = if needs_buffer && pool_guard.is_none() {
        vec![0u8; super::super::super::super::super::adaptive_buffer_size(file_size)]
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

    // When a backup RENAME moved the basis file (non-inplace --backup) or a
    // fuzzy basis is used, point the delta transfer at that separate location so
    // it reads matched blocks from there while the writer builds a fresh
    // destination. Under `--inplace --backup` the destination is instead copied
    // aside (inode preserved) and rewritten in place, so `delta_basis_override`
    // stays unset and the writer IS the basis.
    let delta_basis = delta_basis_override.as_deref().unwrap_or(destination);
    // The inplace skip-optimization is unsafe whenever the writer's file is
    // separate from the basis: a freshly created destination contains nothing to
    // skip over, so every matched block must be copied through. Forcing
    // non-inplace here keeps matched-block bytes flowing to the writer instead of
    // being skipped against an empty file.
    let basis_separate_from_writer = delta_basis_override.is_some();
    let copy_result = context.copy_file_contents(
        &mut reader,
        &mut writer,
        buffer,
        use_sparse_writes,
        compress_enabled,
        source,
        delta_basis,
        record_path,
        delta_signature.as_ref(),
        file_size,
        append_offset,
        preallocated_len,
        start,
        basis_separate_from_writer,
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
                // In --partial/--partial-dir mode discard() finalised the temp
                // onto its partial destination; removing it here would defeat
                // the whole point of keeping the partial.
                if existing_metadata.is_none() && !partial_enabled {
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
            if existing_metadata.is_none() && !partial_enabled {
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

    // EMA throughput sample feeds dynamic buffer sizing.
    if context.use_buffer_pool() {
        let pool = context.buffer_pool();
        pool.record_transfer(outcome.literal_bytes() as usize, elapsed);
    }

    let compressed_bytes = outcome.compressed_bytes();
    context
        .summary_mut()
        .record_file(file_size, outcome.literal_bytes(), compressed_bytes);
    context
        .summary_mut()
        .record_copy_method(CopyMethodKind::Standard);
    context.summary_mut().record_elapsed(elapsed);

    let mut metadata_snapshot = LocalCopyMetadata::from_metadata(metadata, None)
        .virtualize_fake_super(source, metadata_options.fake_super_enabled());
    if let Some(size) = device_as_file_size {
        metadata_snapshot = metadata_snapshot.virtualize_copy_device_as_file(size);
    }
    let total_bytes = Some(metadata_snapshot.len());
    let wrote_data = outcome.literal_bytes() > 0 || append_offset > 0;

    // upstream: generator.c:1039 - a `--copy-dest` reconstruction itemizes the
    // attribute columns against the alternate basis (`sxp->st`), not the
    // (absent) prior destination, and never sets ITEM_IS_NEW. Comparing source
    // against the basis with `destination_previously_existed = true` keeps the
    // size/time/perm slots blank when the source already matched the basis.
    let basis_metadata = match reference_basis.as_deref() {
        Some(path) => match fs::symlink_metadata(path) {
            Ok(meta) => Some(meta),
            Err(error) => {
                return Err(LocalCopyError::io(
                    "inspect reference basis",
                    path.to_path_buf(),
                    error,
                ));
            }
        },
        None => None,
    };
    let is_reference_copy = basis_metadata.is_some();
    let change_set = LocalCopyChangeSet::for_file_with_checksum(
        metadata,
        if is_reference_copy {
            basis_metadata.as_ref()
        } else {
            existing_metadata
        },
        &metadata_options,
        is_reference_copy || destination_previously_existed,
        wrote_data && !is_reference_copy,
        flags.xattrs_enabled(),
        flags.acls_enabled(),
        flags.checksum_enabled,
        context.options().modify_window(),
    );
    // upstream: generator.c itemizes a recreated --fake-super device with
    // ITEM_REPORT_CHANGE ('c') when its %stat rdev/type differs from the
    // destination's, even though the placeholder bytes are identical. The node
    // is recreated, so ITEM_REPORT_TIME is set too - rendered 'T' when times
    // are not preserved (the mtime is reset to the transfer time).
    #[cfg(all(unix, feature = "xattr"))]
    let change_set = if metadata_options.fake_super_enabled()
        && destination_previously_existed
        && super::super::super::comparison::fake_super_stat_differs(source, destination)
    {
        let change_set = change_set.with_checksum_changed(true);
        if metadata_options.times() {
            change_set
        } else {
            change_set.with_time_change(Some(crate::local_copy::TimeChange::TransferTime))
        }
    } else {
        change_set
    };
    let action = if is_reference_copy {
        LocalCopyAction::ReferenceCopied
    } else {
        LocalCopyAction::DataCopied
    };
    context.record(
        LocalCopyRecord::new(
            record_path.to_path_buf(),
            action,
            outcome.literal_bytes(),
            total_bytes,
            elapsed,
            Some(metadata_snapshot),
        )
        .with_change_set(change_set)
        .with_creation(!is_reference_copy && !destination_previously_existed),
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
        existing_metadata,
        #[cfg(all(unix, feature = "xattr"))]
        preserve_xattrs,
        #[cfg(all(any(unix, windows), feature = "acl"))]
        preserve_acls,
    )?;

    Ok(())
}

/// Finds a fuzzy delta basis for `destination` when the exact destination is
/// absent and `--fuzzy` is active.
///
/// Scans the destination directory for the closest similarly-named regular
/// file, reusing the shared [`FuzzyMatcher`] scorer. On a hit, emits the
/// `--debug=FUZZY` selection line and returns the candidate's path and
/// metadata so the caller can build a delta signature against it.
///
/// upstream: generator.c:1767-1795 - `find_fuzzy()` selects the basis and the
/// generator announces `"fuzzy basis selected for %s: %s"` at
/// `DEBUG_GTE(FUZZY, 1)` before using it as `fnamecmp`.
fn find_fuzzy_basis(
    context: &CopyContext,
    destination: &Path,
    relative: Option<&Path>,
    file_type: fs::FileType,
    file_size: u64,
    target_mtime: Option<i64>,
) -> Option<(PathBuf, fs::Metadata)> {
    if context.fuzzy_level_enabled() == 0 || !file_type.is_file() {
        return None;
    }

    let target_name = destination.file_name()?;
    let dest_dir = destination.parent()?;

    let matcher = FuzzyMatcher::with_level(context.fuzzy_level_enabled());
    let candidate = matcher.find_fuzzy_basis(target_name, dest_dir, file_size, target_mtime)?;

    let meta = fs::symlink_metadata(&candidate.path).ok()?;
    if !meta.is_file() {
        return None;
    }

    // upstream: generator.c:1787-1793 - announce the selected basis at FUZZY,1
    // as `"fuzzy basis selected for %s: %s"`, where the target is `fname` (the
    // relative transfer path) and the basis is `f_name(fuzzy_file)` - the
    // basis's relative flist name, not an absolute filesystem path. The basis
    // shares the target's directory, so its relative name is the target's
    // parent joined with the candidate basename.
    let target_rel = relative.unwrap_or(destination);
    let target_display = target_rel.display().to_string();
    let basis_display = match (target_rel.parent(), candidate.path.file_name()) {
        (Some(parent), Some(basis_name)) if !parent.as_os_str().is_empty() => {
            parent.join(basis_name).display().to_string()
        }
        (_, Some(basis_name)) => Path::new(basis_name).display().to_string(),
        (_, None) => candidate.path.display().to_string(),
    };
    trace_fuzzy_basis_selected(&target_display, &basis_display);

    Some((candidate.path, meta))
}

/// Converts a [`std::time::SystemTime`] to whole seconds since the Unix epoch
/// for the fuzzy size/modtime fast-path comparison. upstream: generator.c:858
/// `same_time(fp->modtime, 0, file->modtime, 0)`.
fn system_time_to_unix_secs(time: std::time::SystemTime) -> i64 {
    match time.duration_since(std::time::UNIX_EPOCH) {
        Ok(delta) => delta.as_secs() as i64,
        Err(err) => -(err.duration().as_secs() as i64),
    }
}
