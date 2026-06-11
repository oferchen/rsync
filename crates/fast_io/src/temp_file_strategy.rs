//! Cross-platform temporary file strategy abstraction.
//!
//! Defines the [`TempFileStrategy`] trait that decouples the engine's write
//! guard from platform-specific temp file mechanisms. Implementations select
//! the best available mechanism at runtime:
//!
//! - **Linux**: [`AnonymousTempFileStrategy`] uses `O_TMPFILE` + `linkat(2)`
//!   for zero-cleanup atomic writes. No directory entry exists until commit.
//! - **Windows**: `WindowsTempFileStrategy` uses `FILE_FLAG_DELETE_ON_CLOSE`
//!   for auto-cleanup temp files. The kernel deletes the file when the last
//!   handle closes, providing crash-safe cleanup analogous to `O_TMPFILE`.
//! - **All platforms**: [`NamedTempFileStrategy`] uses a uniquely-named temp
//!   file + `rename(2)` for atomic commit, with cross-device fallback.
//!
//! The [`DefaultTempFileStrategy`] automatically selects the best available
//! strategy at runtime, probing `O_TMPFILE` support on Linux,
//! `FILE_FLAG_DELETE_ON_CLOSE` on Windows, and falling back to named temp
//! files elsewhere.
//!
//! # Design
//!
//! This follows the Strategy Pattern (Dependency Inversion) - the engine crate
//! depends on `TempFileStrategy` rather than on concrete `O_TMPFILE`,
//! `FILE_FLAG_DELETE_ON_CLOSE`, or `rename` logic directly. Each strategy
//! manages its own RAII cleanup.

use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};

/// Handle returned by [`TempFileStrategy::create`] containing the open file
/// and metadata needed for commit or cleanup.
///
/// The `kind` field determines how [`TempFileStrategy::commit`] materializes
/// the file at the destination path.
pub struct TempFileHandle {
    /// Open file handle for writing transfer data.
    pub file: File,
    /// The finalization method for this temp file.
    pub kind: TempFileKind,
}

/// Describes how a temporary file should be finalized.
#[derive(Debug)]
pub enum TempFileKind {
    /// Anonymous inode (Linux `O_TMPFILE`) - commit via `linkat(2)`.
    ///
    /// The `fd_for_link` is a clone of the file descriptor retained for
    /// the `linkat` call. The kernel auto-reclaims the inode on drop.
    #[cfg(target_os = "linux")]
    Anonymous {
        /// Cloned fd for `linkat(2)` - the writer fd is returned separately.
        fd_for_link: File,
    },
    /// Delete-on-close temp file (Windows `FILE_FLAG_DELETE_ON_CLOSE`).
    ///
    /// Commit clears the delete-on-close disposition via
    /// `SetFileInformationByHandle`, then renames to destination.
    /// If dropped without commit, the kernel auto-deletes the file.
    #[cfg(target_os = "windows")]
    DeleteOnClose {
        /// On-disk path of the delete-on-close temp file.
        temp_path: PathBuf,
    },
    /// Named temp file - commit via `rename(2)`.
    Named {
        /// On-disk path of the temp file.
        temp_path: PathBuf,
    },
}

/// Strategy for creating, committing, and discarding temporary files.
///
/// Implementations handle platform-specific mechanisms (O_TMPFILE, named temp
/// files) while exposing a uniform interface to the engine crate.
///
/// # Lifecycle
///
/// 1. [`create`](Self::create) - opens a temp file and returns a [`TempFileHandle`]
/// 2. Caller writes data to `handle.file`
/// 3. [`commit`](Self::commit) - atomically materializes at the destination
/// 4. On error: [`discard`](Self::discard) - cleans up the temp file
pub trait TempFileStrategy: Send + Sync {
    /// Creates a temporary file for writing.
    ///
    /// # Arguments
    ///
    /// * `destination` - the final path where the file will be committed
    ///
    /// # Returns
    ///
    /// A [`TempFileHandle`] with an open writable file and the metadata needed
    /// for commit or cleanup.
    fn create(&self, destination: &Path) -> io::Result<TempFileHandle>;

    /// Atomically materializes the temp file at `destination`.
    ///
    /// For anonymous files, uses `linkat(2)`. For named files, uses `rename(2)`
    /// with retry on `ETXTBSY` and cross-device fallback.
    ///
    /// If a file already exists at `destination`, it is removed first.
    fn commit(&self, handle: TempFileHandle, destination: &Path) -> io::Result<()>;

