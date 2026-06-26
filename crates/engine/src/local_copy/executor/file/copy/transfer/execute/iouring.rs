//! IUD-5 opt-in: io_uring registered-buffer data-write fast path.
//!
//! Routes eligible whole-file writes through `fast_io::write_file_with_io_uring`
//! so the kernel can submit the write via the io_uring submission queue with
//! pre-registered buffers. Limited to the `Direct` write strategy so the
//! wrapper's path-based signature lands the bytes at the same inode the
//! standard path would have produced - no temp file rename, no inplace
//! overwrite, no append seek. Default builds skip this branch entirely.

#![cfg(all(target_os = "linux", feature = "iouring-data-writes"))]

use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::time::{Duration, Instant};

use ::metadata::MetadataOptions;

use crate::local_copy::{
    CopyContext, CopyMethodKind, CreatedEntryKind, LocalCopyAction, LocalCopyChangeSet,
    LocalCopyError, LocalCopyExecution, LocalCopyMetadata, LocalCopyRecord,
};

use super::super::TransferFlags;
use super::super::finalize::finalize_guard_and_metadata;
use super::super::write_strategy::WriteStrategy;

/// Minimum file size to bother dispatching through the io_uring fast path.
const IOURING_DATA_WRITES_MIN_BYTES: u64 = 1024 * 1024;

/// Outcome of the io_uring data-write dispatch helper.
///
/// `elapsed` is measured from the moment the dispatch starts so the caller
/// can record it in the transfer summary.
pub(super) struct IoUringDataWriteOutcome {
    elapsed: Duration,
}

/// Returns the start instant for an io_uring data-write dispatch.
///
/// Wrapped in a function so the helper has a single source of truth and the
/// dispatch site stays focused on the eligibility check.
fn start_iouring_data_write() -> Instant {
    Instant::now()
}

/// Returns whether the current transfer is eligible for the io_uring path.
pub(super) fn eligible(
    context: &CopyContext,
    strategy: WriteStrategy,
    delta_signature_present: bool,
    use_sparse_writes: bool,
    compress_enabled: bool,
    append_offset: u64,
    file_size: u64,
) -> bool {
    matches!(strategy, WriteStrategy::Direct)
        && !delta_signature_present
        && !use_sparse_writes
        && !compress_enabled
        && !context.has_bandwidth_limiter()
        && append_offset == 0
        && file_size >= IOURING_DATA_WRITES_MIN_BYTES
}

/// Attempts to drive the transfer through io_uring; returns `true` on success.
///
/// When the io_uring backend is unavailable at runtime the reader is rewound
/// to byte 0 so the caller transparently falls back to the standard copy
/// path. Any real disk failure after a successful submission is surfaced
/// unchanged so the transfer aborts.
#[allow(clippy::too_many_arguments)]
pub(super) fn try_dispatch(
    context: &mut CopyContext,
    reader: &mut fs::File,
    source: &Path,
    copy_source: &Path,
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
) -> Result<bool, LocalCopyError> {
    let file_size = metadata.len();

    let dispatch_result = dispatch_iouring_data_write(
        context,
        reader,
        copy_source,
        destination,
        file_size,
        start_iouring_data_write(),
    )?;
    if dispatch_result.is_none() {
        // io_uring path unavailable: rewind the reader so the standard copy
        // loop starts from byte 0. The dispatch helper may have drained the
        // source via `read_to_end`.
        reader
            .seek(SeekFrom::Start(0))
            .map_err(|error| LocalCopyError::io("copy file", copy_source, error))?;
        return Ok(false);
    }

    let outcome = dispatch_result.expect("dispatch_result checked above");
    let elapsed = outcome.elapsed;
    context.capture_batch_whole_file(copy_source, file_size)?;
    context.finalize_batch_file_delta(copy_source)?;
    context.register_created_path(
        destination,
        CreatedEntryKind::File,
        destination_previously_existed,
    );
    context.record_hard_link(metadata, destination);
    context
        .summary_mut()
        .record_file(file_size, file_size, None);
    context
        .summary_mut()
        .record_copy_method(CopyMethodKind::IoUring);
    context.summary_mut().record_elapsed(elapsed);
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
            elapsed,
            Some(metadata_snapshot),
        )
        .with_change_set(change_set)
        .with_creation(true),
    );
    let mut writer_for_metadata: Option<fs::File> = None;
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
        &mut writer_for_metadata,
        existing_metadata,
        #[cfg(all(unix, feature = "xattr"))]
        flags.preserve_xattrs,
        #[cfg(all(any(unix, windows), feature = "acl"))]
        flags.preserve_acls,
    )?;
    Ok(true)
}

/// Reads `source` into memory, then writes via `fast_io::write_file_with_io_uring`.
///
/// Returns `Ok(None)` when the io_uring backend is unavailable at runtime so
/// the caller transparently falls back to the standard copy path. Returns
/// `Ok(Some(...))` on a successful write through the registered-buffer path.
///
/// Probes [`fast_io::is_io_uring_available`] up front so kernels that lack
/// io_uring or environments that block `io_uring_setup(2)` (seccomp, missing
/// MEMLOCK headroom, etc.) skip the path silently rather than failing the
/// transfer. Any error after a successful submission is surfaced unchanged so
/// real disk failures still abort the file.
fn dispatch_iouring_data_write(
    context: &mut CopyContext,
    reader: &mut fs::File,
    copy_source: &Path,
    destination: &Path,
    file_size: u64,
    start: Instant,
) -> Result<Option<IoUringDataWriteOutcome>, LocalCopyError> {
    if !fast_io::is_io_uring_available() {
        return Ok(None);
    }

    let mut buf = Vec::with_capacity(file_size as usize);
    reader
        .read_to_end(&mut buf)
        .map_err(|error| LocalCopyError::io("copy file", copy_source, error))?;

    match fast_io::write_file_with_io_uring(destination, &buf) {
        Ok(()) => {
            context.register_progress();
            Ok(Some(IoUringDataWriteOutcome {
                elapsed: start.elapsed(),
            }))
        }
        Err(error) if error.kind() == std::io::ErrorKind::Unsupported => Ok(None),
        Err(error) => Err(LocalCopyError::io("copy file", destination, error)),
    }
}
