//! Destination directory state checking and preparation.
//!
//! Inspects the destination path to determine whether it exists, is a directory,
//! or conflicts with the source type. Handles `--force` replacement of
//! non-directory destinations and records skip events for `--existing` mode.
use std::fs;
use std::io;
use std::path::Path;
use std::time::Duration;

use crate::local_copy::{
    CopyContext, LocalCopyAction, LocalCopyArgumentError, LocalCopyError, LocalCopyMetadata,
    LocalCopyRecord, follow_symlink_metadata,
};

/// Result of checking destination directory state.
#[derive(Debug, Clone)]
pub(super) enum DestinationState {
    /// Destination directory already exists and is ready. Carries the existing
    /// directory metadata so callers can itemize attribute drift (mtime, perms,
    /// ownership) against the source's metadata, matching upstream
    /// `generator.c:1480-1483` which feeds the existing `sx.st` into
    /// `itemize()` with `iflags=0` and lets `itemize()` compute the
    /// `ITEM_REPORT_TIME|PERMS|...` bits.
    Ready(Option<fs::Metadata>),
    /// Destination is missing and needs to be created.
    Missing,
}

impl DestinationState {
    /// Returns `true` when the destination needs to be materialised.
    pub(super) const fn is_missing(&self) -> bool {
        matches!(self, Self::Missing)
    }

    /// Returns the existing destination metadata when available.
    pub(super) fn existing_metadata(&self) -> Option<&fs::Metadata> {
        match self {
            Self::Ready(Some(meta)) => Some(meta),
            _ => None,
        }
    }
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
                Ok(DestinationState::Ready(Some(existing)))
            } else if file_type.is_symlink() && context.keep_dirlinks_enabled() {
                let target_metadata = follow_symlink_metadata(destination)?;
                if target_metadata.file_type().is_dir() {
                    Ok(DestinationState::Ready(Some(target_metadata)))
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
