//! Destination file write guard with atomic temp-file-and-rename semantics.
//!
//! Provides [`DestinationWriteGuard`] which manages the lifecycle of a
//! temporary file during transfer: creation, writing, and atomic rename
//! to the final destination on commit.
//!
//! // upstream: receiver.c:recv_files() - temp file creation and rename

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

/// Finalization strategy for destination writes.
///
/// Named temp files use rename; anonymous temp files use `linkat(2)`.
#[derive(Debug)]
enum GuardStrategy {
    /// Traditional named temp file - commit via rename.
    NamedTempFile {
        temp_path: PathBuf,
        preserve_on_error: bool,
    },
    /// Linux `O_TMPFILE` anonymous file - commit via `linkat(2)`.
    ///
    /// The file descriptor is held by the caller; `commit` uses
    /// `fast_io::link_anonymous_tmpfile` to materialize it. On drop
    /// without commit, the kernel reclaims the anonymous inode.
    #[cfg(target_os = "linux")]
    Anonymous {
        /// Kept alive so the fd stays open until `link_anonymous_tmpfile`.
        file: Option<fs::File>,
    },
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
/// - **Anonymous mode** (Linux only): Uses `O_TMPFILE` + `linkat(2)` for zero-cleanup
///   atomic writes. No directory entry exists until commit.
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
    strategy: GuardStrategy,
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
                    strategy: GuardStrategy::NamedTempFile {
                        temp_path,
                        preserve_on_error: true,
                    },
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
                                strategy: GuardStrategy::NamedTempFile {
                                    temp_path,
                                    preserve_on_error: false,
                                },
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

    /// Creates a write guard backed by an anonymous `O_TMPFILE` file descriptor.
    ///
    /// On Linux 3.11+ with a supporting filesystem, this opens an anonymous inode
    /// via `O_TMPFILE`. The returned `File` is writable but invisible in the
    /// directory listing. Calling [`commit`](Self::commit) materializes it at the
    /// destination using `linkat(2)`.
    ///
    /// # Errors
    ///
    /// Returns an error if `O_TMPFILE` is not supported or the directory is not
    /// writable. Callers should fall back to [`new`](Self::new) on failure.
    #[cfg(target_os = "linux")]
    pub fn new_anonymous(destination: &Path) -> Result<(Self, fs::File), LocalCopyError> {
        let dir = destination.parent().unwrap_or(Path::new("."));
        let file = fast_io::open_anonymous_tmpfile(dir, 0o644)
            .map_err(|error| LocalCopyError::io("open anonymous temp file", destination, error))?;
        // Clone the fd so the guard retains one for linkat while the caller
        // writes through the other. Both refer to the same anonymous inode.
        let writer = file
            .try_clone()
            .map_err(|error| LocalCopyError::io("clone anonymous temp fd", destination, error))?;
        Ok((
            Self {
                final_path: destination.to_path_buf(),
                strategy: GuardStrategy::Anonymous { file: Some(file) },
                committed: false,
            },
            writer,
        ))
    }

    /// Returns the path to the staging (temporary) file.
    ///
    /// For named temp files this is the on-disk temp path. For anonymous files
    /// this returns the final destination path since no intermediate path exists.
    pub fn staging_path(&self) -> &Path {
        match &self.strategy {
            GuardStrategy::NamedTempFile { temp_path, .. } => temp_path,
            #[cfg(target_os = "linux")]
            GuardStrategy::Anonymous { .. } => &self.final_path,
        }
    }

    /// Commits the temporary file to the final destination.
    ///
    /// For named temp files, this atomically renames the temp file. For anonymous
    /// files, this uses `linkat(2)` to materialize the inode at the destination.
    ///
    /// If the destination already exists, it is removed before the commit.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The rename, linkat, or copy operation fails
    /// - The destination cannot be removed
    /// - Permission is denied
    pub fn commit(mut self) -> Result<(), LocalCopyError> {
        // Extract values from strategy before calling methods on self,
        // to avoid borrowing self.strategy and self simultaneously.
        enum CommitAction {
            Named(PathBuf),
            #[cfg(target_os = "linux")]
            Anonymous(Option<std::fs::File>),
        }

        let action = match &mut self.strategy {
            GuardStrategy::NamedTempFile {
                temp_path,
                preserve_on_error: _,
            } => CommitAction::Named(temp_path.clone()),
            #[cfg(target_os = "linux")]
            GuardStrategy::Anonymous { file } => CommitAction::Anonymous(file.take()),
        };

        match action {
            CommitAction::Named(temp_path) => {
                self.commit_named_temp_file(temp_path)?;
            }
            #[cfg(target_os = "linux")]
            CommitAction::Anonymous(file) => {
                self.commit_anonymous(file)?;
            }
        }
        self.committed = true;
        Ok(())
    }

