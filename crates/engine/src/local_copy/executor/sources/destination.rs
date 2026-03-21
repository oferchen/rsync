//! Destination state queries and target path computation.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::local_copy::{LocalCopyArgumentError, LocalCopyError, LocalCopyExecution};

use super::super::follow_symlink_metadata;
use super::types::DestinationState;

/// Queries the filesystem to determine destination state.
pub(crate) fn query_destination_state(path: &Path) -> Result<DestinationState, LocalCopyError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            let file_type = metadata.file_type();
            let symlink_to_dir = if file_type.is_symlink() {
                follow_symlink_metadata(path)
                    .map(|target| target.file_type().is_dir())
                    .unwrap_or(false)
            } else {
                false
            };

            Ok(DestinationState {
                exists: true,
                is_dir: file_type.is_dir(),
                symlink_to_dir,
            })
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(DestinationState::default()),
        Err(error) => Err(LocalCopyError::io(
            "inspect destination",
            path.to_path_buf(),
            error,
        )),
    }
}

/// Ensures the destination path is a directory, creating it if necessary.
pub(crate) fn ensure_destination_directory(
    destination_path: &Path,
    state: &mut DestinationState,
    mode: LocalCopyExecution,
) -> Result<(), LocalCopyError> {
    if state.exists {
        if !state.is_dir {
            return Err(LocalCopyError::invalid_argument(
                LocalCopyArgumentError::DestinationMustBeDirectory,
            ));
        }
        return Ok(());
    }

    if mode.is_dry_run() {
        state.exists = true;
        state.is_dir = true;
        return Ok(());
    }

    fs::create_dir_all(destination_path).map_err(|error| {
        LocalCopyError::io(
            "create destination directory",
            destination_path.to_path_buf(),
            error,
        )
    })?;
    state.exists = true;
    state.is_dir = true;
    Ok(())
}

/// Computes the target path for a non-directory entry.
pub(super) fn compute_target_path(
    destination_path: &Path,
    destination_base: &Path,
    name: &std::ffi::OsStr,
    destination_behaves_like_directory: bool,
    prefer_root_destination: bool,
    is_directory: bool,
) -> PathBuf {
    if destination_behaves_like_directory && (!prefer_root_destination || is_directory) {
        destination_base.join(name)
    } else {
        destination_path.to_path_buf()
    }
}

/// Computes the target path for special entries (symlinks, FIFOs, devices).
///
/// These entries don't use the directory-specific logic that regular files use.
pub(super) fn compute_special_target_path(
    destination_path: &Path,
    destination_base: &Path,
    name: &std::ffi::OsStr,
    destination_behaves_like_directory: bool,
    prefer_root_destination: bool,
) -> PathBuf {
    if destination_behaves_like_directory && !prefer_root_destination {
        destination_base.join(name)
    } else {
        destination_path.to_path_buf()
    }
}
