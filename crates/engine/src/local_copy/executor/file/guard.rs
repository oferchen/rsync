use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering as AtomicOrdering;

use crate::local_copy::LocalCopyError;

use super::super::super::NEXT_TEMP_FILE_ID;
use super::paths::{
    partial_destination_path, partial_directory_destination_path, temporary_destination_path,
};

/// Removes an existing destination file.
///
/// This function removes a file at the given path. If the file does not exist,
/// the function succeeds without error. This is useful for cleanup operations
/// where the file may or may not exist.
///
/// # Errors
///
/// Returns an error if the file exists but cannot be removed due to permissions
/// or other I/O errors.
pub fn remove_existing_destination(path: &Path) -> Result<(), LocalCopyError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(LocalCopyError::io(
            "remove existing destination",
            path.to_path_buf(),
            error,
        )),
    }
}

/// Removes an incomplete destination file, ignoring errors.
///
/// This function attempts to remove a file that represents an incomplete transfer.
/// Unlike [`remove_existing_destination`], this function silently ignores all errors
/// including permission errors, as it's typically called during error recovery where
/// the original error should be preserved.
pub fn remove_incomplete_destination(destination: &Path) {
    if let Err(error) = fs::remove_file(destination)
        && error.kind() != io::ErrorKind::NotFound
    {
        // Preserve the original error from the transfer attempt.
    }
}

/// A guard for atomic file writes via temporary files.
///
/// This type provides atomic file updates by writing to a temporary file and then
/// renaming it to the final destination on commit. If the guard is dropped without
/// calling [`commit`](Self::commit), the temporary file is automatically cleaned up
/// (unless in partial mode).
///
/// # Modes
///
/// - **Normal mode** (`partial = false`): Temporary files are created with a unique
///   name (including process ID and counter) and are automatically cleaned up on failure.
/// - **Partial mode** (`partial = true`): Temporary files are preserved on failure to
///   allow for transfer resumption. These files use the `.rsync-partial-` prefix.
///
/// # Example
///
/// ```no_run
/// use engine::local_copy::DestinationWriteGuard;
/// use std::io::Write;
/// use std::path::Path;
///
/// # fn example() -> Result<(), Box<dyn std::error::Error>> {
/// let dest = Path::new("output.txt");
/// let (guard, mut file) = DestinationWriteGuard::new(dest, false, None, None)?;
///
/// file.write_all(b"Hello, world!")?;
/// drop(file);
///
/// guard.commit()?;
/// # Ok(())
/// # }
/// ```
pub struct DestinationWriteGuard {
    final_path: PathBuf,
    temp_path: PathBuf,
    preserve_on_error: bool,
    committed: bool,
}

