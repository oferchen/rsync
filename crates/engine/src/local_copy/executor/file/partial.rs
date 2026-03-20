//! Partial file management for interrupted transfers.
//!
//! This module implements rsync's `--partial` and `--partial-dir` functionality,
//! which controls what happens to partially transferred files on interruption
//! and how they are resumed on subsequent transfers.
//!
//! # Modes
//!
//! - **Delete (default)**: Temporary files are removed on failure
//! - **Keep (`--partial`)**: Partial files are kept in the same directory as the target
//! - **PartialDir (`--partial-dir=DIR`)**: Partial files are stored in a separate directory
//!
//! # Behavior
//!
//! Without `--partial`:
//! - Temp files use `.rsync-tmp-*` naming
//! - Deleted on transfer failure or interruption
//!
//! With `--partial`:
//! - Temp files use `.rsync-partial-*` naming
//! - Kept on failure, renamed to final destination
//! - On resume, used as basis for delta transfer
//!
//! With `--partial-dir=DIR`:
//! - Temp files stored in DIR/ with original filename
//! - DIR can be relative (per-destination) or absolute (global)
//! - Cleaned up on successful completion
//! - On resume, looked up in DIR/ for delta basis

use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::local_copy::LocalCopyError;

use super::paths::{partial_destination_path, partial_directory_destination_path};

/// Mode for handling partial file transfers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PartialMode {
    /// Delete temporary files on failure (default behavior).
    Delete,
    /// Keep partial files in the same directory as the destination.
    Keep,
    /// Store partial files in a separate directory.
    PartialDir(PathBuf),
}

impl PartialMode {
    /// Creates a `PartialMode` from options, checking the `RSYNC_PARTIAL_DIR` environment variable.
    ///
    /// Priority:
    /// 1. Explicit `partial_dir` parameter
    /// 2. `RSYNC_PARTIAL_DIR` environment variable (if `partial` is true)
    /// 3. `partial` flag alone
    /// 4. Default to `Delete`
    #[must_use]
    pub fn from_options(partial: bool, partial_dir: Option<PathBuf>) -> Self {
        if let Some(dir) = partial_dir {
            return Self::PartialDir(dir);
        }

        if partial {
            // Check environment variable
            if let Ok(env_dir) = env::var("RSYNC_PARTIAL_DIR") {
                if !env_dir.is_empty() {
                    return Self::PartialDir(PathBuf::from(env_dir));
                }
            }
            return Self::Keep;
        }

        Self::Delete
    }

    /// Returns `true` if partial files should be preserved on failure.
    #[must_use]
    pub const fn preserves_on_failure(&self) -> bool {
        matches!(self, Self::Keep | Self::PartialDir(_))
    }

    /// Returns `true` if this mode uses a separate partial directory.
    #[must_use]
    pub const fn uses_partial_dir(&self) -> bool {
        matches!(self, Self::PartialDir(_))
    }

    /// Returns the partial directory path if this mode uses one.
    #[must_use]
    pub fn partial_dir_path(&self) -> Option<&Path> {
        match self {
            Self::PartialDir(path) => Some(path),
            _ => None,
        }
    }
}

/// Manager for partial file operations during transfers.
///
/// This type provides a unified interface for:
/// - Finding basis files from previous partial transfers
/// - Determining temporary file locations
/// - Cleaning up partial files on success
///
/// # Examples
///
/// ```ignore
/// use engine::local_copy::executor::file::partial::{PartialMode, PartialFileManager};
/// use std::path::Path;
///
/// # fn example() -> Result<(), Box<dyn std::error::Error>> {
/// let mode = PartialMode::from_options(true, None);
/// let manager = PartialFileManager::new(mode);
///
/// let dest = Path::new("/data/file.txt");
///
/// // Check for basis file from previous partial transfer
/// if let Some(basis) = manager.find_basis(dest)? {
///     println!("Found partial basis: {}", basis.display());
/// }
///
/// // After successful transfer, clean up partial files
/// manager.cleanup_partial(dest)?;
/// # Ok(())
/// # }
/// ```
#[derive(Clone, Debug)]
pub struct PartialFileManager {
    mode: PartialMode,
}

impl PartialFileManager {
    /// Creates a new partial file manager with the given mode.
    #[must_use]
    pub const fn new(mode: PartialMode) -> Self {
        Self { mode }
    }

    /// Returns the mode used by this manager.
    #[must_use]
    pub const fn mode(&self) -> &PartialMode {
        &self.mode
    }

