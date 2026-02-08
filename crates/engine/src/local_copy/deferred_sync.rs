//! Deferred filesystem sync operations.
//!
//! This module provides [`DeferredSync`] which batches `fsync()` calls to reduce
//! the overhead of individual sync operations. Instead of syncing after every
//! file write, operations are queued and flushed in batches or at transfer end.
//!
//! # Design
//!
//! Upstream rsync provides `--fsync` to force data syncing. This module extends
//! that with configurable batching strategies to balance durability against
//! performance.
//!
//! # Strategies
//!
//! - [`SyncStrategy::Immediate`] - Sync after each file (maximum durability, slowest)
//! - [`SyncStrategy::Batched`] - Sync after N files (balanced)
//! - [`SyncStrategy::DirectoryLevel`] - Sync parent directories only (efficient)
//! - [`SyncStrategy::Deferred`] - Sync at end of transfer (fastest, least durable)
//!
//! # Platform Notes
//!
//! On Linux, [`syncfs()`] can sync an entire filesystem more efficiently than
//! multiple `fsync()` calls. This module uses `syncfs()` when available.

use std::collections::HashSet;
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};

#[cfg(target_os = "linux")]
use std::os::unix::io::AsRawFd;

/// Strategy for when to perform filesystem sync operations.
///
/// Note: Currently only `Batched` and `None` are used in production code.
/// Other variants are reserved for future implementation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // Most variants planned for future use
pub enum SyncStrategy {
    /// Sync immediately after each file write.
    ///
    /// Maximum durability but highest overhead.
    Immediate,

    /// Sync after writing a batch of files.
    ///
    /// The parameter specifies the batch size threshold.
    Batched(usize),

    /// Sync only parent directories, not individual files.
    ///
    /// Efficient when files will be synced via directory sync.
    DirectoryLevel,

    /// Defer all syncs until explicitly flushed.
    ///
    /// Fastest but least durable during transfer.
    Deferred,

    /// No syncing (rely on OS write-back).
    ///
    /// Fastest but data may be lost on crash.
    None,
}

impl Default for SyncStrategy {
    fn default() -> Self {
        Self::Deferred
    }
}

/// Manager for deferred filesystem sync operations.
///
/// Tracks files that need syncing and flushes them according to the
/// configured [`SyncStrategy`].
///
/// Note: Currently only `flush_if_threshold()` is used in production.
/// Other methods are tested and ready for future integration.
#[derive(Debug)]
pub struct DeferredSync {
    /// Files pending sync.
    #[allow(dead_code)] // Used in tests, planned for production use
    pending_files: Vec<PathBuf>,
    /// Directories pending sync (for DirectoryLevel strategy).
    #[allow(dead_code)] // Used in tests, planned for production use
    pending_dirs: HashSet<PathBuf>,
    /// Sync strategy.
    #[allow(dead_code)] // Used internally and in tests
    strategy: SyncStrategy,
    /// Batch threshold (for Batched strategy).
    #[allow(dead_code)] // Used internally and in tests
    threshold: usize,
}

impl DeferredSync {
    /// Creates a new deferred sync manager with the specified strategy.
    #[must_use]
    pub fn new(strategy: SyncStrategy) -> Self {
        let threshold = match strategy {
            SyncStrategy::Batched(n) => n,
            _ => 100, // Default threshold
        };

        Self {
            pending_files: Vec::new(),
            pending_dirs: HashSet::new(),
            strategy,
            threshold,
        }
    }

    /// Creates a new manager with custom batch threshold.
    #[must_use]
    #[allow(dead_code)] // Used in tests, planned for production use
    pub fn with_threshold(strategy: SyncStrategy, threshold: usize) -> Self {
        Self {
            pending_files: Vec::new(),
            pending_dirs: HashSet::new(),
            strategy,
            threshold,
        }
    }

    /// Registers a file that needs to be synced.
    ///
    /// Depending on the strategy, this may trigger an immediate sync
    /// or queue the file for later.
    ///
    /// # Errors
    ///
    /// Returns an error if immediate sync fails.
    pub fn register(&mut self, path: PathBuf) -> io::Result<()> {
        match self.strategy {
            SyncStrategy::Immediate => {
                sync_file(&path)?;
            }
            SyncStrategy::DirectoryLevel => {
                if let Some(parent) = path.parent() {
                    self.pending_dirs.insert(parent.to_path_buf());
                }
            }
            SyncStrategy::Batched(_) | SyncStrategy::Deferred => {
                self.pending_files.push(path);
            }
            SyncStrategy::None => {
                // No-op
            }
        }
        Ok(())
    }

