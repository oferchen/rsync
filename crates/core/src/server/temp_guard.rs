//! RAII guard for temporary file cleanup
//!
//! This module provides a `TempFileGuard` that ensures temporary files are
//! automatically deleted when they go out of scope, preventing resource leaks
//! from error paths.

use std::path::{Path, PathBuf};

/// RAII guard that ensures temp files are deleted on drop.
///
/// This guard provides automatic cleanup of temporary files created during
/// delta transfer operations. By default, the temp file is deleted when the
/// guard is dropped (e.g., on error or panic). If the operation succeeds,
/// call `keep()` to prevent deletion.
///
/// # Examples
///
/// ```
/// use std::path::PathBuf;
/// # use core::server::temp_guard::TempFileGuard;
///
/// # fn example() -> std::io::Result<()> {
/// let temp_path = PathBuf::from("/tmp/file.oc-rsync.tmp");
/// let mut guard = TempFileGuard::new(temp_path.clone());
///
/// // Write to temp file
/// std::fs::write(guard.path(), b"data")?;
///
/// // If successful, keep the file
/// guard.keep();
/// # Ok(())
/// # }
/// ```
///
/// # Drop Behavior
///
/// If `keep()` has not been called, the temp file is automatically removed
/// on drop. Removal errors are silently ignored since the file might not
/// exist or might already be deleted.
#[derive(Debug)]
pub struct TempFileGuard {
    path: PathBuf,
    keep_on_drop: bool,
}

impl TempFileGuard {
    /// Create a new guard for the given temp file path.
    ///
    /// The temp file will be deleted on drop unless `keep()` is called.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::path::PathBuf;
    /// # use core::server::temp_guard::TempFileGuard;
    ///
    /// let guard = TempFileGuard::new(PathBuf::from("/tmp/file.tmp"));
    /// // Temp file will be deleted when guard goes out of scope
    /// ```
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            keep_on_drop: false,
        }
    }

    /// Mark the temp file as successful - don't delete on drop.
    ///
    /// Call this after successfully completing the operation that created
    /// the temp file. The file will persist after the guard is dropped.
    ///
    /// # Examples
    ///
    /// ```
    /// # use std::path::PathBuf;
    /// # use core::server::temp_guard::TempFileGuard;
    /// # fn example() -> std::io::Result<()> {
    /// let mut guard = TempFileGuard::new(PathBuf::from("/tmp/file.tmp"));
    ///
    /// // ... perform operations ...
    ///
    /// // Success! Keep the file
    /// guard.keep();
    /// # Ok(())
    /// # }
    /// ```
    pub fn keep(&mut self) {
        self.keep_on_drop = true;
    }

    /// Get the path to the temp file.
    ///
    /// # Examples
    ///
    /// ```
    /// # use std::path::PathBuf;
    /// # use core::server::temp_guard::TempFileGuard;
    /// let guard = TempFileGuard::new(PathBuf::from("/tmp/file.tmp"));
    /// assert_eq!(guard.path(), std::path::Path::new("/tmp/file.tmp"));
    /// ```
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempFileGuard {
    fn drop(&mut self) {
        if !self.keep_on_drop {
            // Best-effort cleanup - ignore errors since:
            // 1. File might not exist (never created)
            // 2. File might already be deleted (renamed away)
            // 3. We're in a drop context (can't propagate errors)
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn temp_file_deleted_on_drop() {
        let dir = tempdir().expect("create temp dir");
        let temp_path = dir.path().join("test.tmp");

        // Create temp file
        fs::write(&temp_path, b"test data").expect("write temp file");
        assert!(temp_path.exists());

        {
            let _guard = TempFileGuard::new(temp_path.clone());
            // Guard goes out of scope here, should delete file
        }

        // File should be deleted
        assert!(!temp_path.exists());
    }

    #[test]
    fn temp_file_kept_when_keep_called() {
        let dir = tempdir().expect("create temp dir");
        let temp_path = dir.path().join("test.tmp");

        // Create temp file
        fs::write(&temp_path, b"test data").expect("write temp file");
        assert!(temp_path.exists());

        {
            let mut guard = TempFileGuard::new(temp_path.clone());
            guard.keep(); // Mark as successful
                          // Guard goes out of scope here
        }

        // File should still exist
        assert!(temp_path.exists());
    }

    #[test]
    fn temp_file_deleted_on_panic() {
        let dir = tempdir().expect("create temp dir");
        let temp_path = dir.path().join("test.tmp");

        // Create temp file
        fs::write(&temp_path, b"test data").expect("write temp file");
        assert!(temp_path.exists());

        let result = std::panic::catch_unwind(|| {
            let _guard = TempFileGuard::new(temp_path.clone());
            panic!("simulated panic");
        });

        assert!(result.is_err());
        // File should be deleted even after panic
        assert!(!temp_path.exists());
    }

    #[test]
    fn temp_file_deleted_on_error_return() {
        let dir = tempdir().expect("create temp dir");
        let temp_path = dir.path().join("test.tmp");

        // Create temp file
        fs::write(&temp_path, b"test data").expect("write temp file");
        assert!(temp_path.exists());

        fn operation_that_fails(path: PathBuf) -> Result<(), std::io::Error> {
            let _guard = TempFileGuard::new(path);
            // Simulate error
            Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                "operation failed",
            ))
        }

        let result = operation_that_fails(temp_path.clone());
        assert!(result.is_err());

        // File should be deleted on error return
        assert!(!temp_path.exists());
    }

    #[test]
    fn path_returns_correct_path() {
        let temp_path = PathBuf::from("/tmp/test.tmp");
        let guard = TempFileGuard::new(temp_path.clone());
        assert_eq!(guard.path(), Path::new("/tmp/test.tmp"));
    }

    #[test]
    fn guard_handles_nonexistent_file() {
        let temp_path = PathBuf::from("/tmp/nonexistent.tmp");

        // Guard should not panic even if file doesn't exist
        {
            let _guard = TempFileGuard::new(temp_path.clone());
            // File never created - drop should not panic
        }
    }
}