    /// Finds a basis file from a previous partial transfer.
    ///
    /// This searches for partial files that can be used as the basis for
    /// delta transfer when resuming an interrupted transfer.
    ///
    /// # Search locations
    ///
    /// - `PartialMode::Keep`: Looks for `.rsync-partial-<filename>` in the same directory
    /// - `PartialMode::PartialDir`: Looks for `<filename>` in the partial directory
    /// - `PartialMode::Delete`: Returns `None` (no partial support)
    ///
    /// # Errors
    ///
    /// Returns an error if I/O operations fail while checking for basis files.
    pub fn find_basis(&self, destination: &Path) -> Result<Option<PathBuf>, LocalCopyError> {
        match &self.mode {
            PartialMode::Delete => Ok(None),
            PartialMode::Keep => {
                let partial_path = partial_destination_path(destination);
                if partial_path.exists() {
                    Ok(Some(partial_path))
                } else {
                    Ok(None)
                }
            }
            PartialMode::PartialDir(dir) => {
                let partial_path = partial_directory_destination_path(destination, dir)?;
                if partial_path.exists() {
                    Ok(Some(partial_path))
                } else {
                    Ok(None)
                }
            }
        }
    }

    /// Cleans up partial files after a successful transfer.
    ///
    /// This removes partial files that are no longer needed after the
    /// destination file has been successfully created.
    ///
    /// # Behavior by mode
    ///
    /// - `PartialMode::Delete`: No-op (no partial files to clean)
    /// - `PartialMode::Keep`: Removes `.rsync-partial-<filename>` if it exists
    /// - `PartialMode::PartialDir`: Removes `<filename>` from partial directory
    ///
    /// # Errors
    ///
    /// Returns an error if the partial file exists but cannot be removed.
    /// If the partial file doesn't exist, this is not considered an error.
    pub fn cleanup_partial(&self, destination: &Path) -> Result<(), LocalCopyError> {
        match &self.mode {
            PartialMode::Delete => Ok(()),
            PartialMode::Keep => {
                let partial_path = partial_destination_path(destination);
                remove_if_exists(&partial_path)
            }
            PartialMode::PartialDir(dir) => {
                let partial_path = partial_directory_destination_path(destination, dir)?;
                remove_if_exists(&partial_path)
            }
        }
    }

    /// Returns the partial directory path if using `PartialMode::PartialDir`.
    #[must_use]
    pub fn partial_dir(&self) -> Option<&Path> {
        self.mode.partial_dir_path()
    }
}