    /// Cleans up the temp file without committing.
    ///
    /// For anonymous files, dropping the fd is sufficient - the kernel reclaims
    /// the inode. For named files, the temp file is removed from disk.
    fn discard(&self, handle: TempFileHandle);

    /// Returns `true` if this strategy uses anonymous temp files (`O_TMPFILE`).
    fn is_anonymous(&self) -> bool;
}

/// Strategy using Linux `O_TMPFILE` + `linkat(2)` for zero-cleanup atomic writes.
///
/// The file has no directory entry until [`commit`](TempFileStrategy::commit)
/// is called. If dropped without committing, the kernel reclaims the anonymous
/// inode automatically.
///
/// # Platform support
///
/// Only available on Linux 3.11+ with a supporting filesystem (ext4, xfs,
/// btrfs, tmpfs). Callers should probe availability via
/// [`o_tmpfile_available`](crate::o_tmpfile_available) before constructing.
#[cfg(target_os = "linux")]
#[derive(Debug, Default, Clone, Copy)]
pub struct AnonymousTempFileStrategy;

#[cfg(target_os = "linux")]
impl TempFileStrategy for AnonymousTempFileStrategy {
    fn create(&self, destination: &Path) -> io::Result<TempFileHandle> {
        let dir = destination.parent().unwrap_or(Path::new("."));
        let file = crate::open_anonymous_tmpfile(dir, 0o644)?;
        let fd_for_link = file.try_clone()?;
        Ok(TempFileHandle {
            file,
            kind: TempFileKind::Anonymous { fd_for_link },
        })
    }

    fn commit(&self, handle: TempFileHandle, destination: &Path) -> io::Result<()> {
        if let TempFileKind::Anonymous { fd_for_link } = handle.kind {
            // linkat fails with EEXIST if the destination is already present.
            remove_if_exists(destination)?;
            crate::link_anonymous_tmpfile(&fd_for_link, destination)
        } else {
            Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "expected anonymous temp file kind",
            ))
        }
    }

    fn discard(&self, _handle: TempFileHandle) {
        // Dropping the fds is sufficient - kernel reclaims the anonymous inode.
    }

    fn is_anonymous(&self) -> bool {
        true
    }
}

/// Strategy using Windows `FILE_FLAG_DELETE_ON_CLOSE` for auto-cleanup temp files.
///
/// The file is created with a unique name and the delete-on-close flag. If
/// the process crashes before commit, the kernel deletes the file when the
/// handle is closed. On commit, the disposition is cleared and the file is
/// renamed to the destination.
///
/// # Platform support
///
/// Windows Vista+ with NTFS, ReFS, or FAT32. Callers should probe availability
/// via [`delete_on_close_available`](crate::delete_on_close_available) before
/// constructing.
#[cfg(target_os = "windows")]
#[derive(Debug, Default, Clone, Copy)]
pub struct WindowsTempFileStrategy;

#[cfg(target_os = "windows")]
impl TempFileStrategy for WindowsTempFileStrategy {
    fn create(&self, destination: &Path) -> io::Result<TempFileHandle> {
        let dir = destination.parent().unwrap_or(Path::new("."));
        let (file, temp_path) = crate::open_delete_on_close_tmpfile(dir)?;
        Ok(TempFileHandle {
            file,
            kind: TempFileKind::DeleteOnClose { temp_path },
        })
    }

    fn commit(&self, handle: TempFileHandle, destination: &Path) -> io::Result<()> {
        if let TempFileKind::DeleteOnClose { temp_path } = handle.kind {
            crate::commit_delete_on_close(handle.file, &temp_path, destination)
        } else {
            Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "expected delete-on-close temp file kind",
            ))
        }
    }

    fn discard(&self, _handle: TempFileHandle) {
        // Dropping the handle triggers kernel deletion via FILE_FLAG_DELETE_ON_CLOSE.
    }

    fn is_anonymous(&self) -> bool {
        // Delete-on-close files have a directory entry (unlike O_TMPFILE),
        // but provide the same crash-safe auto-cleanup semantics.
        false
    }
}

