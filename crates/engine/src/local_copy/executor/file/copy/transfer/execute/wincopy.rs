//! Windows CopyFileExW / ReFS reflink fast path for whole-file copies.
//!
//! On Windows, `CopyFileExW` provides kernel-side data movement with
//! optional `COPY_FILE_NO_BUFFERING` for files above 4 MiB. On ReFS
//! volumes, `FSCTL_DUPLICATE_EXTENTS_TO_FILE` produces an instant
//! copy-on-write clone. Both paths are reached through the
//! [`fast_io::PlatformCopy`] strategy stored in `LocalCopyOptions`.
//!
//! Without this fast path the executor falls into the generic read/write
//! loop in [`fast_io::copy_file_range::copy_file_contents_buffered`],
//! which on Windows stubs both `copy_file_range` and io_uring and
//! degenerates into a synchronous `ReadFile`/`WriteFile` 256 KiB loop.
//!
//! The parent `mod.rs` gates this module on `target_os = "windows"`, so
//! no inner `#![cfg(...)]` is required.

use std::fs;
use std::path::Path;
use std::time::Instant;

use logging::debug_log;

use ::metadata::MetadataOptions;

use crate::local_copy::{
    CopyContext, CopyMethodKind, CreatedEntryKind, LocalCopyAction, LocalCopyChangeSet,
    LocalCopyError, LocalCopyExecution, LocalCopyMetadata, LocalCopyRecord,
};

use super::super::TransferFlags;
use super::super::finalize::finalize_guard_and_metadata;

/// Returns whether the current transfer satisfies every `CopyFileExW` precondition.
///
/// Mirrors the macOS clonefile eligibility check: the fast path only fires
/// for fresh whole-file copies where no delta, sparse, compression, or
/// bandwidth-limiting logic needs to observe the byte stream.
pub(super) fn eligible(
    context: &CopyContext,
    existing_metadata: Option<&fs::Metadata>,
    flags: TransferFlags,
    copy_source_override_present: bool,
) -> bool {
    let TransferFlags {
        whole_file_enabled,
        inplace_enabled,
        partial_enabled,
        use_sparse_writes,
        compress_enabled,
        ..
    } = flags;

    existing_metadata.is_none()
        && whole_file_enabled
        && !inplace_enabled
        && !partial_enabled
        && !use_sparse_writes
        && !compress_enabled
        && !copy_source_override_present
        && !context.has_bandwidth_limiter()
        && !context.delay_updates_enabled()
        && context.temp_directory_path().is_none()
}