    /// Commits a named temp file via rename with retry logic.
    ///
    /// upstream: `util1.c:robust_rename()` - retry up to 4 times on `ETXTBSY`.
    fn commit_named_temp_file(&self, temp_path: PathBuf) -> Result<(), LocalCopyError> {
        let mut tries = 4u32;
        loop {
            match fs::rename(&temp_path, &self.final_path) {
                Ok(()) => return Ok(()),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    remove_existing_destination(&self.final_path)?;
                    fs::rename(&temp_path, &self.final_path).map_err(|rename_error| {
                        LocalCopyError::io(self.finalise_action(), temp_path.clone(), rename_error)
                    })?;
                    return Ok(());
                }
                Err(error) if error.kind() == io::ErrorKind::ExecutableFileBusy => {
                    tries -= 1;
                    if tries == 0 {
                        return Err(LocalCopyError::io(self.finalise_action(), temp_path, error));
                    }
                    remove_existing_destination(&self.final_path)?;
                }
                Err(error) if error.kind() == io::ErrorKind::CrossesDevices => {
                    fs::copy(&temp_path, &self.final_path).map_err(|copy_error| {
                        LocalCopyError::io(
                            self.finalise_action(),
                            self.final_path.clone(),
                            copy_error,
                        )
                    })?;
                    fs::remove_file(&temp_path).map_err(|remove_error| {
                        LocalCopyError::io(self.finalise_action(), temp_path, remove_error)
                    })?;
                    return Ok(());
                }
                Err(error) => {
                    return Err(LocalCopyError::io(self.finalise_action(), temp_path, error));
                }
            }
        }
    }

    /// Commits an anonymous `O_TMPFILE` via `linkat(2)`.
    ///
    /// If the destination already exists, it is removed first so `linkat` succeeds.
    #[cfg(target_os = "linux")]
    fn commit_anonymous(&self, file: Option<fs::File>) -> Result<(), LocalCopyError> {
        let file = file.ok_or_else(|| {
            LocalCopyError::io(
                "finalise anonymous temp file",
                &self.final_path,
                io::Error::new(io::ErrorKind::Other, "anonymous fd already consumed"),
            )
        })?;
        // Remove existing destination so linkat does not fail with EEXIST.
        remove_existing_destination(&self.final_path)?;
        fast_io::link_anonymous_tmpfile(&file, &self.final_path).map_err(|error| {
            LocalCopyError::io("finalise anonymous temp file", &self.final_path, error)
        })
    }

    /// Returns the final destination path.
    ///
    /// This is the path where the file will be located after calling [`commit`](Self::commit).
    pub fn final_path(&self) -> &Path {
        &self.final_path
    }

    /// Returns `true` if this guard uses the anonymous `O_TMPFILE` strategy.
    #[cfg(target_os = "linux")]
    pub fn is_anonymous(&self) -> bool {
        matches!(self.strategy, GuardStrategy::Anonymous { .. })
    }

    /// Returns `true` if this guard uses the anonymous `O_TMPFILE` strategy.
    #[cfg(not(target_os = "linux"))]
    pub fn is_anonymous(&self) -> bool {
        false
    }

    /// Discards the temporary file without committing.
    ///
    /// In normal mode, this removes the temporary file. In partial mode, the file
    /// is preserved to allow for transfer resumption with an ancient mtime so
    /// that `--update` will not skip it on retry. In anonymous mode, the kernel
    /// reclaims the inode automatically on drop.
    ///
    /// This method consumes the guard, preventing accidental use after discard.
    pub fn discard(mut self) {
        match &self.strategy {
            GuardStrategy::NamedTempFile {
                temp_path,
                preserve_on_error,
            } => {
                if *preserve_on_error {
                    // upstream: receiver.c - set mtime to epoch 0 on partial files so
                    // --update won't skip them during a subsequent retry.
                    let epoch = std::time::SystemTime::UNIX_EPOCH;
                    let times = fs::FileTimes::new().set_modified(epoch);
                    if let Ok(file) = fs::File::options().write(true).open(temp_path) {
                        let _ = file.set_times(times);
                    }
                } else if let Err(error) = fs::remove_file(temp_path)
                    && error.kind() != io::ErrorKind::NotFound
                {
                    // Best-effort cleanup: the file may have been removed concurrently.
                }
            }
            #[cfg(target_os = "linux")]
            GuardStrategy::Anonymous { .. } => {
                // Dropping the guard drops the anonymous fd; the kernel reclaims the inode.
            }
        }
        self.committed = true;
    }

    /// Returns the action description for error messages.
    const fn finalise_action(&self) -> &'static str {
        match &self.strategy {
            GuardStrategy::NamedTempFile {
                preserve_on_error, ..
            } => {
                if *preserve_on_error {
                    "finalise partial file"
                } else {
                    "finalise temporary file"
                }
            }
            #[cfg(target_os = "linux")]
            GuardStrategy::Anonymous { .. } => "finalise anonymous temp file",
        }
    }
}

