//! Destination file write guard with atomic temp-file-and-rename semantics.
//!
//! Provides [`DestinationWriteGuard`] which manages the lifecycle of a
//! temporary file during transfer: creation, writing, and atomic rename
//! to the final destination on commit.
//!
//! upstream: receiver.c:recv_files() - temp file creation and rename

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering as AtomicOrdering;

use crate::local_copy::LocalCopyError;

use super::super::super::NEXT_TEMP_FILE_ID;
use super::paths::{partial_dir_fname, temp_name_with_suffix, temporary_destination_path};
use crate::CleanupManager;

/// Where an interrupted `--partial` temp file is finalised, and how the temp is
/// cleaned up on failure. Mirrors upstream's `keep_partial`/`partial_dir`
/// handling in `receiver.c`/`cleanup.c`.
#[derive(Debug, Clone)]
enum PartialKind {
    /// Non-partial transfer: unlink the temp file on failure.
    Discard,
    /// `--partial` (no dir): on failure the temp is moved onto the real
    /// destination, and its modtime is tweaked to epoch 0 so a later `--update`
    /// will not skip the unfinished file. upstream: `cleanup.c` `tweak_modtime`.
    Keep,
    /// `--partial-dir=DIR`: on failure the temp is moved onto the partial-dir
    /// entry `file`; on success that entry is removed and, for a relative dir,
    /// the now-empty `remove_dir` is rmdir'd. upstream: `handle_partial_dir()`.
    Dir {
        file: PathBuf,
        remove_dir: Option<PathBuf>,
    },
}

impl PartialKind {
    /// The path an interrupted temp is finalised onto, or `None` to unlink it.
    fn partial_dest<'a>(&'a self, final_path: &'a Path) -> Option<&'a Path> {
        match self {
            Self::Discard => None,
            Self::Keep => Some(final_path),
            Self::Dir { file, .. } => Some(file),
        }
    }

    /// Whether the finalised partial's modtime should be reset to epoch 0.
    const fn tweak_mtime(&self) -> bool {
        matches!(self, Self::Keep)
    }
}

/// Generates a six-character mkstemp-style suffix from process-unique inputs.
///
/// upstream fills the `.XXXXXX` template via `mkstemp`; we retry `create_new`
/// with fresh suffixes on the rare collision, so the suffix only needs to vary.
fn temp_suffix() -> String {
    const ALPHABET: &[u8; 62] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    let counter = NEXT_TEMP_FILE_ID.fetch_add(1, AtomicOrdering::Relaxed) as u64;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as u64);
    let mut state =
        (u64::from(std::process::id()) ^ nanos).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ counter;
    let mut suffix = String::with_capacity(6);
    for _ in 0..6 {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        suffix.push(ALPHABET[(state >> 33) as usize % ALPHABET.len()] as char);
    }
    suffix
}

/// Removes an existing destination file.
///
/// If the file does not exist, succeeds without error.
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

