//! Destination directory state checking and preparation.
//!
//! Inspects the destination path to determine whether it exists, is a directory,
//! or conflicts with the source type. Handles `--force` replacement of
//! non-directory destinations and records `--existing` / `--ignore-existing`
//! skip events.
use std::fs;
use std::io;
use std::path::Path;
use std::time::Duration;

use crate::existence_gate::{DestinationEntry, ExistenceSkip};
use crate::local_copy::{
    CopyContext, LocalCopyAction, LocalCopyError, LocalCopyMetadata, LocalCopyRecord,
    follow_symlink_metadata,
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
    /// The `--existing` / `--ignore-existing` gate skips this directory and,
    /// with it, the whole subtree.
    Skipped(ExistenceSkip),
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
/// - Destination exists but is not a directory: removes it and returns `Missing`
/// - Destination doesn't exist: returns `Missing`
/// - The existence gate skips the entry: returns `Skipped` before any removal
///
/// upstream: `generator.c:1839-1842` `recv_generator()` - a directory entry
/// that finds a non-directory in its place runs `delete_item(fname, ..,
/// del_opts | DEL_FOR_DIR)` and carries on. `--force` contributes only
/// `DEL_RECURSE` (`generator.c:1629`), which governs recursing into a
/// non-empty *directory*, so clearing a non-directory is unconditional.
/// `force_remove_destination` elides the removal under `--dry-run`.
#[inline]
pub(super) fn check_destination_state(
    context: &mut CopyContext,
    destination: &Path,
    relative: Option<&Path>,
) -> Result<DestinationState, LocalCopyError> {
    let existing = match fs::symlink_metadata(destination) {
        Ok(existing) => existing,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(
                gated(context, DestinationEntry::Missing).unwrap_or(DestinationState::Missing)
            );
        }
        Err(error) => {
            return Err(LocalCopyError::io(
                "inspect destination directory",
                destination.to_path_buf(),
                error,
            ));
        }
    };
    let file_type = existing.file_type();
    // upstream: generator.c:1745 gen_entry_stat() - `keep_dirlinks && is_dir`
    // follows a destination symlink, so one resolving to a directory is a
    // directory for the gate and is merged into.
    let directory = if file_type.is_dir() {
        Some(existing.clone())
    } else if file_type.is_symlink() && context.keep_dirlinks_enabled() {
        let target = follow_symlink_metadata(destination)?;
        target.file_type().is_dir().then_some(target)
    } else {
        None
    };
    let entry = if directory.is_some() {
        DestinationEntry::Directory
    } else {
        DestinationEntry::Other
    };
    if let Some(skipped) = gated(context, entry) {
        return Ok(skipped);
    }
    if let Some(directory) = directory {
        return Ok(DestinationState::Ready(Some(directory)));
    }
    context.force_remove_destination(destination, relative, &existing)?;
    Ok(DestinationState::Missing)
}

/// upstream: generator.c:1757-1806 - the existence gate reads the destination
/// `lstat` before `delete_item()` clears a non-directory obstacle, so
/// `--ignore-existing` leaves the obstacle standing and `--existing` merges
/// into it rather than treating the cleared path as absent.
fn gated(context: &CopyContext, entry: DestinationEntry) -> Option<DestinationState> {
    context
        .existence_skip(true, entry)
        .map(DestinationState::Skipped)
}

/// Records a directory skipped by the `--existing` / `--ignore-existing` gate.
#[inline]
pub(super) fn record_existence_skip(
    context: &mut CopyContext,
    skip: ExistenceSkip,
    metadata: &fs::Metadata,
    relative: Option<&Path>,
) {
    context.summary_mut().record_directory_total();
    if let Some(relative_path) = relative {
        let metadata_snapshot = LocalCopyMetadata::from_metadata(metadata, None);
        context.record(LocalCopyRecord::new(
            relative_path.to_path_buf(),
            match skip {
                ExistenceSkip::NotCreatingNew => LocalCopyAction::SkippedMissingDestination,
                ExistenceSkip::Exists => LocalCopyAction::SkippedExisting,
            },
            0,
            Some(metadata_snapshot.len()),
            Duration::default(),
            Some(metadata_snapshot),
        ));
    }
}