impl DestinationWriteGuard {
    /// Creates a new write guard with an associated temporary file.
    ///
    /// This function creates a temporary file for writing and returns both the guard
    /// and an open file handle. The temporary file is created in the same directory as
    /// the destination (or in `temp_dir` if provided) to ensure atomic rename operations.
    ///
    /// # Arguments
    ///
    /// * `destination` - The final destination path for the file
    /// * `partial` - If `true`, creates a partial file that is preserved on failure
    /// * `partial_dir` - Optional directory for partial files (only used if `partial` is `true`)
    /// * `temp_dir` - Optional directory for temporary files (only used if `partial` is `false`)
    ///
    /// # Returns
    ///
    /// Returns a tuple of `(DestinationWriteGuard, File)` where the file is open for writing.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The temporary file cannot be created
    /// - The destination directory does not exist
    /// - Permission is denied
    pub fn new(
        destination: &Path,
        partial: bool,
        partial_dir: Option<&Path>,
        temp_dir: Option<&Path>,
    ) -> Result<(Self, fs::File), LocalCopyError> {
        if partial {
            let temp_path = if let Some(dir) = partial_dir {
                partial_directory_destination_path(destination, dir)?
            } else {
                partial_destination_path(destination)
            };
            if let Err(error) = fs::remove_file(&temp_path)
                && error.kind() != io::ErrorKind::NotFound
            {
                return Err(LocalCopyError::io(
                    "remove existing partial file",
                    temp_path,
                    error,
                ));
            }
            let file = fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&temp_path)
                .map_err(|error| LocalCopyError::io("copy file", temp_path.clone(), error))?;
            Ok((
                Self {
                    final_path: destination.to_path_buf(),
                    temp_path,
                    preserve_on_error: true,
                    committed: false,
                },
                file,
            ))
        } else {
            loop {
                let unique = NEXT_TEMP_FILE_ID.fetch_add(1, AtomicOrdering::Relaxed);
                let temp_path = temporary_destination_path(destination, unique, temp_dir);
                match fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&temp_path)
                {
                    Ok(file) => {
                        return Ok((
                            Self {
                                final_path: destination.to_path_buf(),
                                temp_path,
                                preserve_on_error: false,
                                committed: false,
                            },
                            file,
                        ));
                    }
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                        continue;
                    }
                    Err(error) => {
                        return Err(LocalCopyError::io("copy file", temp_path, error));
                    }
                }
            }
        }
    }

    /// Returns the path to the staging (temporary) file.
    ///
    /// This path can be used to access or modify the temporary file directly
    /// before it is committed to the final destination.
    pub fn staging_path(&self) -> &Path {
        &self.temp_path
    }

    /// Commits the temporary file to the final destination.
    ///
    /// This method atomically moves the temporary file to the final destination path
    /// using rename operations when possible. If the rename fails due to crossing
    /// filesystem boundaries, it falls back to copy-and-delete.
    ///
    /// If the destination already exists, it is removed before the rename.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The rename or copy operation fails
    /// - The destination cannot be removed
    /// - Permission is denied
    pub fn commit(mut self) -> Result<(), LocalCopyError> {
        // upstream: util1.c:robust_rename() — retry up to 4 times on ETXTBSY
        let mut tries = 4u32;
        loop {
            match fs::rename(&self.temp_path, &self.final_path) {
                Ok(()) => break,
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    remove_existing_destination(&self.final_path)?;
                    fs::rename(&self.temp_path, &self.final_path).map_err(|rename_error| {
                        LocalCopyError::io(
                            self.finalise_action(),
                            self.temp_path.clone(),
                            rename_error,
                        )
                    })?;
                    break;
                }
                Err(error) if error.kind() == io::ErrorKind::ExecutableFileBusy => {
                    tries -= 1;
                    if tries == 0 {
                        return Err(LocalCopyError::io(
                            self.finalise_action(),
                            self.temp_path.clone(),
                            error,
                        ));
                    }
                    remove_existing_destination(&self.final_path)?;
                }
                Err(error) if error.kind() == io::ErrorKind::CrossesDevices => {
                    fs::copy(&self.temp_path, &self.final_path).map_err(|copy_error| {
                        LocalCopyError::io(
                            self.finalise_action(),
                            self.final_path.clone(),
                            copy_error,
                        )
                    })?;
                    fs::remove_file(&self.temp_path).map_err(|remove_error| {
                        LocalCopyError::io(
                            self.finalise_action(),
                            self.temp_path.clone(),
                            remove_error,
                        )
                    })?;
                    break;
                }
                Err(error) => {
                    return Err(LocalCopyError::io(
                        self.finalise_action(),
                        self.temp_path.clone(),
                        error,
                    ));
                }
            }
        }
        self.committed = true;
        Ok(())
    }

    /// Returns the final destination path.
    ///
    /// This is the path where the file will be located after calling [`commit`](Self::commit).
    pub fn final_path(&self) -> &Path {
        &self.final_path
    }

    /// Discards the temporary file without committing.
    ///
    /// In normal mode, this removes the temporary file. In partial mode, the file
    /// is preserved to allow for transfer resumption with an ancient mtime so
    /// that `--update` will not skip it on retry.
    ///
    /// This method consumes the guard, preventing accidental use after discard.
    pub fn discard(mut self) {
        if self.preserve_on_error {
            // upstream: receiver.c — set mtime to epoch 0 on partial files so
            // --update won't skip them during a subsequent retry.
            let epoch = std::time::SystemTime::UNIX_EPOCH;
            let times = fs::FileTimes::new().set_modified(epoch);
            if let Ok(file) = fs::File::options().write(true).open(&self.temp_path) {
                let _ = file.set_times(times);
            }
            self.committed = true;
            return;
        }

        if let Err(error) = fs::remove_file(&self.temp_path)
            && error.kind() != io::ErrorKind::NotFound
        {
            // Best-effort cleanup: the file may have been removed concurrently.
        }

        self.committed = true;
    }

    const fn finalise_action(&self) -> &'static str {
        if self.preserve_on_error {
            "finalise partial file"
        } else {
            "finalise temporary file"
        }
    }
}