    /// Flushes pending syncs if the threshold is reached.
    ///
    /// For [`SyncStrategy::Batched`], flushes when pending count exceeds threshold.
    /// For other strategies, this is a no-op.
    ///
    /// # Errors
    ///
    /// Returns an error if any sync operation fails.
    pub fn flush_if_threshold(&mut self) -> io::Result<()> {
        if matches!(self.strategy, SyncStrategy::Batched(_))
            && self.pending_files.len() >= self.threshold
        {
            return self.flush();
        }
        Ok(())
    }

    /// Flushes all pending sync operations.
    ///
    /// Syncs all queued files and directories, then clears the pending lists.
    ///
    /// # Errors
    ///
    /// Returns an error if any sync operation fails. Partial flush may occur
    /// on error.
    #[allow(dead_code)] // Used in tests, planned for production use
    pub fn flush(&mut self) -> io::Result<()> {
        match self.strategy {
            SyncStrategy::None => {
                // No-op
            }
            SyncStrategy::Immediate => {
                // Already synced immediately
            }
            SyncStrategy::DirectoryLevel => {
                self.flush_directories()?;
            }
            SyncStrategy::Batched(_) | SyncStrategy::Deferred => {
                self.flush_files()?;
            }
        }

        self.pending_files.clear();
        self.pending_dirs.clear();
        Ok(())
    }

    /// Flushes pending files.
    fn flush_files(&self) -> io::Result<()> {
        // If we have many files, try to use syncfs for efficiency
        #[cfg(target_os = "linux")]
        if self.pending_files.len() > 10 {
            if let Some(first) = self.pending_files.first() {
                if sync_filesystem(first).is_ok() {
                    return Ok(());
                }
            }
        }

        // Fall back to individual syncs
        for path in &self.pending_files {
            if let Err(e) = sync_file(path) {
                // Log but continue - file might have been moved/deleted
                if e.kind() != io::ErrorKind::NotFound {
                    return Err(e);
                }
            }
        }
        Ok(())
    }

    /// Flushes pending directories.
    fn flush_directories(&self) -> io::Result<()> {
        for dir in &self.pending_dirs {
            if let Err(e) = sync_directory(dir) {
                if e.kind() != io::ErrorKind::NotFound {
                    return Err(e);
                }
            }
        }
        Ok(())
    }

    /// Returns the number of files pending sync.
    #[must_use]
    #[allow(dead_code)] // Used in tests, planned for production use
    pub fn pending_count(&self) -> usize {
        self.pending_files.len() + self.pending_dirs.len()
    }

    /// Returns the current sync strategy.
    #[must_use]
    #[allow(dead_code)] // Used in tests, planned for production use
    pub fn strategy(&self) -> SyncStrategy {
        self.strategy
    }

    /// Changes the sync strategy.
    ///
    /// Pending operations are not affected; they will be flushed with
    /// the current strategy on next flush.
    #[allow(dead_code)] // Used in tests, planned for production use
    pub fn set_strategy(&mut self, strategy: SyncStrategy) {
        self.strategy = strategy;
        if let SyncStrategy::Batched(n) = strategy {
            self.threshold = n;
        }
    }
}

impl Default for DeferredSync {
    fn default() -> Self {
        Self::new(SyncStrategy::default())
    }
}

/// Syncs a single file to disk.
#[allow(dead_code)] // Used by flush methods which are tested
fn sync_file(path: &Path) -> io::Result<()> {
    let file = File::open(path)?;
    file.sync_all()
}

/// Syncs a directory to disk.
#[allow(dead_code)] // Used by flush methods which are tested
fn sync_directory(path: &Path) -> io::Result<()> {
    let dir = File::open(path)?;
    dir.sync_all()
}

/// Syncs an entire filesystem using syncfs() on Linux.
#[cfg(target_os = "linux")]
#[allow(dead_code, unsafe_code)] // Used by flush methods which are tested
fn sync_filesystem(path: &Path) -> io::Result<()> {
    let file = File::open(path)?;
    let fd = file.as_raw_fd();

    // SAFETY: fd is valid and syncfs is safe to call
    let result = unsafe { libc::syncfs(fd) };

    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(not(target_os = "linux"))]
