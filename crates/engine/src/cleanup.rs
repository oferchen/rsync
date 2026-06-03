//! Global cleanup registry for temporary files.
//!
//! Tracks temporary file paths so they can be removed on abnormal termination
//! (e.g., SIGKILL). Normal exits clean up via RAII (`TempFileGuard::drop`),
//! but `Drop` is not invoked on SIGKILL. This registry provides a process-wide
//! list that a signal handler or atexit hook can sweep.
//!
//! # Thread Safety
//!
//! All methods are thread-safe. The internal state is protected by a mutex.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

/// Global cleanup manager instance.
static CLEANUP_MANAGER: OnceLock<Mutex<CleanupManagerState>> = OnceLock::new();

/// Singleton instance returned by [`CleanupManager::global`].
static CLEANUP_MANAGER_INSTANCE: CleanupManager = CleanupManager;

/// Global registry for temporary files awaiting cleanup.
///
/// Register a temp file path after creation and unregister it after a
/// successful commit (rename to final destination). On abnormal shutdown
/// the signal handler calls [`cleanup`](Self::cleanup) to sweep any paths
/// still registered.
///
/// # Examples
///
/// ```
/// use engine::CleanupManager;
/// use std::path::PathBuf;
///
/// let temp = PathBuf::from("/tmp/rsync.12345.tmp");
/// CleanupManager::global().register_temp_file(temp.clone());
///
/// // ... write data, commit ...
///
/// CleanupManager::global().unregister_temp_file(&temp);
/// ```
#[derive(Debug)]
pub struct CleanupManager;

impl CleanupManager {
    /// Returns a reference to the global cleanup manager.
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
    /// Call this after a successful commit so the file is not deleted
    /// during shutdown cleanup.
    pub fn unregister_temp_file(&self, path: &Path) {
        if let Some(state) = CLEANUP_MANAGER.get() {
            if let Ok(mut state) = state.lock() {
                state.temp_files.remove(path);
            }
        }
    }

    /// Registers a cleanup callback to run on shutdown.
    ///
    /// Callbacks execute in reverse registration order (LIFO).
    pub fn register_cleanup(&self, callback: Box<dyn FnOnce() + Send>) {
        if let Some(state) = CLEANUP_MANAGER.get() {
            if let Ok(mut state) = state.lock() {
                state.cleanup_callbacks.push(callback);
            }
        }
    }

    /// Performs cleanup of all registered resources.
    ///
    /// Runs cleanup callbacks in reverse order, then deletes all registered
    /// temporary files. Errors are silently ignored to avoid cascading
    /// failures during shutdown.
    pub fn cleanup(&self) {
        if let Some(state) = CLEANUP_MANAGER.get() {
            if let Ok(mut state) = state.lock() {
                state.cleanup();
            }
        }
    }

    /// Cleans up only the registered temporary files.
    pub fn cleanup_temp_files(&self) {
        if let Some(state) = CLEANUP_MANAGER.get() {
            if let Ok(mut state) = state.lock() {
                state.cleanup_temp_files();
            }
        }
    }

    /// Returns the number of registered temporary files.
    #[must_use]
    pub fn temp_file_count(&self) -> usize {
        if let Some(state) = CLEANUP_MANAGER.get() {
            if let Ok(state) = state.lock() {
                return state.temp_files.len();
            }
        }
        0
    }

    /// Clears all registered resources without performing cleanup.
    ///
    /// Intended for test isolation only.
    #[doc(hidden)]
    pub fn reset_for_testing(&self) {
        if let Some(state) = CLEANUP_MANAGER.get() {
            if let Ok(mut state) = state.lock() {
                state.temp_files.clear();
                state.cleanup_callbacks.clear();
            }
        }
    }
}

/// Internal state for the cleanup manager.
struct CleanupManagerState {
    temp_files: HashSet<PathBuf>,
    cleanup_callbacks: Vec<Box<dyn FnOnce() + Send>>,
}

impl CleanupManagerState {
    fn new() -> Self {
        Self {
            temp_files: HashSet::new(),
            cleanup_callbacks: Vec::new(),
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

    static TEST_LOCK: StdMutex<()> = StdMutex::new(());

    #[test]
    fn register_and_unregister_temp_file() {
        let _lock = TEST_LOCK.lock().unwrap();
        let manager = CleanupManager::global();
        manager.reset_for_testing();

        let path = PathBuf::from("/tmp/test_engine_register_unregister.tmp");

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
        let path1 = dir.path().join("test1_engine_cleanup.tmp");
        let path2 = dir.path().join("test2_engine_cleanup.tmp");

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
    fn cleanup_callbacks_run_in_reverse_order() {
        let _lock = TEST_LOCK.lock().unwrap();
        let manager = CleanupManager::global();
        manager.reset_for_testing();

        let order = Arc::new(Mutex::new(Vec::new()));

        let o1 = Arc::clone(&order);
        manager.register_cleanup(Box::new(move || o1.lock().unwrap().push(1)));
        let o2 = Arc::clone(&order);
        manager.register_cleanup(Box::new(move || o2.lock().unwrap().push(2)));
        let o3 = Arc::clone(&order);
        manager.register_cleanup(Box::new(move || o3.lock().unwrap().push(3)));

        manager.cleanup();
        assert_eq!(*order.lock().unwrap(), vec![3, 2, 1]);
    }

    #[test]
    fn unregister_prevents_cleanup() {
        let _lock = TEST_LOCK.lock().unwrap();
        let manager = CleanupManager::global();
        manager.reset_for_testing();

        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("test_engine_unregister.tmp");
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

        let path = PathBuf::from("/tmp/test_engine_multiple.tmp");
        manager.register_temp_file(path.clone());
        manager.register_temp_file(path.clone());
        manager.register_temp_file(path);

        assert_eq!(manager.temp_file_count(), 1, "HashSet deduplicates");
    }

    #[test]
    fn cleanup_runs_callbacks_and_removes_files() {
        let _lock = TEST_LOCK.lock().unwrap();
        let manager = CleanupManager::global();
        manager.reset_for_testing();

        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("test_engine_cleanup_all.tmp");
        fs::write(&path, b"data").expect("write file");

        manager.register_temp_file(path.clone());

        let ran = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&ran);
        manager.register_cleanup(Box::new(move || flag.store(true, Ordering::SeqCst)));

        manager.cleanup();

        assert!(ran.load(Ordering::SeqCst));
        assert!(!path.exists());
        assert_eq!(manager.temp_file_count(), 0);
    }
}
