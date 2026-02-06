//! Cleanup coordination for temporary files and resources.
//!
//! This module provides a global cleanup manager that tracks temporary files,
//! partial transfers, and other resources that need cleanup on shutdown or error.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

/// Global cleanup manager instance.
static CLEANUP_MANAGER: OnceLock<Mutex<CleanupManagerState>> = OnceLock::new();

/// Cleanup manager for tracking and cleaning up temporary resources.
///
/// This type provides a global registry for temporary files and resources
/// that should be cleaned up on shutdown or error. It's designed to work
/// with signal handlers and RAII guards.
///
/// # Thread Safety
///
/// All methods are thread-safe and can be called from multiple threads
/// simultaneously. The internal state is protected by a mutex.
///
/// # Examples
///
/// ```
/// use core::signal::CleanupManager;
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
        // Ensure the cleanup manager is initialized
        let _ = CLEANUP_MANAGER.get_or_init(|| Mutex::new(CleanupManagerState::new()));
        &CLEANUP_MANAGER_INSTANCE
    }

    /// Registers a temporary file for cleanup.
    ///
    /// The file will be deleted when [`cleanup`](Self::cleanup) or
    /// [`cleanup_temp_files`](Self::cleanup_temp_files) is called,
    /// unless it's unregistered first.
    ///
    /// # Examples
    ///
    /// ```
    /// use core::signal::CleanupManager;
    /// use std::path::PathBuf;
    ///
    /// let temp = PathBuf::from("/tmp/transfer.tmp");
    /// CleanupManager::global().register_temp_file(temp);
    /// ```
    pub fn register_temp_file(&self, path: PathBuf) {
        if let Some(state) = CLEANUP_MANAGER.get() {
            if let Ok(mut state) = state.lock() {
                state.temp_files.insert(path);
            }
        }
    }

    /// Unregisters a temporary file from cleanup.
    ///
    /// Call this when a temporary file has been successfully completed
    /// and should not be deleted during cleanup.
    ///
    /// # Examples
    ///
    /// ```
    /// use core::signal::CleanupManager;
    /// use std::path::Path;
    ///
    /// let temp = Path::new("/tmp/transfer.tmp");
    /// CleanupManager::global().unregister_temp_file(temp);
    /// ```
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
    ///
    /// # Examples
    ///
    /// ```
    /// use core::signal::CleanupManager;
    ///
    /// CleanupManager::global().register_cleanup(Box::new(|| {
    ///     println!("Cleanup callback executed");
    /// }));
    /// ```
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
    ///
    /// # Examples
    ///
    /// ```
    /// use core::signal::CleanupManager;
    ///
    /// // In signal handler or error path:
    /// CleanupManager::global().cleanup();
    /// ```
    pub fn cleanup(&self) {
        if let Some(state) = CLEANUP_MANAGER.get() {
            if let Ok(mut state) = state.lock() {
                state.cleanup();
            }
        }
    }

    /// Cleans up only the registered temporary files.
    ///
    /// This is similar to [`cleanup`](Self::cleanup) but only removes
    /// temporary files, without running cleanup callbacks.
    ///
    /// # Examples
    ///
    /// ```
    /// use core::signal::CleanupManager;
    ///
    /// // Clean up temp files only
    /// CleanupManager::global().cleanup_temp_files();
    /// ```
    pub fn cleanup_temp_files(&self) {
        if let Some(state) = CLEANUP_MANAGER.get() {
            if let Ok(mut state) = state.lock() {
                state.cleanup_temp_files();
            }
        }
    }

    /// Returns the number of registered temporary files.
    ///
    /// This is primarily useful for testing and diagnostics.
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
    /// This is primarily useful for testing. In production code, you should
    /// call [`cleanup`](Self::cleanup) instead.
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

/// Singleton instance of the cleanup manager.
static CLEANUP_MANAGER_INSTANCE: CleanupManager = CleanupManager;

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
        // Run cleanup callbacks in reverse order (LIFO)
        while let Some(callback) = self.cleanup_callbacks.pop() {
            callback();
        }

        // Clean up temp files
        self.cleanup_temp_files();
    }

    fn cleanup_temp_files(&mut self) {
        for path in &self.temp_files {
            // Best-effort cleanup - ignore errors
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

        // Create temp files
        fs::write(&path1, b"data1").expect("write file 1");
        fs::write(&path2, b"data2").expect("write file 2");

        manager.register_temp_file(path1.clone());
        manager.register_temp_file(path2.clone());

        assert!(path1.exists());
        assert!(path2.exists());
        assert_eq!(manager.temp_file_count(), 2);

        manager.cleanup_temp_files();

        // Files should be removed
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

        // Should not panic
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
        // Callbacks run in LIFO order (reverse of registration)
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

        // File should still exist because it was unregistered
        assert!(path.exists());
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

        // HashSet should deduplicate
        assert_eq!(manager.temp_file_count(), 1);
    }

    #[test]
    fn global_returns_same_instance() {
        let _lock = TEST_LOCK.lock().unwrap();
        let manager1 = CleanupManager::global();
        let manager2 = CleanupManager::global();

        manager1.reset_for_testing();
        manager1.register_temp_file(PathBuf::from("/tmp/test_global.tmp"));

        // Both references should see the same state
        assert_eq!(manager2.temp_file_count(), 1);
    }
}
