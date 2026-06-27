//! Linux FICLONE ioctl fast path for whole-file copies.
//!
//! `ioctl(FICLONE)` creates a copy-on-write reflink on filesystems that
//! support it (Btrfs, XFS with reflink enabled, bcachefs). Zero data is
//! copied - source and destination share storage blocks until either is
//! modified. The operation is O(1) regardless of file size.
//!
//! The eligibility checks, dispatch, and post-clone bookkeeping evolve as
//! a single concern; the parent `mod.rs` already gates this module on
//! `target_os = "linux"`, so no inner `#![cfg(...)]` is needed.
//!
//! Cross-filesystem, unsupported-filesystem, and read-only-fs failures are
//! mapped to `Ok(false)` so the caller falls through to the generic copy
//! path. Any I/O error after a successful reflink propagates as
//! `LocalCopyError`.

use std::fs;
use std::path::Path;
use std::time::Instant;

use logging::debug_log;

use ::metadata::MetadataOptions;

use crate::local_copy::overrides::same_filesystem;
use crate::local_copy::{
    CopyContext, CopyMethodKind, CreatedEntryKind, LocalCopyAction, LocalCopyChangeSet,
    LocalCopyError, LocalCopyExecution, LocalCopyMetadata, LocalCopyRecord,
};

use super::super::TransferFlags;
use super::super::finalize::finalize_guard_and_metadata;

/// Returns whether the current transfer satisfies every FICLONE precondition.
///
/// Mirrors the macOS [`super::clonefile::eligible`] gate: FICLONE preserves
/// the source's data and timestamps verbatim, so any code path that needs
/// delta, sparse handling, inplace writes, compression, bandwidth shaping,
/// staging directories, or xattr filter rules must fall through to the
/// regular copy path. The destination must also be a fresh file - FICLONE
/// fails if it already exists.
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

    // Unlike macOS clonefile, FICLONE clones data extents only into a fresh
    // File::create() inode - it never copies the source's xattrs, so plain
    // `-a` (no -X) is safe with no post-clone strip: the destination simply
    // starts with none. When -X is set, finalize applies the source xattrs
    // (it runs for this path with flags.preserve_xattrs). The one case a
    // blanket clone cannot reproduce is a selective xattr `--filter`, which
    // still takes the slow path so the filter can be honoured per-attribute.
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

/// Attempts the FICLONE fast path; returns `true` on success.
///
/// Delegates to [`fast_io::try_ficlone`] (which wraps `ioctl_ficlone` via
/// `rustix`). FICLONE failures - cross-device (`EXDEV`), unsupported
/// filesystem (`EOPNOTSUPP`), permission, etc. - are mapped to
/// `Ok(false)` so the caller falls through to the generic read/write loop.
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

    // REFLINK-2: skip the FICLONE ioctl when source and destination live on
    // different filesystems. ioctl(FICLONE) only reflinks within one
    // filesystem and fails with EXDEV across mounts - but not before
    // File::create() has already materialised an empty destination inode that
    // we then have to unlink. Comparing st_dev up front avoids that wasted
    // create/ioctl/unlink round-trip on cross-filesystem copies. When the
    // device ids cannot be determined the helper returns None and we fall
    // through to let try_ficlone decide.
    if same_filesystem(source, metadata, destination) == Some(false) {
        return Ok(false);
    }

    if fast_io::try_ficlone(source, destination).is_err() {
        let _ = std::fs::remove_file(destination);
        return Ok(false);
    }

    let start = Instant::now();
    debug_log!(
        Send,
        1,
        "cloned {}: {} bytes (FICLONE)",
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
        .record_copy_method(CopyMethodKind::Ficlone);
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

    // FICLONE preserves source metadata verbatim. Normalize so finalize
    // sees a fresh-open()-style destination when the user did not request
    // preservation of perms/times, mirroring the macOS clonefile path.
    // upstream: rsync creates via open() then applies metadata - reflinks
    // must produce identical observable results.
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
        None,
        #[cfg(all(unix, feature = "xattr"))]
        flags.preserve_xattrs,
        #[cfg(all(any(unix, windows), feature = "acl"))]
        flags.preserve_acls,
    )?;

    Ok(true)
}

/// Normalizes a FICLONE'd destination to match open()-created file defaults.
///
/// `FICLONE` clones the source's data and inherits the destination's mode
/// from the `creat()` call inside the dispatcher. Permissions thus already
/// reflect the process umask, but timestamps inherit from the source. When
/// preservation is disabled (`--no-times`), reset mtime to "now" so
/// finalize behaves identically to the regular copy path.
fn normalize_cloned_metadata(
    destination: &Path,
    _source_metadata: &fs::Metadata,
    options: &::metadata::MetadataOptions,
) -> Result<(), LocalCopyError> {
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
