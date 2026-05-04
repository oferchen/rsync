//! Guard commit and metadata finalization for file transfers.
//!
//! After a file transfer completes, the destination must be finalized: the
//! staging guard is either committed (renamed to the final path) or deferred
//! for batch updates, and file metadata is applied. This module handles both
//! the guarded (temp-file) and unguarded (inplace/direct) code paths.

use std::fs;
use std::path::Path;

use logging::debug_log;

use ::metadata::MetadataOptions;

use crate::local_copy::{
    CopyContext, DeferredUpdate, FinalizeMetadataParams, LocalCopyError, LocalCopyExecution,
    MetadataPathContext, OwnedPathContext,
};

use super::super::super::guard::DestinationWriteGuard;

/// Commits or defers a destination guard, applying metadata as appropriate.
///
/// When `delay_updates_enabled` is true, the guard is registered for deferred
/// commit. Otherwise the guard is committed immediately and metadata is applied.
/// If no guard is present (inplace path), metadata is applied directly to
/// `destination`.
#[allow(clippy::too_many_arguments)]
pub(in crate::local_copy) fn finalize_guard_and_metadata(
    context: &mut CopyContext,
    guard: Option<DestinationWriteGuard>,
    destination: &Path,
    metadata: &fs::Metadata,
    metadata_options: MetadataOptions,
    mode: LocalCopyExecution,
    source: &Path,
    record_path: &Path,
    relative: Option<&Path>,
    file_type: fs::FileType,
    destination_previously_existed: bool,
    delay_updates_enabled: bool,
    writer_for_metadata: &mut Option<fs::File>,
    #[cfg(all(unix, feature = "xattr"))] preserve_xattrs: bool,
    #[cfg(all(any(unix, windows), feature = "acl"))] preserve_acls: bool,
) -> Result<(), LocalCopyError> {
    let relative_for_removal = Some(record_path.to_path_buf());
    if let Some(guard) = guard {
        if delay_updates_enabled {
            drop(writer_for_metadata.take());
            let destination_path = guard.final_path().to_path_buf();
            let update = DeferredUpdate::new(
                guard,
                metadata.clone(),
                metadata_options,
                mode,
                OwnedPathContext {
                    source: source.to_path_buf(),
                    relative: relative_for_removal,
                    file_type,
                    destination_previously_existed,
                },
                destination_path,
                #[cfg(all(unix, feature = "xattr"))]
                preserve_xattrs,
                #[cfg(all(any(unix, windows), feature = "acl"))]
                preserve_acls,
            );
            context.register_deferred_update(update);
        } else {
            let destination_path = guard.final_path().to_path_buf();
            debug_log!(
                Io,
                3,
                "renaming temp file to {}",
                destination_path.display()
            );
            guard.commit()?;
            #[allow(unused_mut)] // REASON: mutated on unix for with_fd()
            let mut params = FinalizeMetadataParams::new(
                metadata,
                metadata_options,
                mode,
                MetadataPathContext {
                    source,
                    relative: relative_for_removal.as_deref(),
                    file_type,
                    destination_previously_existed,
                },
                #[cfg(all(unix, feature = "xattr"))]
                preserve_xattrs,
                #[cfg(all(any(unix, windows), feature = "acl"))]
                preserve_acls,
            );
            #[cfg(unix)]
            if let Some(ref w) = *writer_for_metadata {
                use std::os::fd::AsFd;
                params = params.with_fd(w.as_fd());
            }
            context.apply_metadata_and_finalize(destination_path.as_path(), params)?;
            drop(writer_for_metadata.take());
        }
    } else {
        #[allow(unused_mut)] // REASON: mutated on unix for with_fd()
        let mut params = FinalizeMetadataParams::new(
            metadata,
            metadata_options,
            mode,
            MetadataPathContext {
                source,
                relative,
                file_type,
                destination_previously_existed,
            },
            #[cfg(all(unix, feature = "xattr"))]
            preserve_xattrs,
            #[cfg(all(any(unix, windows), feature = "acl"))]
            preserve_acls,
        );
        #[cfg(unix)]
        if let Some(ref w) = *writer_for_metadata {
            use std::os::fd::AsFd;
            params = params.with_fd(w.as_fd());
        }
        context.apply_metadata_and_finalize(destination, params)?;
        drop(writer_for_metadata.take());
    }
    Ok(())
}