#[allow(dead_code)]
fn sync_filesystem(_path: &Path) -> io::Result<()> {
    // Not available on this platform
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "syncfs not available",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn create_test_file(dir: &TempDir, name: &str) -> PathBuf {
        let path = dir.path().join(name);
        fs::write(&path, b"test content").unwrap();
        path
    }

    #[test]
    fn test_deferred_strategy() {
        let dir = TempDir::new().unwrap();
        let mut sync = DeferredSync::new(SyncStrategy::Deferred);

        let file1 = create_test_file(&dir, "file1.txt");
        let file2 = create_test_file(&dir, "file2.txt");

        sync.register(file1).unwrap();
        sync.register(file2).unwrap();

        assert_eq!(sync.pending_count(), 2);

        sync.flush().unwrap();
        assert_eq!(sync.pending_count(), 0);
    }

    #[test]
    fn test_immediate_strategy() {
        let dir = TempDir::new().unwrap();
        let mut sync = DeferredSync::new(SyncStrategy::Immediate);

        let file = create_test_file(&dir, "file.txt");
        sync.register(file).unwrap();

        // Immediate strategy doesn't queue files
        assert_eq!(sync.pending_count(), 0);
    }

    #[test]
    fn test_batched_strategy_threshold() {
        let dir = TempDir::new().unwrap();
        let mut sync = DeferredSync::with_threshold(SyncStrategy::Batched(3), 3);

        // Add files up to threshold
        for i in 0..3 {
            let file = create_test_file(&dir, &format!("file{i}.txt"));
            sync.register(file).unwrap();
        }

        assert_eq!(sync.pending_count(), 3);

        // flush_if_threshold should trigger flush
        sync.flush_if_threshold().unwrap();
        assert_eq!(sync.pending_count(), 0);
    }

    #[test]
    fn test_directory_level_strategy() {
        let dir = TempDir::new().unwrap();
        let subdir = dir.path().join("subdir");
        fs::create_dir(&subdir).unwrap();

        let mut sync = DeferredSync::new(SyncStrategy::DirectoryLevel);

        let file = subdir.join("file.txt");
        fs::write(&file, b"test").unwrap();

        sync.register(file).unwrap();

        // Should have queued the parent directory
        assert_eq!(sync.pending_count(), 1);
        assert!(sync.pending_dirs.contains(&subdir));
    }

    #[test]
    fn test_none_strategy() {
        let dir = TempDir::new().unwrap();
        let mut sync = DeferredSync::new(SyncStrategy::None);

        let file = create_test_file(&dir, "file.txt");
        sync.register(file).unwrap();

        // None strategy doesn't track anything
        assert_eq!(sync.pending_count(), 0);
    }

    #[test]
    fn test_flush_clears_pending() {
        let dir = TempDir::new().unwrap();
        let mut sync = DeferredSync::new(SyncStrategy::Deferred);

        for i in 0..5 {
            let file = create_test_file(&dir, &format!("file{i}.txt"));
            sync.register(file).unwrap();
        }

        assert_eq!(sync.pending_count(), 5);
        sync.flush().unwrap();
        assert_eq!(sync.pending_count(), 0);
    }

    #[test]
    fn test_set_strategy() {
        let mut sync = DeferredSync::new(SyncStrategy::Deferred);
        assert_eq!(sync.strategy(), SyncStrategy::Deferred);

        sync.set_strategy(SyncStrategy::Batched(50));
        assert_eq!(sync.strategy(), SyncStrategy::Batched(50));
        assert_eq!(sync.threshold, 50);
    }

    #[test]
    fn test_default() {
        let sync = DeferredSync::default();
        assert_eq!(sync.strategy(), SyncStrategy::Deferred);
    }

    #[test]
    fn test_register_handles_missing_file() {
        let mut sync = DeferredSync::new(SyncStrategy::Deferred);
        // Register a non-existent file (this is allowed, error on flush)
        sync.register(PathBuf::from("/nonexistent/file.txt"))
            .unwrap();
        assert_eq!(sync.pending_count(), 1);

        // Flush should handle missing file gracefully
        sync.flush().unwrap(); // NotFound is ignored
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn test_syncfs_on_linux() {
        let dir = TempDir::new().unwrap();
        let file = create_test_file(&dir, "file.txt");

        // This should succeed on Linux
        let result = sync_filesystem(&file);
        assert!(result.is_ok());
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn test_syncfs_fallback() {
        let dir = TempDir::new().unwrap();
        let file = create_test_file(&dir, "file.txt");

        // Should return unsupported error
        let result = sync_filesystem(&file);
        assert!(result.is_err());
    }
}
