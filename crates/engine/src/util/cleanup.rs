//! Cleanup coordination for temporary files and resources.
//!
//! Provides a global cleanup manager that tracks temporary files, partial
//! transfers, and other resources that need cleanup on shutdown or error.
//! Signal handlers and RAII guards in the `transfer` and `core` crates use
//! this registry to ensure stale temp files are removed on abnormal exit.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

/// Global cleanup manager instance.
static CLEANUP_MANAGER: OnceLock<Mutex<CleanupManagerState>> = OnceLock::new();

/// An in-progress temp file and where its partial data should end up if the
/// transfer is cut short. Mirrors upstream's `cleanup_fname`/`cleanup_new_fname`
/// pair (`cleanup.c:cleanup_set()`): `temp` is the `.name.XXXXXX` staging file,
/// and `partial_dest` is the destination the partial is moved to on interrupt
/// (the real file for `--partial`, or the partial-dir entry for
/// `--partial-dir`). `None` means "no partial kept" - just unlink the temp.
#[derive(Clone, Debug)]
struct PartialEntry {
    temp: PathBuf,
    partial_dest: Option<PathBuf>,
    tweak_mtime: bool,
}

/// Moves an interrupted temp file to its partial destination, or removes it.
///
/// upstream: `cleanup.c:exit_cleanup()` calls `finish_transfer()` to rename the
/// temp onto `cleanup_new_fname` (creating the partial dir first via
/// `handle_partial_dir(PDIR_CREATE)`), tweaking the modtime to epoch 0 when the
/// partial lands on the real destination file so `--update` will not skip it.
/// With no partial destination it unlinks the temp (`do_unlink_at`).
pub fn finalize_partial(temp: &Path, partial_dest: Option<&Path>, tweak_mtime: bool) {
    match partial_dest {
        Some(dest) => {
            if let Some(parent) = dest.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            // Best-effort: an already-committed transfer leaves no temp, so a
            // failed rename here is expected and harmless.
            if std::fs::rename(temp, dest).is_ok() && tweak_mtime {
                let epoch = std::time::SystemTime::UNIX_EPOCH;
                let times = std::fs::FileTimes::new().set_modified(epoch);
                if let Ok(file) = std::fs::File::options().write(true).open(dest) {
                    let _ = file.set_times(times);
                }
            }
        }
        None => {
            let _ = std::fs::remove_file(temp);
        }
    }
}

/// Cleanup manager for tracking and cleaning up temporary resources.
///
/// This type provides a global registry for temporary files and resources
/// that should be cleaned up on shutdown or error. It works with signal
/// handlers and RAII guards to ensure stale temp files are removed.
///
/// # Thread Safety
///
/// All methods are thread-safe and can be called from multiple threads
/// simultaneously. The internal state is protected by a mutex.
///
/// # Examples
///
/// ```
/// use engine::CleanupManager;
/// use std::path::PathBuf;
///
/// // Register a temp file for cleanup
/// let temp_file = PathBuf::from("/tmp/rsync.12345.tmp");
/// CleanupManager::global().register_temp_file(temp_file.clone());
///
/// // Do work...
///
/// // If successful, unregister so it's not cleaned up
/// CleanupManager::global().unregister_temp_file(&temp_file);
///
/// // Or if there's an error, cleanup all registered files
/// // CleanupManager::global().cleanup();
/// ```
#[derive(Debug)]
pub struct CleanupManager;