/// Strategy using a uniquely-named temporary file + `rename(2)`.
///
/// Works on all platforms. The temp file is created in the same directory as
/// the destination (or in a specified temp directory) to ensure atomic rename.
///
/// # Commit semantics
///
/// - Primary: `rename(2)` for same-filesystem atomic swap
/// - Retry: up to 4 attempts on `ETXTBSY` (upstream `util1.c:robust_rename()`)
/// - Fallback: `copy` + `remove` on cross-device (`EXDEV`)
#[derive(Debug)]
pub struct NamedTempFileStrategy {
    /// Optional temp directory override. If `None`, temp files are created
    /// alongside the destination.
    temp_dir: Option<PathBuf>,
    /// Counter for generating unique temp file names.
    counter: std::sync::atomic::AtomicU64,
}

impl NamedTempFileStrategy {
    /// Creates a new named temp file strategy.
    ///
    /// If `temp_dir` is `Some`, temp files are created there instead of
    /// alongside the destination. The temp directory must be on the same
    /// filesystem for atomic rename to succeed.
    #[must_use]
    pub fn new(temp_dir: Option<PathBuf>) -> Self {
        Self {
            temp_dir,
            counter: std::sync::atomic::AtomicU64::new(0),
        }
    }

    /// Generates a unique temp file path.
    fn temp_path(&self, destination: &Path) -> PathBuf {
        let unique = self
            .counter
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let pid = std::process::id();
        let name = format!(
            ".oc-rsync-{}.{pid}.{unique}",
            destination
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("tmp")
        );
        if let Some(ref dir) = self.temp_dir {
            dir.join(name)
        } else {
            destination.parent().unwrap_or(Path::new(".")).join(name)
        }
    }
}

impl Default for NamedTempFileStrategy {
    fn default() -> Self {
        Self::new(None)
    }
}

impl TempFileStrategy for NamedTempFileStrategy {
    fn create(&self, destination: &Path) -> io::Result<TempFileHandle> {
        let temp_path = self.temp_path(destination);
        let file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)?;
        Ok(TempFileHandle {
            file,
            kind: TempFileKind::Named { temp_path },
        })
    }

    fn commit(&self, handle: TempFileHandle, destination: &Path) -> io::Result<()> {
        match &handle.kind {
            TempFileKind::Named { temp_path } => {
                let path = temp_path.clone();
                drop(handle);
                commit_named_temp_file(&path, destination)
            }
            #[cfg(target_os = "linux")]
            TempFileKind::Anonymous { .. } => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "expected named temp file kind",
            )),
            #[cfg(target_os = "windows")]
            TempFileKind::DeleteOnClose { .. } => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "expected named temp file kind",
            )),
        }
    }

    fn discard(&self, handle: TempFileHandle) {
        match &handle.kind {
            TempFileKind::Named { temp_path } => {
                let path = temp_path.clone();
                drop(handle);
                let _ = fs::remove_file(&path);
            }
            #[cfg(target_os = "linux")]
            TempFileKind::Anonymous { .. } => {}
            #[cfg(target_os = "windows")]
            TempFileKind::DeleteOnClose { .. } => {
                // Drop triggers kernel deletion.
            }
        }
    }

    fn is_anonymous(&self) -> bool {
        false
    }
}

/// Auto-selecting strategy that probes for the best available temp file
/// mechanism at runtime.
///
/// - **Linux**: probes `O_TMPFILE` on the destination filesystem. If available,
///   uses anonymous temp files via [`AnonymousTempFileStrategy`].
/// - **Windows**: probes `FILE_FLAG_DELETE_ON_CLOSE`. If available, uses
///   delete-on-close temp files via `WindowsTempFileStrategy`.
/// - **All platforms**: falls back to named temp files via [`NamedTempFileStrategy`].
pub struct DefaultTempFileStrategy {
    named: NamedTempFileStrategy,
}

impl DefaultTempFileStrategy {
    /// Creates a new auto-selecting strategy.
    #[must_use]
    pub fn new(temp_dir: Option<PathBuf>) -> Self {
        Self {
            named: NamedTempFileStrategy::new(temp_dir),
        }
    }
}

impl Default for DefaultTempFileStrategy {
    fn default() -> Self {
        Self::new(None)
    }
}