/// Removes an incomplete destination file, ignoring all errors.
///
/// Unlike [`remove_existing_destination`], silently ignores permission errors
/// since this is called during error recovery where the original error takes
/// priority.
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
        partial: PartialKind,
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
    /// The temporary file is created in the same directory as the
    /// destination (or in `temp_dir` if provided) to ensure atomic rename.
    ///
    /// # Errors
    ///
    /// Returns an error if the temporary file cannot be created, the
    /// destination directory does not exist, or permission is denied.
    pub fn new(
        destination: &Path,
        partial: bool,
        partial_dir: Option<&Path>,
        temp_dir: Option<&Path>,
    ) -> Result<(Self, fs::File), LocalCopyError> {
        let partial_kind = if partial {
            match partial_dir {
                Some(dir) => {
                    let file = partial_dir_fname(destination, dir);
                    // upstream: handle_partial_dir() only rmdir's a *relative*
                    // partial dir; an absolute one is a reserved location.
                    let remove_dir = if dir.is_relative() {
                        file.parent().map(Path::to_path_buf)
                    } else {
                        None
                    };
                    PartialKind::Dir { file, remove_dir }
                }
                None => PartialKind::Keep,
            }
        } else {
            PartialKind::Discard
        };

        // upstream: receiver.c always stages into a `.name.XXXXXX` temp beside
        // the destination (or in --temp-dir), regardless of partial mode; the
        // partial only appears at its final resting place on interrupt/success.
        loop {
            let temp_path = if partial {
                temp_name_with_suffix(destination, temp_dir, &temp_suffix())
            } else {
                let unique = NEXT_TEMP_FILE_ID.fetch_add(1, AtomicOrdering::Relaxed);
                temporary_destination_path(destination, unique, temp_dir)
            };
            match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp_path)
            {
                Ok(file) => {
                    let final_path = destination.to_path_buf();
                    // Only --partial/--partial-dir temps need the abort-path
                    // registry; a plain temp is unlinked by its own guard, so
                    // registering it would only add global-lock traffic to the
                    // hot non-partial copy path.
                    if partial {
                        CleanupManager::global().register_partial(
                            temp_path.clone(),
                            partial_kind
                                .partial_dest(&final_path)
                                .map(Path::to_path_buf),
                            partial_kind.tweak_mtime(),
                        );
                    }
                    return Ok((
                        Self {
                            final_path,
                            strategy: GuardStrategy::NamedTempFile {
                                temp_path,
                                partial: partial_kind,
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
    #[must_use]
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
    /// Returns `true` when the commit required a cross-device copy (EXDEV fallback)
    /// instead of an atomic rename. Callers holding an open fd to the temp file
    /// must invalidate it after a cross-device commit because the destination is a
    /// new inode - fd-based metadata operations (fchmod/fchown) would target the
    /// now-unlinked temp inode instead of the destination.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The rename, linkat, or copy operation fails
    /// - The destination cannot be removed
    /// - Permission is denied
    pub fn commit(mut self) -> Result<bool, LocalCopyError> {
        // Extract values from strategy before calling methods on self,
        // to avoid borrowing self.strategy and self simultaneously.
        enum CommitAction {
            Named(PathBuf),
            #[cfg(target_os = "linux")]
            Anonymous(Option<std::fs::File>),
        }

        let (action, partial) = match &mut self.strategy {
            GuardStrategy::NamedTempFile { temp_path, partial } => (
                CommitAction::Named(temp_path.clone()),
                Some(partial.clone()),
            ),
            #[cfg(target_os = "linux")]
            GuardStrategy::Anonymous { file } => (CommitAction::Anonymous(file.take()), None),
        };

        let cross_device = match action {
            CommitAction::Named(temp_path) => {
                let cross = self.commit_named_temp_file(temp_path.clone())?;
                match partial {
                    Some(PartialKind::Keep) => {
                        CleanupManager::global().unregister_partial(&temp_path);
                    }
                    // upstream: handle_partial_dir(partialptr, PDIR_DELETE) after
                    // a successful finish_transfer removes the partial-dir basis
                    // entry and rmdir's a now-empty relative partial dir. Only
                    // rmdir when we actually consumed a partial from the dir, so
                    // an empty, unused partial dir the user pre-created survives.
                    Some(PartialKind::Dir { file, remove_dir }) => {
                        CleanupManager::global().unregister_partial(&temp_path);
                        let consumed = fs::remove_file(&file).is_ok();
                        if consumed {
                            if let Some(dir) = remove_dir {
                                let _ = fs::remove_dir(dir);
                            }
                        }
                    }
                    _ => {}
                }
                cross
            }
            #[cfg(target_os = "linux")]
            CommitAction::Anonymous(file) => {
                self.commit_anonymous(file)?;
                false
            }
        };
        self.committed = true;
        Ok(cross_device)
    }

    /// Commits a named temp file via rename with retry logic.
    ///
    /// On Linux 5.11+ with io_uring available, the rename is submitted as an
    /// `IORING_OP_RENAMEAT` SQE instead of a synchronous `rename(2)` syscall.
    /// Falls back to `std::fs::rename` on all other platforms or when the
    /// kernel lacks the opcode.
    ///
    /// upstream: `util1.c:robust_rename()` - retry up to 4 times on `ETXTBSY`.
    /// Returns `true` when commit used a cross-device copy instead of rename.
    fn commit_named_temp_file(&self, temp_path: PathBuf) -> Result<bool, LocalCopyError> {
        let mut tries = 4u32;
        loop {
            let rename_result = if let Some(result) =
                fast_io::try_rename_via_io_uring(&temp_path, &self.final_path)
            {
                result
            } else {
                fs::rename(&temp_path, &self.final_path)
            };
            match rename_result {
                Ok(()) => return Ok(false),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    remove_existing_destination(&self.final_path)?;
                    let retry_result = if let Some(result) =
                        fast_io::try_rename_via_io_uring(&temp_path, &self.final_path)
                    {
                        result
                    } else {
                        fs::rename(&temp_path, &self.final_path)
                    };
                    retry_result.map_err(|rename_error| {
                        LocalCopyError::io(self.finalise_action(), temp_path.clone(), rename_error)
                    })?;
                    return Ok(false);
                }
                Err(error) if error.kind() == io::ErrorKind::ExecutableFileBusy => {
                    tries -= 1;
                    if tries == 0 {
                        return Err(LocalCopyError::io(self.finalise_action(), temp_path, error));
                    }
                    remove_existing_destination(&self.final_path)?;
                }
                // upstream: util1.c:robust_rename() EXDEV fallback calls
                // copy_file() which calls unlink_and_reopen() - removing the
                // existing destination before creating a fresh file. Without
                // this unlink, fs::copy fails with EACCES when the existing
                // destination has restrictive permissions (e.g. mode 440).
                Err(error) if error.kind() == io::ErrorKind::CrossesDevices => {
                    remove_existing_destination(&self.final_path)?;
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
                    return Ok(true);
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
                io::Error::other("anonymous fd already consumed"),
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
    #[must_use]
    pub fn final_path(&self) -> &Path {
        &self.final_path
    }

    /// Returns `true` if this guard uses the anonymous `O_TMPFILE` strategy.
    #[cfg(target_os = "linux")]
    #[must_use]
    pub fn is_anonymous(&self) -> bool {
        matches!(self.strategy, GuardStrategy::Anonymous { .. })
    }

    /// Returns `true` if this guard uses the anonymous `O_TMPFILE` strategy.
    #[cfg(not(target_os = "linux"))]
    #[must_use]
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
        self.finalize_partial_on_failure();
        self.committed = true;
    }

    /// Finalises the staging temp on an unsuccessful transfer: moves it onto the
    /// partial destination for `--partial`/`--partial-dir`, or unlinks it
    /// otherwise. Shared by [`discard`](Self::discard) and [`Drop`].
    fn finalize_partial_on_failure(&mut self) {
        if let GuardStrategy::NamedTempFile { temp_path, partial } = &self.strategy {
            crate::finalize_partial(
                temp_path,
                partial.partial_dest(&self.final_path),
                partial.tweak_mtime(),
            );
            // Only partial temps were registered on the abort path.
            if !matches!(partial, PartialKind::Discard) {
                CleanupManager::global().unregister_partial(temp_path);
            }
        }
    }

    /// Returns the action description for error messages.
    const fn finalise_action(&self) -> &'static str {
        match &self.strategy {
            GuardStrategy::NamedTempFile { partial, .. } => match partial {
                PartialKind::Discard => "finalise temporary file",
                PartialKind::Keep | PartialKind::Dir { .. } => "finalise partial file",
            },
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
        // An uncommitted guard means the transfer failed or was interrupted:
        // preserve the partial (--partial/--partial-dir) or unlink the temp.
        // upstream: cleanup.c moves the temp onto its partial destination when
        // keep_partial, otherwise do_unlink_at() removes it.
        self.finalize_partial_on_failure();
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

        // upstream: --partial moves the interrupted temp onto the destination
        // file itself. The staging temp is consumed by the move; the partial
        // now lives at `dest`.
        assert!(!staging.exists(), "staging temp is renamed onto the dest");
        assert!(
            dest.exists(),
            "partial data is preserved at the destination"
        );
        assert_eq!(fs::read(&dest).expect("read"), b"partial content");
    }

    #[test]
    fn destination_write_guard_partial_temp_uses_upstream_naming() {
        let temp = tempdir().expect("tempdir");
        let dest = temp.path().join("f3");

        let (guard, _file) = DestinationWriteGuard::new(&dest, true, None, None).expect("guard");
        let name = guard
            .staging_path()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        // upstream get_tmpname(): `.f3.XXXXXX` with a six-character suffix.
        assert!(name.starts_with(".f3."), "got {name}");
        assert_eq!(name.len(), ".f3.".len() + 6, "six-char suffix: {name}");
        guard.discard();
    }

    #[test]
    fn destination_write_guard_partial_dir_moves_temp_into_dir_on_discard() {
        let temp = tempdir().expect("tempdir");
        let dest_dir = temp.path().join("d1");
        fs::create_dir(&dest_dir).expect("mkdir");
        let dest = dest_dir.join("f3");
        let partial_dir = std::path::Path::new(".rsync-partial");

        let (guard, mut file) =
            DestinationWriteGuard::new(&dest, true, Some(partial_dir), None).expect("guard");
        // The in-progress temp lives beside the destination, not in the dir.
        let staging = guard.staging_path().to_path_buf();
        assert_eq!(staging.parent(), Some(dest_dir.as_path()));
        file.write_all(b"partialdata").expect("write");
        drop(file);
        guard.discard();

        // On interrupt the temp is moved into the (created) partial dir.
        let landed = dest_dir.join(".rsync-partial").join("f3");
        assert!(landed.exists(), "partial moved into --partial-dir");
        assert!(!staging.exists());
        assert!(
            !dest.exists(),
            "dest not written on interrupt for --partial-dir"
        );
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

    /// Tests for the io_uring RENAMEAT2 dispatch in `commit_named_temp_file`.
    ///
    /// These tests verify that the guard commits correctly regardless of
    /// whether io_uring handles the rename or the fallback `std::fs::rename`
    /// does. The dispatch is transparent to callers.
    mod io_uring_rename_dispatch {
        use super::*;

        #[test]
        fn commit_succeeds_via_io_uring_or_fallback() {
            let temp = tempdir().expect("tempdir");
            let dest = temp.path().join("iouring_rename.txt");

            let (guard, mut file) =
                DestinationWriteGuard::new(&dest, false, None, None).expect("guard");
            file.write_all(b"io_uring rename test").expect("write");
            drop(file);

            guard.commit().expect("commit must succeed");

            assert!(dest.exists());
            assert_eq!(
                fs::read_to_string(&dest).expect("read"),
                "io_uring rename test"
            );
        }

        #[test]
        fn commit_replaces_existing_via_io_uring_or_fallback() {
            let temp = tempdir().expect("tempdir");
            let dest = temp.path().join("iouring_replace.txt");

            fs::write(&dest, b"old content").expect("write existing");

            let (guard, mut file) =
                DestinationWriteGuard::new(&dest, false, None, None).expect("guard");
            file.write_all(b"new via io_uring or fallback")
                .expect("write");
            drop(file);

            guard.commit().expect("commit must succeed");

            assert_eq!(
                fs::read_to_string(&dest).expect("read"),
                "new via io_uring or fallback"
            );
        }

        #[test]
        fn try_rename_via_io_uring_returns_consistent_availability() {
            let dir = tempfile::tempdir().unwrap();
            let src = dir.path().join("probe_src.txt");
            let dst1 = dir.path().join("probe_dst1.txt");
            let dst2 = dir.path().join("probe_dst2.txt");
            fs::write(&src, b"data").unwrap();

            let first = fast_io::try_rename_via_io_uring(&src, &dst1).is_some();
            // If first call consumed the file, recreate.
            if first {
                fs::write(&src, b"data").unwrap();
                let _ = fs::remove_file(&dst1);
            }
            let second = fast_io::try_rename_via_io_uring(&src, &dst2).is_some();
            assert_eq!(first, second, "availability must be consistent");
        }
    }
}
