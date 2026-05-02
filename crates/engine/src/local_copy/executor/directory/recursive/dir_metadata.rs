//! Final directory metadata application and completion recording.
//!
//! Applies ownership, permissions, timestamps, ACLs, and extended attributes
//! to directories after all their contents have been transferred.
//!
//! // upstream: receiver.c - directory metadata finalization after recv_files()
use std::fs;
use std::path::Path;

#[cfg(all(unix, any(feature = "acl", feature = "xattr")))]
use crate::local_copy::LocalCopyExecution;
#[cfg(all(any(unix, windows), feature = "acl"))]
use crate::local_copy::sync_acls_if_requested;
#[cfg(all(unix, feature = "xattr"))]
use crate::local_copy::sync_xattrs_if_requested;
use crate::local_copy::{CopyContext, LocalCopyError, LocalCopyRecord, map_metadata_error};
use ::metadata::apply_directory_metadata_with_options;

/// Applies final metadata to a directory after all contents have been processed.
///
/// This includes permissions, timestamps (unless omit_dir_times is enabled),
/// extended attributes, and ACLs.
pub(super) fn apply_final_directory_metadata(
    context: &CopyContext,
    source: &Path,
    destination: &Path,
    metadata: &fs::Metadata,
    #[cfg(all(unix, any(feature = "acl", feature = "xattr")))] mode: LocalCopyExecution,
    #[cfg(all(unix, feature = "xattr"))] preserve_xattrs: bool,
    #[cfg(all(any(unix, windows), feature = "acl"))] preserve_acls: bool,
) -> Result<(), LocalCopyError> {
    let metadata_options = if context.omit_dir_times_enabled() {
        context.metadata_options().preserve_times(false)
    } else {
        context.metadata_options()
    };
    apply_directory_metadata_with_options(destination, metadata, metadata_options)
        .map_err(map_metadata_error)?;

    #[cfg(all(unix, feature = "xattr"))]
    sync_xattrs_if_requested(
        preserve_xattrs,
        mode,
        source,
        destination,
        true,
        context.filter_program(),
    )?;

    #[cfg(all(any(unix, windows), feature = "acl"))]
    sync_acls_if_requested(preserve_acls, mode, source, destination, true)?;

    // Suppress unused variable warnings when features are disabled
    let _ = source;

    Ok(())
}

/// Records directory completion statistics and pending records.
#[inline]
pub(super) fn record_directory_completion(
    context: &mut CopyContext,
    creation_record_pending: bool,
    pending_record: Option<LocalCopyRecord>,
) {
    context.summary_mut().record_directory_total();
    if creation_record_pending {
        context.summary_mut().record_directory();
    }
    if let Some(record) = pending_record {
        context.record(record);
    }
}
