//! macOS APFS clonefile fast path for whole-file copies.
//!
//! `clonefile(2)` creates a copy-on-write clone on APFS, avoiding all
//! read/write I/O. It is only safe when all metadata will either be
//! preserved by finalize, reset by the clone itself (`fast_io::CloneFreshTimes`),
//! or corrected by [`normalize_cloned_metadata`].
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

    // The injected `PlatformCopy` still decides whether this tier runs at all:
    // `NoReflinkPlatformCopy` (and any test fake) reports no reflink support and
    // vetoes the fast path, leaving the standard copy to do the work. The
    // *destination write* no longer goes through the strategy - a path-based
    // `copy_file(src, dst)` cannot express an anchored destination, which is
    // exactly what made it an escape - so the strategy's role here is the gate,
    // not the write.
    let transfer_ok = context.options().platform_copy().supports_reflink()
        && existing_metadata.is_none()
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
    // ACLs (`-A`) need no gate: without `CLONE_ACL` the clone takes the target
    // directory's inheritable ACLs exactly as an `open()`ed file does, and
    // finalize applies the source ACL under `-A`.
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

    // A reflink both creates and fills the destination in one syscall, so the
    // create IS the confinement decision - there is no later commit to anchor.
    // `confined_clone_file` walks the destination's parent per component and
    // clones through that dirfd, which is the only race-free form: a
    // check-then-`clonefile` gate would still lose to a parent flipped between
    // the two.
    //
    // A confinement refusal is an error and propagates. Only "this filesystem
    // will not reflink" falls through to the standard copy path, which anchors
    // its own create.
    //
    // upstream: `rsync-3.5.0/receiver.c` has no CoW fast path at all - it
    // always stages into a temp and commits with `do_rename_at()`, inheriting
    // confinement from the rename. This keeps oc's optimisation on the same
    // invariant rather than beside it.
    // The clone READS the source, so the source side takes the SAME confined
    // open the standard copy path uses (`open_source_content`, which honours the
    // transfer-root anchor and the leaf policy). Handing `confined_clone_file`
    // a PATH instead would let it re-resolve the source with the libc
    // resolver, which follows every parent component - so a parent flipped to
    // a symlink pointing outside the transfer root would be followed and its
    // contents cloned into the destination. That is a measured defect, not a
    // hypothetical one: it is what made the upstream `symlink-race-source`
    // cell leak on macOS while the confined open refused the same shape.
    //
    // upstream: `rsync-3.5.1/sender.c:215-252` `sender_open_confined()` - the
    // sender's content read is confined, and this CoW fast path is an oc
    // extension that must not weaken that invariant.
    #[cfg(unix)]
    let clone_method = {
        let reader = context
            .open_source_content(source)
            .map_err(|error| LocalCopyError::io("copy file", source, error))?;
        let fresh = fast_io::CloneFreshTimes {
            crtime: !metadata_options.crtimes(),
            mtime: !metadata_options.times(),
            atime: !metadata_options.atimes(),
        };
        match fast_io::confined_clone_file(context.destination_root(), &reader, destination, fresh)
        {
            Ok(fast_io::CloneAttempt::Cloned) => Some(
                context
                    .options()
                    .platform_copy()
                    .preferred_method(file_size),
            ),
            Ok(fast_io::CloneAttempt::Unsupported) => None,
            Err(error) => return Err(LocalCopyError::io("copy file", destination, error)),
        }
    };
    #[cfg(not(unix))]
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
        // A whole-file clone/copy is all literal: no signature was
        // consulted, so nothing was matched.
        .record_file(file_size, file_size, crate::local_copy::MATCHED_NONE, None);
    // upstream writes the same bytes through write_file(), crediting every
    // block from offset 0 (fileio.c:251-252).
    context.summary_mut().record_touched_blocks(
        crate::block_touch::BlockTouchTracker::blocks_spanned(0, file_size),
    );
    context
        .summary_mut()
        .record_copy_method(CopyMethodKind::from_platform(clone_method));
    context.summary_mut().record_elapsed(start.elapsed());

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
            start.elapsed(),
            Some(metadata_snapshot),
        )
        .with_change_set(change_set)
        .with_creation(true),
    );

    // Normalize cloned metadata to match what open()-created files have.
    // clonefile() preserves source metadata verbatim. Without this,
    // finalize_guard_and_metadata skips corrections when preservation is
    // disabled (e.g. --no-perms), leaving source metadata instead of the
    // umask default. Timestamps were already reset by the clone itself (see
    // `CloneFreshTimes` above).
    // upstream: rsync creates files via open() then applies metadata -
    // clonefile must produce identical results.
    normalize_cloned_metadata(destination, metadata, &metadata_options)?;

    // clonefile() copies the source's extended attributes verbatim, but
    // upstream's receiver open()s a fresh file that carries none of them. Under
    // `-X` finalize's sync_xattrs reconciles the ordinary names; it never
    // deletes rsync's own `rsync.%` channel, which the sender never transmits,
    // so that channel is stripped here. Without `-X` every source-originated
    // name goes.
    #[cfg(all(unix, feature = "xattr"))]
    {
        let strip = if flags.preserve_xattrs {
            ::metadata::strip_source_internal_xattrs
        } else {
            ::metadata::strip_source_xattrs
        };
        strip(source, destination, false, Some(context.destination_root()))
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

/// Normalizes a clonefile'd destination's permissions to the `open()` default.
///
/// `clonefile()` preserves the source's exact mode. When the user has not
/// requested permission preservation (`--no-perms`),
/// `finalize_guard_and_metadata` skips the correction because it assumes the
/// file already has the process default, so this resets the mode to
/// `source_mode & ~umask` as `open()` would. Timestamps are not handled here:
/// the clone resets them through its anchored parent before returning.
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

    Ok(())
}
