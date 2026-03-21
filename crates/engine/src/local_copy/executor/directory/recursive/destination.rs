/// Destination directory state checking and preparation.
use std::fs;
use std::io;
use std::path::Path;
use std::time::Duration;

use crate::local_copy::{
    CopyContext, LocalCopyAction, LocalCopyArgumentError, LocalCopyError, LocalCopyMetadata,
    LocalCopyRecord, follow_symlink_metadata,
};

/// Result of checking destination directory state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DestinationState {
    /// Destination directory already exists and is ready.
    Ready,
    /// Destination is missing and needs to be created.
    Missing,
}

/// Checks the destination path and determines if it needs to be created.
///
/// Handles various cases:
/// - Destination is already a directory: returns `Ready`
/// - Destination is a symlink to a directory with `--keep-dirlinks`: returns `Ready`
/// - Destination exists but is not a directory: removes it if force is enabled
/// - Destination doesn't exist: returns `Missing`
#[inline]
pub(super) fn check_destination_state(
    context: &mut CopyContext,
    destination: &Path,
    relative: Option<&Path>,
) -> Result<DestinationState, LocalCopyError> {
    match fs::symlink_metadata(destination) {
        Ok(existing) => {
            let file_type = existing.file_type();
            if file_type.is_dir() {
                // Directory already present; nothing to do.
                Ok(DestinationState::Ready)
            } else if file_type.is_symlink() && context.keep_dirlinks_enabled() {
                let target_metadata = follow_symlink_metadata(destination)?;
                if target_metadata.file_type().is_dir() {
                    Ok(DestinationState::Ready)
                } else if context.force_replacements_enabled() {
                    context.force_remove_destination(destination, relative, &existing)?;
                    Ok(DestinationState::Missing)
                } else {
                    Err(LocalCopyError::invalid_argument(
                        LocalCopyArgumentError::ReplaceNonDirectoryWithDirectory,
                    ))
                }
            } else if context.force_replacements_enabled() {
                context.force_remove_destination(destination, relative, &existing)?;
                Ok(DestinationState::Missing)
            } else {
                Err(LocalCopyError::invalid_argument(
                    LocalCopyArgumentError::ReplaceNonDirectoryWithDirectory,
                ))
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(DestinationState::Missing),
        Err(error) => Err(LocalCopyError::io(
            "inspect destination directory",
            destination.to_path_buf(),
            error,
        )),
    }
}

/// Records that a directory was skipped because existing_only mode is enabled
/// and the destination doesn't exist.
#[inline]
pub(super) fn record_skipped_missing_destination(
    context: &mut CopyContext,
    metadata: &fs::Metadata,
    relative: Option<&Path>,
) {
    context.summary_mut().record_directory_total();
    if let Some(relative_path) = relative {
        let metadata_snapshot = LocalCopyMetadata::from_metadata(metadata, None);
        context.record(LocalCopyRecord::new(
            relative_path.to_path_buf(),
            LocalCopyAction::SkippedMissingDestination,
            0,
            Some(metadata_snapshot.len()),
            Duration::default(),
            Some(metadata_snapshot),
        ));
    }
}