impl TempFileStrategy for DefaultTempFileStrategy {
    fn create(&self, destination: &Path) -> io::Result<TempFileHandle> {
        #[cfg(target_os = "linux")]
        {
            let dir = destination.parent().unwrap_or(Path::new("."));
            if crate::o_tmpfile_available(dir) {
                let anon = AnonymousTempFileStrategy;
                return anon.create(destination);
            }
        }
        #[cfg(target_os = "windows")]
        {
            let dir = destination.parent().unwrap_or(Path::new("."));
            if crate::delete_on_close_available(dir) {
                let doc = WindowsTempFileStrategy;
                return doc.create(destination);
            }
        }
        self.named.create(destination)
    }

    fn commit(&self, handle: TempFileHandle, destination: &Path) -> io::Result<()> {
        match &handle.kind {
            #[cfg(target_os = "linux")]
            TempFileKind::Anonymous { .. } => {
                let anon = AnonymousTempFileStrategy;
                anon.commit(handle, destination)
            }
            #[cfg(target_os = "windows")]
            TempFileKind::DeleteOnClose { .. } => {
                let doc = WindowsTempFileStrategy;
                doc.commit(handle, destination)
            }
            TempFileKind::Named { .. } => self.named.commit(handle, destination),
        }
    }

    fn discard(&self, handle: TempFileHandle) {
        match &handle.kind {
            #[cfg(target_os = "linux")]
            TempFileKind::Anonymous { .. } => {
                // Drop fds - kernel reclaims inode.
            }
            #[cfg(target_os = "windows")]
            TempFileKind::DeleteOnClose { .. } => {
                // Drop triggers kernel deletion via FILE_FLAG_DELETE_ON_CLOSE.
            }
            TempFileKind::Named { temp_path } => {
                let path = temp_path.clone();
                drop(handle);
                let _ = fs::remove_file(&path);
            }
        }
    }

    fn is_anonymous(&self) -> bool {
        // DefaultTempFileStrategy may use either - report false since we
        // can't know until create() is called.
        false
    }
}

/// Commits a named temp file via rename with retry on `ETXTBSY`.
///
/// upstream: `util1.c:robust_rename()` - retry up to 4 times.
fn commit_named_temp_file(temp_path: &Path, destination: &Path) -> io::Result<()> {
    let mut tries = 4u32;
    loop {
        match fs::rename(temp_path, destination) {
            Ok(()) => return Ok(()),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                remove_if_exists(destination)?;
                return fs::rename(temp_path, destination);
            }
            Err(error) if error.kind() == io::ErrorKind::ExecutableFileBusy => {
                tries -= 1;
                if tries == 0 {
                    return Err(error);
                }
                remove_if_exists(destination)?;
            }
            #[cfg(unix)]
            Err(error) if error.raw_os_error() == Some(libc::EXDEV) => {
                fs::copy(temp_path, destination)?;
                fs::remove_file(temp_path)?;
                return Ok(());
            }
            Err(error) => return Err(error),
        }
    }
}