/// Attempts the Windows-optimized copy; returns `true` on success.
///
/// Dispatches through `context.options().platform_copy()` which on Windows
/// chains: ReFS `FSCTL_DUPLICATE_EXTENTS_TO_FILE` (when destination is on a
/// ReFS volume) -> `CopyFileExW` with `COPY_FILE_NO_BUFFERING` for files
/// larger than 4 MiB -> `std::fs::copy` fallback.
///
/// On any error or when the strategy reports `StandardCopy` (the portable
/// `std::fs::copy` fallback), the destination is removed and the caller
/// falls through to the normal copy path so the standard write-strategy
/// logic (Direct / TempFileRename) runs unchanged.
#[allow(clippy::too_many_arguments)]
pub(super) fn try_copy(
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
) -> Result<bool, LocalCopyError> {
    use fast_io::CopyMethod;

    let file_size = metadata.len();

    let dispatched_method =
        match context
            .options()
            .platform_copy()
            .copy_file(source, destination, file_size)
        {
            Ok(result) => match result.method {
                method @ (CopyMethod::CopyFileEx | CopyMethod::ReFsReflink) => Some(method),
                _ => {
                    // StandardCopy (std::fs::copy fallback) does not exercise the
                    // Windows-optimal kernel path; let the executor's normal write
                    // strategy own the bytes so behaviour matches non-Windows.
                    let _ = std::fs::remove_file(destination);
                    None
                }
            },
            Err(_) => {
                let _ = std::fs::remove_file(destination);
                None
            }
        };
    let Some(dispatched_method) = dispatched_method else {
        return Ok(false);
    };

    let start = Instant::now();
    debug_log!(
        Send,
        1,
        "wincopy {}: {} bytes",
        record_path.display(),
        file_size
    );

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
    context
        .summary_mut()
        .record_copy_method(CopyMethodKind::from_platform(dispatched_method));
    context.summary_mut().record_elapsed(start.elapsed());

    // Normalize copied metadata to match what open()-created files have.
    // `CopyFileExW` preserves the source's mtime verbatim. Without this,
    // finalize_guard_and_metadata skips corrections when preservation is
    // disabled (e.g. --no-times), leaving the source mtime instead of the
    // current-time default a newly created file would have.
    // upstream: rsync creates files via open() then applies metadata -
    // CopyFileExW must produce identical results.
    normalize_copied_metadata(destination, &metadata_options)?;

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

/// Normalizes a `CopyFileExW`-copied destination to match open()-created defaults.
///
/// `CopyFileExW` preserves the source's exact mtime. When the user has not
/// requested timestamp preservation (e.g. `--no-times`),
/// `finalize_guard_and_metadata` will skip mtime corrections because it
/// assumes the file already has process-default metadata. This function
/// bridges that gap by resetting mtime to the current time so the finalize
/// step works identically to the standard copy path.
fn normalize_copied_metadata(
    destination: &Path,
    options: &::metadata::MetadataOptions,
) -> Result<(), LocalCopyError> {
    if !options.times() {
        let now = filetime::FileTime::now();
        filetime::set_file_mtime(destination, now)
            .map_err(|e| LocalCopyError::io("normalize copied mtime", destination, e))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use fast_io::{CopyMethod, CopyResult, PlatformCopy};
    use std::io;
    use std::sync::Mutex;
    use tempfile::TempDir;

    /// Test double that records the size hint and returns a chosen method.
    #[derive(Debug)]
    struct StubPlatformCopy {
        method: CopyMethod,
        last_size: Mutex<Option<u64>>,
        bytes_copied: u64,
    }

    impl PlatformCopy for StubPlatformCopy {
        fn copy_file(
            &self,
            src: &std::path::Path,
            dst: &std::path::Path,
            size_hint: u64,
        ) -> io::Result<CopyResult> {
            *self.last_size.lock().expect("lock") = Some(size_hint);
            // Honour the contract: zero-copy methods report bytes_copied = 0,
            // data-copy methods report the actual bytes the kernel moved.
            let bytes = if matches!(self.method, CopyMethod::ReFsReflink) {
                0
            } else {
                self.bytes_copied
            };
            // Touch the destination so eligibility callers can observe a
            // visible side-effect, mirroring real CopyFileExW semantics.
            std::fs::copy(src, dst)?;
            Ok(CopyResult::new(bytes, self.method))
        }

        fn preferred_method(&self, _size: u64) -> CopyMethod {
            self.method
        }

        fn supports_reflink(&self) -> bool {
            matches!(self.method, CopyMethod::ReFsReflink)
        }
    }

    fn write_file(path: &std::path::Path, bytes: &[u8]) {
        std::fs::write(path, bytes).expect("write source file");
    }

    /// Files above 4 MiB must hand a size hint that triggers
    /// `COPY_FILE_NO_BUFFERING` inside the dispatcher.
    #[test]
    fn size_hint_above_no_buffering_threshold_is_forwarded() {
        let temp = TempDir::new().expect("temp dir");
        let src = temp.path().join("src.bin");
        let dst = temp.path().join("dst.bin");
        let payload =
            vec![0xCDu8; (crate::local_copy::win_copy::NO_BUFFERING_THRESHOLD + 1) as usize];
        write_file(&src, &payload);

        let stub = StubPlatformCopy {
            method: CopyMethod::CopyFileEx,
            last_size: Mutex::new(None),
            bytes_copied: payload.len() as u64,
        };

        let result = stub
            .copy_file(&src, &dst, payload.len() as u64)
            .expect("copy succeeds");
        assert!(matches!(result.method, CopyMethod::CopyFileEx));
        assert_eq!(result.bytes_copied, payload.len() as u64);
        let recorded = stub.last_size.lock().expect("lock").expect("size recorded");
        assert!(
            recorded > crate::local_copy::win_copy::NO_BUFFERING_THRESHOLD,
            "dispatcher must see size above NO_BUFFERING_THRESHOLD; got {recorded}"
        );

        // Destination must exist and round-trip.
        let copied = std::fs::read(&dst).expect("read dst");
        assert_eq!(copied, payload);
    }

    /// A `StandardCopy` outcome means the dispatcher fell back to
    /// `std::fs::copy`; the executor must NOT short-circuit in that case so
    /// the normal write-strategy path runs.
    #[test]
    fn standard_copy_result_is_not_treated_as_fast_path() {
        let temp = TempDir::new().expect("temp dir");
        let src = temp.path().join("src.bin");
        let dst = temp.path().join("dst.bin");
        write_file(&src, b"hello");

        let stub = StubPlatformCopy {
            method: CopyMethod::StandardCopy,
            last_size: Mutex::new(None),
            bytes_copied: 5,
        };
        let result = stub.copy_file(&src, &dst, 5).expect("copy succeeds");

        // The classification used by `try_copy` to decide fast-path success.
        let dispatched = matches!(
            result.method,
            CopyMethod::CopyFileEx | CopyMethod::ReFsReflink
        );
        assert!(
            !dispatched,
            "StandardCopy must fall through to the executor's normal write path"
        );
    }

    /// ReFS reflink reports zero bytes_copied because nothing physically
    /// moved; the executor must still treat it as the fast-path success.
    #[test]
    fn refs_reflink_result_is_treated_as_fast_path() {
        let temp = TempDir::new().expect("temp dir");
        let src = temp.path().join("src.bin");
        let dst = temp.path().join("dst.bin");
        write_file(&src, b"hello");

        let stub = StubPlatformCopy {
            method: CopyMethod::ReFsReflink,
            last_size: Mutex::new(None),
            bytes_copied: 0,
        };
        let result = stub.copy_file(&src, &dst, 5).expect("copy succeeds");

        let dispatched = matches!(
            result.method,
            CopyMethod::CopyFileEx | CopyMethod::ReFsReflink
        );
        assert!(dispatched, "ReFsReflink must take the fast path");
        assert_eq!(result.bytes_copied, 0, "reflink reports zero-copy");
    }
}
