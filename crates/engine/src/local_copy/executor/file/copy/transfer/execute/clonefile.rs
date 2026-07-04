//! macOS APFS clonefile fast path for whole-file copies.
//!
//! `clonefile(2)` creates a copy-on-write clone on APFS, avoiding all
//! read/write I/O. It is only safe when all metadata will either be
//! preserved by finalize or corrected by [`normalize_cloned_metadata`].
//!
//! The fast-path dispatcher and metadata normalizer live here together so
//! the eligibility checks, dispatch, and post-clone bookkeeping all evolve
//! as a single concern. The parent `mod.rs` already gates this module on
//! `target_os = "macos"`, so no inner `#![cfg(...)]` is needed.

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

/// Returns whether the current transfer satisfies every clonefile precondition.
///
/// Centralizes all conditions that would make clonefile produce incorrect
/// results so [`finalize_guard_and_metadata`] works identically for both the
/// clonefile and standard copy paths.
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

    let transfer_ok = existing_metadata.is_none()
        && whole_file_enabled
        && !inplace_enabled
        && !partial_enabled
        && !use_sparse_writes
        && !compress_enabled
        && !copy_source_override_present
        && !context.has_bandwidth_limiter()
        && !context.delay_updates_enabled()
        && context.temp_directory_path().is_none();

    // Extended attributes: clonefile copies all xattrs verbatim. We can still
    // take the fast path when xattrs are disabled (`-a` without `-X`) by
    // stripping the source-originated attributes from the clone afterwards
    // (see try_clone). The one case we cannot reproduce with a blanket strip
    // is a selective xattr `--filter`, so keep the slow path there.
    //
    // The ACL dimension (`-A`) is intentionally unchanged here: clonefile
    // already copies source ACLs on the existing `-X`-on fast path regardless
    // of `-A`, so relaxing `-X` does not introduce a new ACL behavior. The
    // pre-existing ACL-leak-when-`-A`-off is tracked separately.
    let xattr_ok = {
        #[cfg(all(unix, feature = "xattr"))]
        {
            let has_filter_rules = context
                .filter_program()
                .is_some_and(|p| p.has_xattr_rules());
            !has_filter_rules
        }
        #[cfg(not(all(unix, feature = "xattr")))]
        {
            true
        }
    };

    transfer_ok && xattr_ok
}

/// Attempts the clonefile fast path; returns `true` on success.
///
/// Dispatches through the configured `PlatformCopy`. Only commits to the
/// fast path when the strategy reported a true zero-copy reflink
/// (clonefile/FICLONE/ReFS reflink); any data-copy fallback would bypass
/// rsync's delta machinery without honouring the eligibility assumptions,
/// so on non-zero-copy results we discard and fall through to the normal
/// copy path.
#[allow(clippy::too_many_arguments)]
pub(super) fn try_clone(
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
    let file_size = metadata.len();

    let clone_method =
        match context
            .options()
            .platform_copy()
            .copy_file(source, destination, file_size)
        {
            Ok(result) if result.is_zero_copy() => Some(result.method),
            Ok(_) => {
                let _ = std::fs::remove_file(destination);
                None
            }
            Err(_) => {
                let _ = std::fs::remove_file(destination);
                None
            }
        };
    let Some(clone_method) = clone_method else {
        // clonefile failed (cross-device, non-APFS, etc.) - caller falls
        // through to normal copy path.
        debug_log!(
            Clone,
            1,
            "CoW clone unavailable for {}: using standard copy",
            record_path.display()
        );
        return Ok(false);
    };

    let start = Instant::now();
    debug_log!(
        Send,
        1,
        "cloned {}: {} bytes (CoW)",
        record_path.display(),
        file_size
    );
    debug_log!(
        Clone,
        1,
        "CoW clone succeeded: dst={} ({} bytes)",
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
        .record_copy_method(CopyMethodKind::from_platform(clone_method));
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
        context.options().modify_window(),
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

    // clonefile() copies the source's extended attributes verbatim. When the
    // user did not request xattr preservation (`-a` without `-X`), upstream
    // rsync writes a fresh destination that carries none of them, so strip the
    // source-originated attributes the clone introduced. finalize's sync_xattrs
    // only runs when preservation is on, so this is the off-case correction.
    // upstream: rsync's receiver open()s a new file and never copies xattrs
    // without --xattrs.
    #[cfg(all(unix, feature = "xattr"))]
    if !flags.preserve_xattrs {
        ::metadata::strip_source_xattrs(source, destination, false)
            .map_err(crate::local_copy::metadata_sync::map_metadata_error)?;
    }

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
        existing_metadata,
        #[cfg(all(unix, feature = "xattr"))]
        flags.preserve_xattrs,
        #[cfg(all(any(unix, windows), feature = "acl"))]
        flags.preserve_acls,
    )?;

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
