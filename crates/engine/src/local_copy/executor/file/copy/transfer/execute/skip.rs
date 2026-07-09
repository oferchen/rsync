//! Skip-detection and metadata-only completion paths.
//!
//! Houses the two helpers that allow `execute_transfer` to bail out before
//! opening the source for read: the quick-check comparator and the metadata
//! reuse recorder. Both paths run when the source and destination already
//! hold identical content, so the only remaining work is metadata sync and
//! event recording.

use std::fs;
use std::path::Path;
use std::time::Duration;

use logging::debug_log;

use ::metadata::MetadataOptions;

#[cfg(all(any(unix, windows), feature = "acl"))]
use crate::local_copy::sync_acls_if_requested;
#[cfg(all(unix, feature = "xattr"))]
use crate::local_copy::sync_xattrs_if_requested;

use crate::local_copy::{
    CopyContext, LocalCopyAction, LocalCopyChangeSet, LocalCopyError, LocalCopyExecution,
    LocalCopyMetadata, LocalCopyRecord,
};

use super::super::super::super::comparison::{
    CopyComparison, files_checksum_match, should_skip_copy,
};
use super::super::TransferFlags;

/// Checks if the destination is already up-to-date and can be skipped.
///
/// When the file is in-sync, applies metadata and records a `MetadataReused`
/// action. Returns `true` if the transfer should be skipped.
#[allow(clippy::too_many_arguments)]
pub(super) fn try_skip_up_to_date(
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

    record_metadata_only_skip(
        context,
        source,
        destination,
        metadata,
        metadata_options,
        record_path,
        existing,
        flags,
        mode,
        "already up-to-date",
    )?;

    Ok(true)
}

/// Records a metadata-only skip outcome and applies destination metadata.
///
/// Used by both the up-to-date quick check and the xxh64 dedup heuristic.
/// The caller has already established that the source and destination are
/// content-identical, so the only remaining work is to sync metadata,
/// xattrs, ACLs, and emit the `MetadataReused` event.
#[allow(clippy::too_many_arguments)]
pub(super) fn record_metadata_only_skip(
    context: &mut CopyContext,
    #[allow(unused_variables)] // REASON: used on unix with feature "xattr"
    source: &Path,
    destination: &Path,
    metadata: &fs::Metadata,
    metadata_options: &MetadataOptions,
    record_path: &Path,
    existing: &fs::Metadata,
    flags: &TransferFlags,
    #[allow(unused_variables)] // REASON: used on unix with feature "xattr"
    mode: LocalCopyExecution,
    reason: &str,
) -> Result<(), LocalCopyError> {
    debug_log!(
        Deltasum,
        2,
        "skipping {}: {}",
        record_path.display(),
        reason
    );
    // upstream: rsync.c:672-676 - rprintf(FCLIENT, "%s is uptodate\n", fname)
    // at INFO_GTE(NAME, 2) when a quick-check or content-equal path reuses
    // the existing destination instead of transferring. The CLI renders this
    // line from the MetadataReused event so it lands ahead of the summary
    // (matching upstream's in-line FCLIENT emission); the diagnostic queue
    // drains after the summary, so emitting via info_log! here would race the
    // banner+totals ordering. Keep the record-based render as the single
    // source of truth.
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
    #[cfg(all(any(unix, windows), feature = "acl"))]
    sync_acls_if_requested(flags.preserve_acls, mode, source, destination, true)?;

    context.record_hard_link(metadata, destination);
    context.summary_mut().record_regular_file_matched();
    let metadata_snapshot = LocalCopyMetadata::from_metadata(metadata, None)
        .virtualize_fake_super(source, metadata_options.fake_super_enabled());
    let total_bytes = Some(metadata_snapshot.len());
    let change_set = LocalCopyChangeSet::for_file(
        metadata,
        Some(existing),
        metadata_options,
        true,
        false,
        flags.xattrs_enabled(),
        flags.acls_enabled(),
        context.options().modify_window(),
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

    Ok(())
}