/// Removes a file if it exists, succeeding if it doesn't exist.
fn remove_if_exists(path: &Path) -> Result<(), LocalCopyError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(LocalCopyError::io(
            "remove partial file",
            path.to_path_buf(),
            error,
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn partial_mode_from_options_default_is_delete() {
        let mode = PartialMode::from_options(false, None);
        assert_eq!(mode, PartialMode::Delete);
    }

    #[test]
    fn partial_mode_from_options_partial_flag_enables_keep() {
        let mode = PartialMode::from_options(true, None);
        assert_eq!(mode, PartialMode::Keep);
    }

    #[test]
    fn partial_mode_from_options_partial_dir_takes_precedence() {
        let dir = PathBuf::from("/tmp/partial");
        let mode = PartialMode::from_options(true, Some(dir.clone()));
        assert_eq!(mode, PartialMode::PartialDir(dir));
    }

    #[test]
    fn partial_mode_from_options_respects_env_var() {
        // Safety: This test is single-threaded and we restore the environment after
        unsafe {
            env::set_var("RSYNC_PARTIAL_DIR", "/tmp/env-partial");
        }
        let mode = PartialMode::from_options(true, None);
        unsafe {
            env::remove_var("RSYNC_PARTIAL_DIR");
        }

        assert_eq!(
            mode,
            PartialMode::PartialDir(PathBuf::from("/tmp/env-partial"))
        );
    }

    #[test]
    fn partial_mode_from_options_explicit_dir_overrides_env() {
        // Safety: This test is single-threaded and we restore the environment after
        unsafe {
            env::set_var("RSYNC_PARTIAL_DIR", "/tmp/env-partial");
        }
        let dir = PathBuf::from("/tmp/explicit");
        let mode = PartialMode::from_options(true, Some(dir.clone()));
        unsafe {
            env::remove_var("RSYNC_PARTIAL_DIR");
        }

        assert_eq!(mode, PartialMode::PartialDir(dir));
    }

    #[test]
    fn partial_mode_from_options_ignores_empty_env_var() {
        // Safety: This test is single-threaded and we restore the environment after
        unsafe {
            env::set_var("RSYNC_PARTIAL_DIR", "");
        }
        let mode = PartialMode::from_options(true, None);
        unsafe {
            env::remove_var("RSYNC_PARTIAL_DIR");
        }

        assert_eq!(mode, PartialMode::Keep);
    }

    #[test]
    fn partial_mode_preserves_on_failure() {
        assert!(!PartialMode::Delete.preserves_on_failure());
        assert!(PartialMode::Keep.preserves_on_failure());
        assert!(PartialMode::PartialDir(PathBuf::from("/tmp")).preserves_on_failure());
    }

    #[test]
    fn partial_mode_uses_partial_dir() {
        assert!(!PartialMode::Delete.uses_partial_dir());
        assert!(!PartialMode::Keep.uses_partial_dir());
        assert!(PartialMode::PartialDir(PathBuf::from("/tmp")).uses_partial_dir());
    }

    #[test]
    fn partial_mode_partial_dir_path() {
        assert_eq!(PartialMode::Delete.partial_dir_path(), None);
        assert_eq!(PartialMode::Keep.partial_dir_path(), None);
        let dir = PathBuf::from("/tmp/partial");
        assert_eq!(
            PartialMode::PartialDir(dir.clone()).partial_dir_path(),
            Some(dir.as_path())
        );
    }

    #[test]
    fn partial_file_manager_new_stores_mode() {
        let mode = PartialMode::Keep;
        let manager = PartialFileManager::new(mode.clone());
        assert_eq!(manager.mode(), &mode);
    }

    #[test]
    fn partial_file_manager_find_basis_delete_mode_returns_none() {
        let dir = tempdir().expect("tempdir");
        let dest = dir.path().join("file.txt");

        let manager = PartialFileManager::new(PartialMode::Delete);
        let basis = manager.find_basis(&dest).expect("find_basis");
        assert!(basis.is_none());
    }

    #[test]
    fn partial_file_manager_find_basis_keep_mode_finds_partial() {
        let dir = tempdir().expect("tempdir");
        let dest = dir.path().join("file.txt");
        let partial = dir.path().join(".rsync-partial-file.txt");

        // Create partial file
        fs::write(&partial, b"partial data").expect("write partial");

        let manager = PartialFileManager::new(PartialMode::Keep);
        let basis = manager.find_basis(&dest).expect("find_basis");
        assert_eq!(basis, Some(partial));
    }

    #[test]
    fn partial_file_manager_find_basis_keep_mode_returns_none_when_missing() {
        let dir = tempdir().expect("tempdir");
        let dest = dir.path().join("file.txt");

        let manager = PartialFileManager::new(PartialMode::Keep);
        let basis = manager.find_basis(&dest).expect("find_basis");
        assert!(basis.is_none());
    }

    #[test]
    fn partial_file_manager_find_basis_partial_dir_mode_finds_file() {
        let dir = tempdir().expect("tempdir");
        let partial_dir = dir.path().join(".rsync-partial");
        fs::create_dir(&partial_dir).expect("create partial dir");

        let dest = dir.path().join("file.txt");
        let partial = partial_dir.join("file.txt");

        // Create partial file in partial dir
        fs::write(&partial, b"partial data").expect("write partial");

        let manager = PartialFileManager::new(PartialMode::PartialDir(partial_dir));
        let basis = manager.find_basis(&dest).expect("find_basis");
        assert_eq!(basis, Some(partial));
    }

    #[test]
    fn partial_file_manager_find_basis_partial_dir_mode_returns_none_when_missing() {
        let dir = tempdir().expect("tempdir");
        let partial_dir = dir.path().join(".rsync-partial");

        let dest = dir.path().join("file.txt");

        let manager = PartialFileManager::new(PartialMode::PartialDir(partial_dir));
        let basis = manager.find_basis(&dest).expect("find_basis");
        assert!(basis.is_none());
    }

    #[test]
    fn partial_file_manager_find_basis_partial_dir_absolute() {
        let base_dir = tempdir().expect("tempdir");
        let partial_dir = base_dir.path().join("global-partial");
        fs::create_dir(&partial_dir).expect("create partial dir");

        // Destination in different directory
        let dest_dir = base_dir.path().join("data");
        fs::create_dir(&dest_dir).expect("create dest dir");
        let dest = dest_dir.join("file.txt");

        // Partial file in absolute partial dir
        let partial = partial_dir.join("file.txt");
        fs::write(&partial, b"partial data").expect("write partial");

        let manager = PartialFileManager::new(PartialMode::PartialDir(partial_dir));
        let basis = manager.find_basis(&dest).expect("find_basis");
        assert_eq!(basis, Some(partial));
    }

    #[test]
    fn partial_file_manager_find_basis_partial_dir_relative() {
        let base_dir = tempdir().expect("tempdir");
        let dest_dir = base_dir.path().join("data");
        fs::create_dir(&dest_dir).expect("create dest dir");

        // Relative partial dir (.partial relative to dest directory)
        let partial_dir_rel = PathBuf::from(".partial");
        let partial_dir_abs = dest_dir.join(&partial_dir_rel);
        fs::create_dir(&partial_dir_abs).expect("create partial dir");

        let dest = dest_dir.join("file.txt");
        let partial = partial_dir_abs.join("file.txt");
        fs::write(&partial, b"partial data").expect("write partial");

        let manager = PartialFileManager::new(PartialMode::PartialDir(partial_dir_rel));
        let basis = manager.find_basis(&dest).expect("find_basis");
        assert_eq!(basis, Some(partial));
    }

    #[test]
    fn partial_file_manager_cleanup_delete_mode_is_noop() {
        let dir = tempdir().expect("tempdir");
        let dest = dir.path().join("file.txt");

        let manager = PartialFileManager::new(PartialMode::Delete);
        let result = manager.cleanup_partial(&dest);
        assert!(result.is_ok());
    }

    #[test]
    fn partial_file_manager_cleanup_keep_mode_removes_partial() {
        let dir = tempdir().expect("tempdir");
        let dest = dir.path().join("file.txt");
        let partial = dir.path().join(".rsync-partial-file.txt");

        // Create partial file
        fs::write(&partial, b"partial data").expect("write partial");
        assert!(partial.exists());

        let manager = PartialFileManager::new(PartialMode::Keep);
        manager.cleanup_partial(&dest).expect("cleanup");

        // Partial should be removed
        assert!(!partial.exists());
    }

    #[test]
    fn partial_file_manager_cleanup_keep_mode_succeeds_when_missing() {
        let dir = tempdir().expect("tempdir");
        let dest = dir.path().join("file.txt");

        let manager = PartialFileManager::new(PartialMode::Keep);
        let result = manager.cleanup_partial(&dest);
        assert!(result.is_ok());
    }

    #[test]
    fn partial_file_manager_cleanup_partial_dir_mode_removes_file() {
        let dir = tempdir().expect("tempdir");
        let partial_dir = dir.path().join(".rsync-partial");
        fs::create_dir(&partial_dir).expect("create partial dir");

        let dest = dir.path().join("file.txt");
        let partial = partial_dir.join("file.txt");

        // Create partial file
        fs::write(&partial, b"partial data").expect("write partial");
        assert!(partial.exists());

        let manager = PartialFileManager::new(PartialMode::PartialDir(partial_dir));
        manager.cleanup_partial(&dest).expect("cleanup");

        // Partial should be removed
        assert!(!partial.exists());
    }

    #[test]
    fn partial_file_manager_cleanup_partial_dir_mode_succeeds_when_missing() {
        let dir = tempdir().expect("tempdir");
        let partial_dir = dir.path().join(".rsync-partial");

        let dest = dir.path().join("file.txt");

        let manager = PartialFileManager::new(PartialMode::PartialDir(partial_dir));
        let result = manager.cleanup_partial(&dest);
        assert!(result.is_ok());
    }

    #[test]
    fn partial_file_manager_partial_dir_returns_path() {
        let dir = PathBuf::from("/tmp/partial");
        let manager = PartialFileManager::new(PartialMode::PartialDir(dir.clone()));
        assert_eq!(manager.partial_dir(), Some(dir.as_path()));
    }

    #[test]
    fn partial_file_manager_partial_dir_returns_none_for_other_modes() {
        let manager = PartialFileManager::new(PartialMode::Delete);
        assert_eq!(manager.partial_dir(), None);

        let manager = PartialFileManager::new(PartialMode::Keep);
        assert_eq!(manager.partial_dir(), None);
    }

    #[test]
    fn remove_if_exists_removes_file() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("test.txt");

        fs::write(&path, b"content").expect("write");
        assert!(path.exists());

        remove_if_exists(&path).expect("remove");
        assert!(!path.exists());
    }

    #[test]
    fn remove_if_exists_succeeds_when_missing() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("nonexistent.txt");

        let result = remove_if_exists(&path);
        assert!(result.is_ok());
    }

    #[test]
    fn partial_file_manager_handles_nested_directories() {
        let base_dir = tempdir().expect("tempdir");
        let partial_dir = base_dir.path().join(".partial");
        fs::create_dir(&partial_dir).expect("create partial dir");

        let dest_dir = base_dir.path().join("a").join("b").join("c");
        fs::create_dir_all(&dest_dir).expect("create nested dirs");
        let dest = dest_dir.join("file.txt");

        // Partial dir should be created relative to destination directory
        let expected_partial_dir = dest_dir.join(".partial");
        fs::create_dir(&expected_partial_dir).expect("create nested partial dir");
        let partial = expected_partial_dir.join("file.txt");
        fs::write(&partial, b"nested partial").expect("write partial");

        let manager = PartialFileManager::new(PartialMode::PartialDir(PathBuf::from(".partial")));
        let basis = manager.find_basis(&dest).expect("find_basis");
        assert_eq!(basis, Some(partial));
    }
}