/// Removes a file if it exists, ignoring `NotFound`.
fn remove_if_exists(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::tempdir;

    #[test]
    fn named_strategy_create_and_commit() {
        let dir = tempdir().expect("tempdir");
        let dest = dir.path().join("output.txt");
        let strategy = NamedTempFileStrategy::default();

        let mut handle = strategy.create(&dest).expect("create");
        handle.file.write_all(b"hello world").expect("write");

        assert!(matches!(handle.kind, TempFileKind::Named { .. }));
        strategy.commit(handle, &dest).expect("commit");

        assert!(dest.exists());
        assert_eq!(fs::read_to_string(&dest).expect("read"), "hello world");
    }

    #[test]
    fn named_strategy_discard_removes_temp() {
        let dir = tempdir().expect("tempdir");
        let dest = dir.path().join("output.txt");
        let strategy = NamedTempFileStrategy::default();

        let handle = strategy.create(&dest).expect("create");
        let temp_path = match &handle.kind {
            TempFileKind::Named { temp_path } => temp_path.clone(),
            #[cfg(target_os = "linux")]
            TempFileKind::Anonymous { .. } => panic!("expected named"),
            #[cfg(target_os = "windows")]
            TempFileKind::DeleteOnClose { .. } => panic!("expected named"),
        };

        assert!(temp_path.exists());
        strategy.discard(handle);
        assert!(!temp_path.exists());
        assert!(!dest.exists());
    }

    #[test]
    fn named_strategy_commit_replaces_existing() {
        let dir = tempdir().expect("tempdir");
        let dest = dir.path().join("output.txt");
        fs::write(&dest, b"old content").expect("write existing");

        let strategy = NamedTempFileStrategy::default();
        let mut handle = strategy.create(&dest).expect("create");
        handle.file.write_all(b"new content").expect("write");
        strategy.commit(handle, &dest).expect("commit");

        assert_eq!(fs::read_to_string(&dest).expect("read"), "new content");
    }

    #[test]
    fn named_strategy_is_not_anonymous() {
        let strategy = NamedTempFileStrategy::default();
        assert!(!strategy.is_anonymous());
    }

    #[test]
    fn named_strategy_with_temp_dir() {
        let dir = tempdir().expect("tempdir");
        let temp_dir = tempdir().expect("temp_dir");
        let dest = dir.path().join("output.txt");

        let strategy = NamedTempFileStrategy::new(Some(temp_dir.path().to_path_buf()));
        let handle = strategy.create(&dest).expect("create");

        match &handle.kind {
            TempFileKind::Named { temp_path } => {
                assert!(temp_path.starts_with(temp_dir.path()));
            }
            #[cfg(target_os = "linux")]
            TempFileKind::Anonymous { .. } => panic!("expected named"),
            #[cfg(target_os = "windows")]
            TempFileKind::DeleteOnClose { .. } => panic!("expected named"),
        }

        strategy.discard(handle);
    }

    #[test]
    fn named_strategy_unique_names() {
        let dir = tempdir().expect("tempdir");
        let dest = dir.path().join("output.txt");
        let strategy = NamedTempFileStrategy::default();

        let h1 = strategy.create(&dest).expect("create 1");
        let h2 = strategy.create(&dest).expect("create 2");

        let p1 = match &h1.kind {
            TempFileKind::Named { temp_path } => temp_path.clone(),
            #[cfg(target_os = "linux")]
            TempFileKind::Anonymous { .. } => panic!("expected named"),
            #[cfg(target_os = "windows")]
            TempFileKind::DeleteOnClose { .. } => panic!("expected named"),
        };
        let p2 = match &h2.kind {
            TempFileKind::Named { temp_path } => temp_path.clone(),
            #[cfg(target_os = "linux")]
            TempFileKind::Anonymous { .. } => panic!("expected named"),
            #[cfg(target_os = "windows")]
            TempFileKind::DeleteOnClose { .. } => panic!("expected named"),
        };

        assert_ne!(p1, p2);

        strategy.discard(h1);
        strategy.discard(h2);
    }

    #[test]
    fn default_strategy_create_and_commit() {
        let dir = tempdir().expect("tempdir");
        let dest = dir.path().join("output.txt");
        let strategy = DefaultTempFileStrategy::default();

        let mut handle = strategy.create(&dest).expect("create");
        handle.file.write_all(b"default strategy").expect("write");
        strategy.commit(handle, &dest).expect("commit");

        assert!(dest.exists());
        assert_eq!(fs::read_to_string(&dest).expect("read"), "default strategy");
    }

    #[test]
    fn default_strategy_discard_leaves_no_file() {
        let dir = tempdir().expect("tempdir");
        let dest = dir.path().join("output.txt");
        let strategy = DefaultTempFileStrategy::default();

        let handle = strategy.create(&dest).expect("create");
        strategy.discard(handle);

        assert!(!dest.exists());
    }

    #[cfg(target_os = "linux")]
    mod linux {
        use super::*;

        fn o_tmpfile_supported(dir: &Path) -> bool {
            crate::o_tmpfile_available(dir)
        }

        #[test]
        fn anonymous_strategy_create_and_commit() {
            let dir = tempdir().expect("tempdir");
            if !o_tmpfile_supported(dir.path()) {
                return;
            }
            let dest = dir.path().join("anon.txt");
            let strategy = AnonymousTempFileStrategy;

            let mut handle = strategy.create(&dest).expect("create");
            assert!(strategy.is_anonymous());
            handle.file.write_all(b"anonymous data").expect("write");
            strategy.commit(handle, &dest).expect("commit");

            assert!(dest.exists());
            assert_eq!(fs::read_to_string(&dest).expect("read"), "anonymous data");
        }

        #[test]
        fn anonymous_strategy_discard_no_orphan() {
            let dir = tempdir().expect("tempdir");
            if !o_tmpfile_supported(dir.path()) {
                return;
            }
            let dest = dir.path().join("anon_discard.txt");
            let strategy = AnonymousTempFileStrategy;

            let handle = strategy.create(&dest).expect("create");
            strategy.discard(handle);

            assert!(!dest.exists());
            let count = fs::read_dir(dir.path()).expect("read_dir").count();
            assert_eq!(count, 0);
        }

        #[test]
        fn anonymous_strategy_commit_replaces_existing() {
            let dir = tempdir().expect("tempdir");
            if !o_tmpfile_supported(dir.path()) {
                return;
            }
            let dest = dir.path().join("anon_replace.txt");
            fs::write(&dest, b"old").expect("write existing");

            let strategy = AnonymousTempFileStrategy;
            let mut handle = strategy.create(&dest).expect("create");
            handle.file.write_all(b"new").expect("write");
            strategy.commit(handle, &dest).expect("commit");

            assert_eq!(fs::read_to_string(&dest).expect("read"), "new");
        }

        #[test]
        fn default_strategy_prefers_anonymous_when_available() {
            let dir = tempdir().expect("tempdir");
            if !o_tmpfile_supported(dir.path()) {
                return;
            }
            let dest = dir.path().join("default_anon.txt");
            let strategy = DefaultTempFileStrategy::default();

            let handle = strategy.create(&dest).expect("create");
            assert!(matches!(handle.kind, TempFileKind::Anonymous { .. }));
            strategy.discard(handle);
        }
    }

    #[cfg(target_os = "windows")]
    mod windows {
        use super::*;

        #[test]
        fn delete_on_close_strategy_create_and_commit() {
            let dir = tempdir().expect("tempdir");
            let dest = dir.path().join("doc.txt");
            let strategy = WindowsTempFileStrategy;

            let mut handle = strategy.create(&dest).expect("create");
            assert!(!strategy.is_anonymous());
            handle
                .file
                .write_all(b"delete-on-close data")
                .expect("write");
            strategy.commit(handle, &dest).expect("commit");

            assert!(dest.exists());
            assert_eq!(
                fs::read_to_string(&dest).expect("read"),
                "delete-on-close data"
            );
        }

        #[test]
        fn delete_on_close_strategy_discard_no_orphan() {
            let dir = tempdir().expect("tempdir");
            let dest = dir.path().join("doc_discard.txt");
            let strategy = WindowsTempFileStrategy;

            let handle = strategy.create(&dest).expect("create");
            let temp_path = match &handle.kind {
                TempFileKind::DeleteOnClose { temp_path } => temp_path.clone(),
                _ => panic!("expected delete-on-close"),
            };

            assert!(temp_path.exists());
            strategy.discard(handle);
            // Kernel should have deleted via FILE_FLAG_DELETE_ON_CLOSE.
            assert!(!temp_path.exists());
            assert!(!dest.exists());
        }

        #[test]
        fn delete_on_close_strategy_commit_replaces_existing() {
            let dir = tempdir().expect("tempdir");
            let dest = dir.path().join("doc_replace.txt");
            fs::write(&dest, b"old").expect("write existing");

            let strategy = WindowsTempFileStrategy;
            let mut handle = strategy.create(&dest).expect("create");
            handle.file.write_all(b"new").expect("write");
            strategy.commit(handle, &dest).expect("commit");

            assert_eq!(fs::read_to_string(&dest).expect("read"), "new");
        }

        #[test]
        fn default_strategy_prefers_delete_on_close_on_windows() {
            let dir = tempdir().expect("tempdir");
            let dest = dir.path().join("default_doc.txt");
            let strategy = DefaultTempFileStrategy::default();

            let handle = strategy.create(&dest).expect("create");
            assert!(matches!(handle.kind, TempFileKind::DeleteOnClose { .. }));
            strategy.discard(handle);
        }
    }

    #[test]
    fn remove_if_exists_succeeds_on_missing_file() {
        let result = remove_if_exists(Path::new("/nonexistent_test_path_xyz"));
        assert!(result.is_ok());
    }

    #[test]
    fn remove_if_exists_removes_existing_file() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("to_remove.txt");
        fs::write(&path, b"data").expect("write");

        remove_if_exists(&path).expect("remove");
        assert!(!path.exists());
    }
}