impl CleanupManager {
    /// Returns a reference to the global cleanup manager.
    ///
    /// This is the primary entry point for cleanup operations.
    #[must_use]
    pub fn global() -> &'static Self {
        let _ = CLEANUP_MANAGER.get_or_init(|| Mutex::new(CleanupManagerState::new()));
        &CLEANUP_MANAGER_INSTANCE
    }

    /// Registers a temporary file for cleanup.
    ///
    /// The file will be deleted when [`cleanup`](Self::cleanup) or
    /// [`cleanup_temp_files`](Self::cleanup_temp_files) is called,
    /// unless it is unregistered first.
    pub fn register_temp_file(&self, path: PathBuf) {
        if let Some(state) = CLEANUP_MANAGER.get() {
            if let Ok(mut state) = state.lock() {
                state.temp_files.insert(path);
            }
        }
    }

    /// Unregisters a temporary file from cleanup.
    ///
    /// Call this when a temporary file has been successfully committed
    /// (renamed to its final destination) and should not be deleted
    /// during cleanup.
    pub fn unregister_temp_file(&self, path: &Path) {
        if let Some(state) = CLEANUP_MANAGER.get() {
            if let Ok(mut state) = state.lock() {
                state.temp_files.remove(path);
            }
        }
    }

    /// Registers a cleanup callback to run on shutdown.
    ///
    /// The callback will be executed when [`cleanup`](Self::cleanup) is called.
    /// Callbacks are run in reverse order of registration (LIFO).
    pub fn register_cleanup(&self, callback: Box<dyn FnOnce() + Send>) {
        if let Some(state) = CLEANUP_MANAGER.get() {
            if let Ok(mut state) = state.lock() {
                state.cleanup_callbacks.push(callback);
            }
        }
    }

    /// Performs cleanup of all registered resources.
    ///
    /// This method:
    /// 1. Runs all registered cleanup callbacks in reverse order (LIFO)
    /// 2. Deletes all registered temporary files
    /// 3. Clears all registered resources
    ///
    /// Cleanup errors are logged but do not prevent other cleanup from proceeding.
    pub fn cleanup(&self) {
        if let Some(state) = CLEANUP_MANAGER.get() {
            if let Ok(mut state) = state.lock() {
                state.cleanup();
            }
        }
    }

    /// Cleans up only the registered temporary files.
    ///
    /// Similar to [`cleanup`](Self::cleanup) but only removes temporary files,
    /// without running cleanup callbacks.
    pub fn cleanup_temp_files(&self) {
        if let Some(state) = CLEANUP_MANAGER.get() {
            if let Ok(mut state) = state.lock() {
                state.cleanup_temp_files();
            }
        }
    }

    /// Returns the number of registered temporary files.
    ///
    /// Primarily useful for testing and diagnostics.
    #[must_use]
    pub fn temp_file_count(&self) -> usize {
        if let Some(state) = CLEANUP_MANAGER.get() {
            if let Ok(state) = state.lock() {
                return state.temp_files.len();
            }
        }
        0
    }

    /// Registers an in-progress temp file and its partial destination.
    ///
    /// Called when a `--partial`/`--partial-dir` staging file is opened so that
    /// a signal handler's abort path can finalise it even if the owning thread
    /// never returns to run its RAII guard. `partial_dest` is `None` for a
    /// non-partial transfer (the temp is simply unlinked on abort).
    pub fn register_partial(
        &self,
        temp: PathBuf,
        partial_dest: Option<PathBuf>,
        tweak_mtime: bool,
    ) {
        if let Some(state) = CLEANUP_MANAGER.get() {
            if let Ok(mut state) = state.lock() {
                state.partials.retain(|entry| entry.temp != temp);
                state.partials.push(PartialEntry {
                    temp,
                    partial_dest,
                    tweak_mtime,
                });
            }
        }
    }

    /// Removes a temp file from the partial registry after its guard committed
    /// or already finalised it.
    pub fn unregister_partial(&self, temp: &Path) {
        if let Some(state) = CLEANUP_MANAGER.get() {
            if let Ok(mut state) = state.lock() {
                state.partials.retain(|entry| entry.temp != temp);
            }
        }
    }

    /// Finalises every registered in-progress temp: moves each onto its partial
    /// destination (or unlinks it), then clears the registry. Invoked from the
    /// abort path (a second interrupt signal) that cannot wait for graceful
    /// unwinding. upstream: `cleanup.c:exit_cleanup()` on `RERR_SIGNAL`.
    pub fn finalize_partials(&self) {
        let entries = {
            let Some(state) = CLEANUP_MANAGER.get() else {
                return;
            };
            let Ok(mut state) = state.lock() else {
                return;
            };
            std::mem::take(&mut state.partials)
        };
        for entry in entries {
            finalize_partial(
                &entry.temp,
                entry.partial_dest.as_deref(),
                entry.tweak_mtime,
            );
        }
    }

    /// Clears all registered resources without performing cleanup.
    ///
    /// Primarily useful for testing.
    #[doc(hidden)]
    pub fn reset_for_testing(&self) {
        if let Some(state) = CLEANUP_MANAGER.get() {
            if let Ok(mut state) = state.lock() {
                state.temp_files.clear();
                state.cleanup_callbacks.clear();
                state.partials.clear();
            }
        }
    }
}

/// Singleton instance of the cleanup manager.
static CLEANUP_MANAGER_INSTANCE: CleanupManager = CleanupManager;

/// Internal state for the cleanup manager.
struct CleanupManagerState {
    temp_files: HashSet<PathBuf>,
    cleanup_callbacks: Vec<Box<dyn FnOnce() + Send>>,
    partials: Vec<PartialEntry>,
}

impl CleanupManagerState {
    fn new() -> Self {
        Self {
            temp_files: HashSet::new(),
            cleanup_callbacks: Vec::new(),
            partials: Vec::new(),
        }
    }

    fn cleanup(&mut self) {
        while let Some(callback) = self.cleanup_callbacks.pop() {
            callback();
        }

        self.cleanup_temp_files();
    }

    fn cleanup_temp_files(&mut self) {
        for path in &self.temp_files {
            let _ = std::fs::remove_file(path);
        }
        self.temp_files.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex as StdMutex};
    use tempfile::tempdir;

