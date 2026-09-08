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
///
/// Reads the gate flags off [`TransferFlags`] like `clonefile::eligible`,
/// `ficlone::eligible` and `wincopy::eligible` do, so every whole-file mover
/// consumes the one owner of `whole_file_enabled` instead of re-deriving it.
/// Without that conjunct `--no-whole-file` was honoured by the other movers
/// and ignored here whenever no basis existed to build a delta signature
/// from (`delta_signature_present` only covers the existing-destination
/// case), so the operator's request landed in this mover's unobserved
/// whole-file path instead of the read loop.
pub(super) fn eligible(
    context: &CopyContext,
    strategy: WriteStrategy,
    delta_signature_present: bool,
    flags: TransferFlags,
    append_offset: u64,
    file_size: u64,
) -> bool {
    let TransferFlags {
        whole_file_enabled,
        use_sparse_writes,
        compress_enabled,
        ..
    } = flags;

    matches!(strategy, WriteStrategy::Direct)
        && whole_file_enabled
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
        // A whole-file clone/copy is all literal: no signature was
        // consulted, so nothing was matched.
        .record_file(file_size, file_size, crate::local_copy::MATCHED_NONE, None);
    context
        .summary_mut()
        .record_copy_method(CopyMethodKind::IoUring);
    context.summary_mut().record_elapsed(elapsed);
    let metadata_snapshot = LocalCopyMetadata::from_metadata(metadata, None)
        .virtualize_fake_super(source, metadata_options.fake_super_enabled());
    let total_bytes = Some(metadata_snapshot.len());
    let change_set = LocalCopyChangeSet::for_file(
        metadata,
        existing_metadata,
        &metadata_options,
        destination_previously_existed,
        true,
        flags.xattrs_changed,
        flags.acls_enabled(),
        context.options().modify_window(),
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
            // The `read_to_end` above is this mover's only look at the
            // source; a byte count short of the length the transfer was
            // sized from is the same source-shrank-mid-transfer condition
            // the standard read loop reports, so funnel it through the one
            // owner of that diagnostic instead of claiming the full length.
            context.note_short_source_read(copy_source, buf.len() as u64, file_size);
            context.register_progress();
            Ok(Some(IoUringDataWriteOutcome {
                elapsed: start.elapsed(),
            }))
        }
        Err(error) if error.kind() == std::io::ErrorKind::Unsupported => Ok(None),
        Err(error) => Err(LocalCopyError::io("copy file", destination, error)),
    }
}

#[cfg(test)]
mod tests {
    use std::fs::{self, File};
    use std::io::Write;
    use std::path::Path;

    use tempfile::TempDir;

    use super::super::super::TransferFlags;
    use super::*;
    use crate::local_copy::{LocalCopyExecution, LocalCopyOptions};

    /// Flags that satisfy every conjunct of [`eligible`].
    fn eligible_flags() -> TransferFlags {
        TransferFlags {
            append_allowed: false,
            append_verify: false,
            whole_file_enabled: true,
            inplace_enabled: false,
            partial_enabled: false,
            use_sparse_writes: false,
            compress_enabled: false,
            size_only_enabled: false,
            ignore_times_enabled: false,
            checksum_enabled: false,
            #[cfg(all(any(unix, windows), feature = "xattr"))]
            preserve_xattrs: false,
            xattrs_changed: false,
            #[cfg(all(any(unix, windows), feature = "acl"))]
            preserve_acls: false,
        }
    }

    fn test_context(root: &Path) -> CopyContext<'_> {
        CopyContext::new(
            LocalCopyExecution::Apply,
            LocalCopyOptions::default(),
            None,
            root.to_path_buf(),
        )
    }

    /// `--no-whole-file` must refuse this mover exactly like it refuses the
    /// other whole-file movers, including when no basis exists and therefore
    /// no delta signature was built.
    #[test]
    fn eligible_requires_whole_file() {
        let temp = TempDir::new().expect("tempdir");
        let context = test_context(temp.path());
        let mut flags = eligible_flags();

        assert!(
            eligible(
                &context,
                WriteStrategy::Direct,
                false,
                flags,
                0,
                IOURING_DATA_WRITES_MIN_BYTES,
            ),
            "control: fully eligible flags must pass the gate",
        );

        flags.whole_file_enabled = false;
        assert!(
            !eligible(
                &context,
                WriteStrategy::Direct,
                false,
                flags,
                0,
                IOURING_DATA_WRITES_MIN_BYTES,
            ),
            "--no-whole-file must disqualify the io_uring data-write mover",
        );
    }

    /// A source that ended before the length the transfer was sized from must
    /// be reported through `note_short_source_read` (forcing exit 23), not
    /// recorded as a full-length success.
    #[test]
    fn dispatch_reports_short_source_read() {
        if !fast_io::is_io_uring_available() {
            eprintln!("skipping: io_uring unavailable in this environment");
            return;
        }

        let temp = TempDir::new().expect("tempdir");
        let source = temp.path().join("src.bin");
        let destination = temp.path().join("dst.bin");
        const ACTUAL_LEN: usize = 1024;
        File::create(&source)
            .expect("create source")
            .write_all(&[0x5a; ACTUAL_LEN])
            .expect("write source");
        // The file list recorded more bytes than the opened file holds.
        let declared_len = ACTUAL_LEN as u64 + 4096;

        let mut context = test_context(temp.path());
        let mut reader = File::open(&source).expect("open source");
        let outcome = dispatch_iouring_data_write(
            &mut context,
            &mut reader,
            &source,
            &destination,
            declared_len,
            start_iouring_data_write(),
        )
        .expect("dispatch");
        assert!(
            outcome.is_some(),
            "io_uring probe passed but the dispatch fell back",
        );
        assert!(
            context.source_read_error_occurred(),
            "a short source read must be recorded for RERR_PARTIAL (23)",
        );
        assert_eq!(
            fs::metadata(&destination).expect("stat destination").len(),
            ACTUAL_LEN as u64,
            "the destination holds exactly the bytes the source still had",
        );
    }

    /// Control for the short-read pin: a source that still holds every
    /// declared byte must not be flagged.
    #[test]
    fn dispatch_full_source_records_no_read_error() {
        if !fast_io::is_io_uring_available() {
            eprintln!("skipping: io_uring unavailable in this environment");
            return;
        }

        let temp = TempDir::new().expect("tempdir");
        let source = temp.path().join("src.bin");
        let destination = temp.path().join("dst.bin");
        const ACTUAL_LEN: usize = 1024;
        File::create(&source)
            .expect("create source")
            .write_all(&[0x5a; ACTUAL_LEN])
            .expect("write source");

        let mut context = test_context(temp.path());
        let mut reader = File::open(&source).expect("open source");
        let outcome = dispatch_iouring_data_write(
            &mut context,
            &mut reader,
            &source,
            &destination,
            ACTUAL_LEN as u64,
            start_iouring_data_write(),
        )
        .expect("dispatch");
        assert!(
            outcome.is_some(),
            "io_uring probe passed but the dispatch fell back",
        );
        assert!(
            !context.source_read_error_occurred(),
            "a full-length read must not be flagged as a short source read",
        );
    }
}