impl Drop for DestinationWriteGuard {
    fn drop(&mut self) {
        if !self.committed && !self.preserve_on_error {
            let _ = fs::remove_file(&self.temp_path);
        }
    }
}

/// A guard for direct file writes without a temporary file.
///
/// Used for new file creation where no existing file needs atomic protection.
/// If the guard is dropped without calling [`commit`](Self::commit), the
/// destination file is removed — matching the cleanup behavior of
/// [`DestinationWriteGuard`] and upstream rsync's signal-handler cleanup.
///
/// This avoids the overhead of creating a temporary file and renaming it,
/// while still ensuring partial files are cleaned up on error, panic, or
/// signal-induced process termination.
pub struct DirectWriteGuard {
    path: PathBuf,
    committed: bool,
}

impl DirectWriteGuard {
    /// Creates a new direct-write guard for the given destination path.
    ///
    /// The file at `path` should already be open for writing. The guard
    /// takes ownership of cleanup responsibility.
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            committed: false,
        }
    }

    /// Marks the write as successfully completed.
    ///
    /// After calling this method, the guard will not remove the destination
    /// file on drop.
    pub fn commit(&mut self) {
        self.committed = true;
    }

    /// Returns the destination path.
    #[cfg(test)]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for DirectWriteGuard {
    fn drop(&mut self) {
        if !self.committed {
            let _ = fs::remove_file(&self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::tempdir;

    #[test]
    fn remove_existing_destination_removes_file() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("test.txt");
        fs::write(&path, b"content").expect("write file");

        let result = remove_existing_destination(&path);
        assert!(result.is_ok());
        assert!(!path.exists());
    }

    #[test]
    fn remove_existing_destination_succeeds_when_not_found() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("nonexistent.txt");

        let result = remove_existing_destination(&path);
        assert!(result.is_ok());
    }

    #[test]
    fn remove_incomplete_destination_removes_file() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("incomplete.txt");
        fs::write(&path, b"partial content").expect("write file");

        remove_incomplete_destination(&path);
        assert!(!path.exists());
    }

    #[test]
    fn remove_incomplete_destination_does_not_panic_when_not_found() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("nonexistent.txt");

        // Should not panic
        remove_incomplete_destination(&path);
    }

    #[test]
    fn destination_write_guard_new_creates_temp_file() {
        let temp = tempdir().expect("tempdir");
        let dest = temp.path().join("final.txt");

        let (guard, mut file) =
            DestinationWriteGuard::new(&dest, false, None, None).expect("guard");

        // Temp file should exist and be writable
        file.write_all(b"test content").expect("write");

        // Verify staging path is different from final path
        assert_ne!(guard.staging_path(), guard.final_path());
        assert!(guard.staging_path().exists());

        guard.discard();
    }

    #[test]
    fn destination_write_guard_commit_renames_to_final_path() {
        let temp = tempdir().expect("tempdir");
        let dest = temp.path().join("final.txt");

        let (guard, mut file) =
            DestinationWriteGuard::new(&dest, false, None, None).expect("guard");
        file.write_all(b"content").expect("write");
        drop(file);

        let staging = guard.staging_path().to_path_buf();
        guard.commit().expect("commit");

        // Final path should exist
        assert!(dest.exists());
        // Staging path should be gone
        assert!(!staging.exists());

        // Verify content
        let content = fs::read_to_string(&dest).expect("read");
        assert_eq!(content, "content");
    }

    #[test]
    fn destination_write_guard_discard_removes_temp_file() {
        let temp = tempdir().expect("tempdir");
        let dest = temp.path().join("final.txt");

        let (guard, _file) = DestinationWriteGuard::new(&dest, false, None, None).expect("guard");
        let staging = guard.staging_path().to_path_buf();

        guard.discard();

        // Staging path should be removed
        assert!(!staging.exists());
        // Final path should not exist
        assert!(!dest.exists());
    }

    #[test]
    fn destination_write_guard_drop_removes_temp_file_if_not_committed() {
        let temp = tempdir().expect("tempdir");
        let dest = temp.path().join("final.txt");

        let staging;
        {
            let (guard, _file) =
                DestinationWriteGuard::new(&dest, false, None, None).expect("guard");
            staging = guard.staging_path().to_path_buf();
            // Guard is dropped here without commit
        }

        // Staging path should be removed by Drop
        assert!(!staging.exists());
    }

    #[test]
    fn destination_write_guard_partial_mode_creates_partial_file() {
        let temp = tempdir().expect("tempdir");
        let dest = temp.path().join("final.txt");

        let (guard, mut file) = DestinationWriteGuard::new(&dest, true, None, None).expect("guard");

        file.write_all(b"partial content").expect("write");

        // Staging path should end with appropriate suffix for partial
        let staging = guard.staging_path().to_path_buf();
        assert!(staging.to_string_lossy().contains("final.txt"));

        guard.discard();
    }

    #[test]
    fn destination_write_guard_partial_preserves_on_discard() {
        let temp = tempdir().expect("tempdir");
        let dest = temp.path().join("final.txt");

        let (guard, mut file) = DestinationWriteGuard::new(&dest, true, None, None).expect("guard");
        file.write_all(b"partial content").expect("write");
        drop(file);

        let staging = guard.staging_path().to_path_buf();
        guard.discard();

        // In partial mode, discard preserves the file
        assert!(staging.exists());
    }

    #[test]
    fn destination_write_guard_final_path_returns_destination() {
        let temp = tempdir().expect("tempdir");
        let dest = temp.path().join("final.txt");

        let (guard, _file) = DestinationWriteGuard::new(&dest, false, None, None).expect("guard");

        assert_eq!(guard.final_path(), dest.as_path());

        guard.discard();
    }

    #[test]
    fn destination_write_guard_commit_replaces_existing_file() {
        let temp = tempdir().expect("tempdir");
        let dest = temp.path().join("final.txt");

        // Create existing file
        fs::write(&dest, b"old content").expect("write existing");

        let (guard, mut file) =
            DestinationWriteGuard::new(&dest, false, None, None).expect("guard");
        file.write_all(b"new content").expect("write");
        drop(file);

        guard.commit().expect("commit");

        // Should have new content
        let content = fs::read_to_string(&dest).expect("read");
        assert_eq!(content, "new content");
    }

    #[test]
    fn destination_write_guard_staging_path_is_accessible() {
        let temp = tempdir().expect("tempdir");
        let dest = temp.path().join("final.txt");

        let (guard, _file) = DestinationWriteGuard::new(&dest, false, None, None).expect("guard");

        // Staging path should be a valid path we can access
        let staging = guard.staging_path();
        assert!(staging.exists());
        assert!(staging.is_file());

        guard.discard();
    }

    // ==================== DirectWriteGuard Tests ====================

    #[test]
    fn direct_write_guard_removes_file_on_drop() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("direct.txt");
        fs::write(&path, b"partial content").expect("write file");

        {
            let _guard = DirectWriteGuard::new(path.clone());
            assert!(path.exists());
            // Guard is dropped here without commit
        }

        assert!(!path.exists(), "file should be removed on drop");
    }

    #[test]
    fn direct_write_guard_preserves_file_on_commit() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("direct-committed.txt");
        fs::write(&path, b"complete content").expect("write file");

        {
            let mut guard = DirectWriteGuard::new(path.clone());
            guard.commit();
            // Guard is dropped here after commit
        }

        assert!(path.exists(), "file should be preserved after commit");
        let content = fs::read_to_string(&path).expect("read");
        assert_eq!(content, "complete content");
    }

    #[test]
    fn direct_write_guard_handles_already_removed_file() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("removed.txt");
        // Don't create the file — guard drop should not panic

        {
            let _guard = DirectWriteGuard::new(path.clone());
            // Guard is dropped, file doesn't exist — should not panic
        }

        assert!(!path.exists());
    }

    #[test]
    fn direct_write_guard_path_returns_destination() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("check-path.txt");
        fs::write(&path, b"data").expect("write file");

        let guard = DirectWriteGuard::new(path.clone());
        assert_eq!(guard.path(), path.as_path());

        let mut guard = guard;
        guard.commit();
    }
}
