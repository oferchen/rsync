//! Destination state queries and target path computation.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use protocol::iconv::FilenameConverter;

use crate::local_copy::{LocalCopyArgumentError, LocalCopyError, LocalCopyExecution};

use super::super::follow_symlink_metadata;
use super::super::transcode_filename_component;
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
///
/// Returns `true` when this call materialised the destination directory,
/// `false` when it was already present. upstream: main.c:798-799 - the
/// generator emits `created directory %s` only when the pre-flight mkdir
/// actually created the dest; subsequent runs against the same destination
/// must remain silent.
///
/// The `mkpath` argument selects how many components may be created, mirroring
/// upstream `get_local_name()`: without `--mkpath` a single `do_mkdir(dest_path)`
/// creates only the final component and a missing ancestor is a fatal ENOENT
/// (`main.c:787,796`), whereas `--mkpath` first runs `make_path(dest_path, ...)`
/// to build the whole leading chain (`main.c:736`).
pub(crate) fn ensure_destination_directory(
    destination_path: &Path,
    state: &mut DestinationState,
    mode: LocalCopyExecution,
    mkpath: bool,
) -> Result<bool, LocalCopyError> {
    if state.exists {
        if !state.is_dir {
            return Err(LocalCopyError::invalid_argument(
                LocalCopyArgumentError::DestinationMustBeDirectory,
            ));
        }
        return Ok(false);
    }

    if mode.is_dry_run() {
        state.exists = true;
        state.is_dir = true;
        return Ok(true);
    }

    // upstream: main.c:736 vs main.c:787 - `--mkpath` runs `make_path()` (full
    // chain) while the plain path does a single `do_mkdir(dest_path)` that fails
    // with ENOENT when a leading directory is absent.
    let outcome = if mkpath {
        fs::create_dir_all(destination_path)
    } else {
        fs::create_dir(destination_path)
    };
    outcome.map_err(|error| {
        LocalCopyError::io(
            "create destination directory",
            destination_path.to_path_buf(),
            error,
        )
    })?;
    state.exists = true;
    state.is_dir = true;
    Ok(true)
}

/// Computes the target path for a non-directory entry.
///
/// When an `--iconv` converter is supplied, the source filename component
/// `name` is transcoded with [`transcode_filename_component`] before being
/// appended to `destination_base`. This mirrors upstream rsync's
/// `flist.c:1579-1603` (sender) + `flist.c:738-754` (receiver) composition
/// in local-copy mode (`rsync.c:118-140`).
pub(super) fn compute_target_path(
    destination_path: &Path,
    destination_base: &Path,
    name: &std::ffi::OsStr,
    destination_behaves_like_directory: bool,
    prefer_root_destination: bool,
    is_directory: bool,
    iconv: Option<&FilenameConverter>,
) -> PathBuf {
    if destination_behaves_like_directory && (!prefer_root_destination || is_directory) {
        let converted = transcode_filename_component(name, iconv);
        destination_base.join(Path::new(&*converted))
    } else {
        destination_path.to_path_buf()
    }
}

/// Computes the target path for special entries (symlinks, FIFOs, devices).
///
/// These entries don't use the directory-specific logic that regular files use.
/// The `iconv` parameter applies the same LOCAL -> REMOTE transcoding the
/// per-directory path uses; see [`compute_target_path`].
pub(super) fn compute_special_target_path(
    destination_path: &Path,
    destination_base: &Path,
    name: &std::ffi::OsStr,
    destination_behaves_like_directory: bool,
    prefer_root_destination: bool,
    iconv: Option<&FilenameConverter>,
) -> PathBuf {
    if destination_behaves_like_directory && !prefer_root_destination {
        let converted = transcode_filename_component(name, iconv);
        destination_base.join(Path::new(&*converted))
    } else {
        destination_path.to_path_buf()
    }
}