impl Drop for DestinationWriteGuard {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        match &self.strategy {
            GuardStrategy::NamedTempFile {
                temp_path,
                preserve_on_error,
            } => {
                if !preserve_on_error {
                    let _ = fs::remove_file(temp_path);
                }
            }
            #[cfg(target_os = "linux")]
            GuardStrategy::Anonymous { .. } => {
                // Anonymous fd is dropped, kernel reclaims the inode.
            }
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

    #[test]
    fn is_anonymous_false_for_named_temp_file() {
        let temp = tempdir().expect("tempdir");
        let dest = temp.path().join("final.txt");

        let (guard, _file) = DestinationWriteGuard::new(&dest, false, None, None).expect("guard");
        assert!(!guard.is_anonymous());
        guard.discard();
    }

    // --- Linux-specific anonymous guard tests ---

    #[cfg(target_os = "linux")]
    mod anonymous {
        use super::*;

        fn o_tmpfile_supported(dir: &Path) -> bool {
            fast_io::o_tmpfile_available(dir)
        }

        #[test]
        fn new_anonymous_creates_guard() {
            let temp = tempdir().expect("tempdir");
            let dest = temp.path().join("anon.txt");
            if !o_tmpfile_supported(temp.path()) {
                return;
            }

            let (guard, _file) = DestinationWriteGuard::new_anonymous(&dest).expect("guard");
            assert!(guard.is_anonymous());
            // Anonymous files have no visible staging path - staging_path returns final_path.
            assert_eq!(guard.staging_path(), guard.final_path());
            guard.discard();
        }

        #[test]
        fn anonymous_write_and_commit() {
            let temp = tempdir().expect("tempdir");
            let dest = temp.path().join("anon_commit.txt");
            if !o_tmpfile_supported(temp.path()) {
                return;
            }

            let (guard, mut file) = DestinationWriteGuard::new_anonymous(&dest).expect("guard");
            file.write_all(b"anonymous content").expect("write");
            drop(file);

            guard.commit().expect("commit");
            assert!(dest.exists());
            assert_eq!(
                fs::read_to_string(&dest).expect("read"),
                "anonymous content"
            );
        }

        #[test]
        fn anonymous_commit_replaces_existing() {
            let temp = tempdir().expect("tempdir");
            let dest = temp.path().join("anon_replace.txt");
            if !o_tmpfile_supported(temp.path()) {
                return;
            }

            fs::write(&dest, b"old").expect("create existing");

            let (guard, mut file) = DestinationWriteGuard::new_anonymous(&dest).expect("guard");
            file.write_all(b"new").expect("write");
            drop(file);

            guard.commit().expect("commit");
            assert_eq!(fs::read_to_string(&dest).expect("read"), "new");
        }

        #[test]
        fn anonymous_discard_leaves_no_file() {
            let temp = tempdir().expect("tempdir");
            let dest = temp.path().join("anon_discard.txt");
            if !o_tmpfile_supported(temp.path()) {
                return;
            }

            let (guard, mut file) = DestinationWriteGuard::new_anonymous(&dest).expect("guard");
            file.write_all(b"discarded").expect("write");
            drop(file);
            guard.discard();

            assert!(!dest.exists());
            // Directory should be empty - no orphaned temp files.
            let count = fs::read_dir(temp.path()).expect("read_dir").count();
            assert_eq!(count, 0);
        }

        #[test]
        fn anonymous_drop_without_commit_leaves_no_file() {
            let temp = tempdir().expect("tempdir");
            let dest = temp.path().join("anon_drop.txt");
            if !o_tmpfile_supported(temp.path()) {
                return;
            }

            {
                let (_guard, _file) = DestinationWriteGuard::new_anonymous(&dest).expect("guard");
            }

            assert!(!dest.exists());
            let count = fs::read_dir(temp.path()).expect("read_dir").count();
            assert_eq!(count, 0);
        }
    }
}