    // Global lock to serialize tests that use the global CleanupManager
    static TEST_LOCK: StdMutex<()> = StdMutex::new(());

    #[test]
    fn register_and_unregister_temp_file() {
        let _lock = TEST_LOCK.lock().unwrap();
        let manager = CleanupManager::global();
        manager.reset_for_testing();

        let path = PathBuf::from("/tmp/test_register_unregister.tmp");

        manager.register_temp_file(path.clone());
        assert_eq!(manager.temp_file_count(), 1);

        manager.unregister_temp_file(&path);
        assert_eq!(manager.temp_file_count(), 0);
    }

    #[test]
    fn cleanup_temp_files_removes_files() {
        let _lock = TEST_LOCK.lock().unwrap();
        let manager = CleanupManager::global();
        manager.reset_for_testing();

        let dir = tempdir().expect("tempdir");
        let path1 = dir.path().join("test1_cleanup.tmp");
        let path2 = dir.path().join("test2_cleanup.tmp");

        fs::write(&path1, b"data1").expect("write file 1");
        fs::write(&path2, b"data2").expect("write file 2");

        manager.register_temp_file(path1.clone());
        manager.register_temp_file(path2.clone());

        assert!(path1.exists());
        assert!(path2.exists());
        assert_eq!(manager.temp_file_count(), 2);

        manager.cleanup_temp_files();

        assert!(!path1.exists());
        assert!(!path2.exists());
        assert_eq!(manager.temp_file_count(), 0);
    }

    #[test]
    fn cleanup_temp_files_ignores_nonexistent() {
        let _lock = TEST_LOCK.lock().unwrap();
        let manager = CleanupManager::global();
        manager.reset_for_testing();

        let path = PathBuf::from("/tmp/nonexistent_test.tmp");
        manager.register_temp_file(path);

        manager.cleanup_temp_files();
        assert_eq!(manager.temp_file_count(), 0);
    }

    #[test]
    fn cleanup_callbacks_run_in_reverse_order() {
        let _lock = TEST_LOCK.lock().unwrap();
        let manager = CleanupManager::global();
        manager.reset_for_testing();

        let order = Arc::new(Mutex::new(Vec::new()));

        let order1 = Arc::clone(&order);
        manager.register_cleanup(Box::new(move || {
            order1.lock().unwrap().push(1);
        }));

        let order2 = Arc::clone(&order);
        manager.register_cleanup(Box::new(move || {
            order2.lock().unwrap().push(2);
        }));

        let order3 = Arc::clone(&order);
        manager.register_cleanup(Box::new(move || {
            order3.lock().unwrap().push(3);
        }));

        manager.cleanup();

        let final_order = order.lock().unwrap();
        assert_eq!(*final_order, vec![3, 2, 1]);
    }

    #[test]
    fn cleanup_runs_callbacks_and_removes_files() {
        let _lock = TEST_LOCK.lock().unwrap();
        let manager = CleanupManager::global();
        manager.reset_for_testing();

        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("test_cleanup_all.tmp");
        fs::write(&path, b"data").expect("write file");

        manager.register_temp_file(path.clone());

        let callback_ran = Arc::new(AtomicBool::new(false));
        let callback_flag = Arc::clone(&callback_ran);
        manager.register_cleanup(Box::new(move || {
            callback_flag.store(true, Ordering::SeqCst);
        }));

        manager.cleanup();

        assert!(callback_ran.load(Ordering::SeqCst));
        assert!(!path.exists());
        assert_eq!(manager.temp_file_count(), 0);
    }

    #[test]
    fn unregister_prevents_cleanup() {
        let _lock = TEST_LOCK.lock().unwrap();
        let manager = CleanupManager::global();
        manager.reset_for_testing();

        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("test_unregister.tmp");
        fs::write(&path, b"data").expect("write file");

        manager.register_temp_file(path.clone());
        manager.unregister_temp_file(&path);

        manager.cleanup_temp_files();

        assert!(path.exists(), "unregistered file must survive cleanup");
    }

    #[test]
    fn multiple_registrations_of_same_file() {
        let _lock = TEST_LOCK.lock().unwrap();
        let manager = CleanupManager::global();
        manager.reset_for_testing();

        let path = PathBuf::from("/tmp/test_multiple.tmp");

        manager.register_temp_file(path.clone());
        manager.register_temp_file(path.clone());
        manager.register_temp_file(path);

        assert_eq!(manager.temp_file_count(), 1, "HashSet deduplicates");
    }

    #[test]
    fn global_returns_same_instance() {
        let _lock = TEST_LOCK.lock().unwrap();
        let manager1 = CleanupManager::global();
        let manager2 = CleanupManager::global();

        manager1.reset_for_testing();
        manager1.register_temp_file(PathBuf::from("/tmp/test_global.tmp"));

        assert_eq!(manager2.temp_file_count(), 1);
    }
}
