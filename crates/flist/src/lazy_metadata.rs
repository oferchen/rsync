//! Lazy metadata loading for deferred stat() calls.
//!
//! This module provides [`LazyMetadata`] which defers filesystem metadata
//! fetching until the data is actually needed. This optimization is useful
//! when building file lists where many entries may be filtered out before
//! their metadata is required.
//!
//! # Design
//!
//! The pattern follows rsync's approach of deferring work until necessary.
//! During directory enumeration, only the path is recorded. The expensive
//! `stat()` syscall is deferred until the metadata is actually accessed.
//!
//! # Example
//!
//! ```ignore
//! use flist::lazy_metadata::LazyMetadata;
//! use std::path::PathBuf;
//!
//! let mut meta = LazyMetadata::new(PathBuf::from("/some/file"), false);
//! assert!(!meta.is_resolved());
//!
//! // stat() call happens here
//! if let Ok(metadata) = meta.get() {
//!     println!("File size: {}", metadata.len());
//! }
//! ```

use std::fs;
use std::io;
use std::path::PathBuf;

/// Lazy wrapper for filesystem metadata.
///
/// Defers the `stat()` syscall until [`get()`](Self::get) is called,
/// then caches the result for subsequent accesses.
#[derive(Debug)]
pub enum LazyMetadata {
    /// Metadata not yet fetched; contains path and follow_symlinks flag.
    Pending {
        /// Path to the file.
        path: PathBuf,
        /// Whether to follow symlinks when fetching metadata.
        follow_symlinks: bool,
    },
    /// Metadata successfully fetched and cached.
    Resolved(fs::Metadata),
    /// Metadata fetch failed; error is cached.
    Error(io::Error),
}

impl LazyMetadata {
    /// Creates a new lazy metadata wrapper.
    ///
    /// No filesystem access occurs until [`get()`](Self::get) is called.
    ///
    /// # Arguments
    ///
    /// * `path` - Path to the file.
    /// * `follow_symlinks` - If true, uses `fs::metadata()` (follows symlinks).
    ///   If false, uses `fs::symlink_metadata()` (does not follow symlinks).
    #[must_use]
    pub fn new(path: PathBuf, follow_symlinks: bool) -> Self {
        Self::Pending {
            path,
            follow_symlinks,
        }
    }

    /// Creates a lazy metadata wrapper that is already resolved.
    ///
    /// Useful when metadata is already available (e.g., from `DirEntry`).
    #[must_use]
    pub fn from_metadata(metadata: fs::Metadata) -> Self {
        Self::Resolved(metadata)
    }

    /// Returns true if metadata has been fetched or an error occurred.
    #[must_use]
    pub fn is_resolved(&self) -> bool {
        !matches!(self, Self::Pending { .. })
    }

    /// Returns true if metadata fetch resulted in an error.
    #[must_use]
    pub fn is_error(&self) -> bool {
        matches!(self, Self::Error(_))
    }

    /// Gets the metadata, fetching it if not yet resolved.
    ///
    /// On first call, performs the `stat()` syscall and caches the result.
    /// Subsequent calls return the cached value without filesystem access.
    ///
    /// # Errors
    ///
    /// Returns the cached error if metadata fetch previously failed.
    pub fn get(&mut self) -> Result<&fs::Metadata, &io::Error> {
        // Resolve if pending
        if let Self::Pending {
            path,
            follow_symlinks,
        } = self
        {
            let result = if *follow_symlinks {
                fs::metadata(&path)
            } else {
                fs::symlink_metadata(&path)
            };

            *self = match result {
                Ok(metadata) => Self::Resolved(metadata),
                Err(error) => Self::Error(error),
            };
        }

        // Return cached result
        match self {
            Self::Resolved(metadata) => Ok(metadata),
            Self::Error(error) => Err(error),
            Self::Pending { .. } => unreachable!(),
        }
    }

    /// Gets the metadata if already resolved, without triggering a fetch.
    ///
    /// Returns `None` if metadata has not been fetched yet.
    #[must_use]
    pub fn get_if_resolved(&self) -> Option<Result<&fs::Metadata, &io::Error>> {
        match self {
            Self::Pending { .. } => None,
            Self::Resolved(metadata) => Some(Ok(metadata)),
            Self::Error(error) => Some(Err(error)),
        }
    }

    /// Returns the path if still pending, or None if already resolved.
    #[must_use]
    pub fn pending_path(&self) -> Option<&PathBuf> {
        match self {
            Self::Pending { path, .. } => Some(path),
            _ => None,
        }
    }

    /// Consumes self and returns the resolved metadata.
    ///
    /// Fetches metadata if not yet resolved.
    ///
    /// # Errors
    ///
    /// Returns the error if metadata fetch failed.
    pub fn into_metadata(mut self) -> Result<fs::Metadata, io::Error> {
        // Ensure resolved
        let _ = self.get();

        match self {
            Self::Resolved(metadata) => Ok(metadata),
            Self::Error(error) => Err(error),
            Self::Pending { .. } => unreachable!(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use tempfile::TempDir;

    fn create_test_file() -> (TempDir, PathBuf) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("test.txt");
        File::create(&path).unwrap();
        (dir, path)
    }

    #[test]
    fn test_lazy_metadata_deferred() {
        let (_dir, path) = create_test_file();
        let meta = LazyMetadata::new(path, false);
        assert!(!meta.is_resolved());
    }

    #[test]
    fn test_lazy_metadata_resolves_on_get() {
        let (_dir, path) = create_test_file();
        let mut meta = LazyMetadata::new(path, false);

        assert!(!meta.is_resolved());
        let result = meta.get();
        assert!(result.is_ok());
        assert!(meta.is_resolved());
    }

    #[test]
    fn test_lazy_metadata_caches_result() {
        let (_dir, path) = create_test_file();
        let mut meta = LazyMetadata::new(path, false);

        // First call - get the length
        let len1 = {
            let result = meta.get();
            assert!(result.is_ok());
            result.unwrap().len()
        };

        // Second call should return cached value with same length
        let len2 = {
            let result = meta.get();
            assert!(result.is_ok());
            result.unwrap().len()
        };

        // Both should have the same metadata (same length)
        assert_eq!(len1, len2);
    }

    #[test]
    fn test_lazy_metadata_error_handling() {
        let mut meta = LazyMetadata::new(PathBuf::from("/nonexistent/path/to/file"), false);

        let result = meta.get();
        assert!(result.is_err());
        assert!(meta.is_error());

        // Error is cached
        let result2 = meta.get();
        assert!(result2.is_err());
    }

    #[test]
    fn test_lazy_metadata_follow_symlinks() {
        let dir = TempDir::new().unwrap();
        let file_path = dir.path().join("target.txt");
        File::create(&file_path).unwrap();

        #[cfg(unix)]
        {
            let link_path = dir.path().join("link.txt");
            std::os::unix::fs::symlink(&file_path, &link_path).unwrap();

            // With follow_symlinks = false, should get symlink metadata
            let mut meta_no_follow = LazyMetadata::new(link_path.clone(), false);
            let result = meta_no_follow.get().unwrap();
            assert!(result.file_type().is_symlink());

            // With follow_symlinks = true, should get target metadata
            let mut meta_follow = LazyMetadata::new(link_path, true);
            let result = meta_follow.get().unwrap();
            assert!(result.file_type().is_file());
        }
    }

    #[test]
    fn test_from_metadata() {
        let (_dir, path) = create_test_file();
        let metadata = fs::metadata(&path).unwrap();
        let lazy = LazyMetadata::from_metadata(metadata);

        assert!(lazy.is_resolved());
    }

    #[test]
    fn test_get_if_resolved() {
        let (_dir, path) = create_test_file();
        let mut meta = LazyMetadata::new(path, false);

        // Not resolved yet
        assert!(meta.get_if_resolved().is_none());

        // Resolve it
        let _ = meta.get();

        // Now should return Some
        assert!(meta.get_if_resolved().is_some());
    }

    #[test]
    fn test_pending_path() {
        let path = PathBuf::from("/some/path");
        let meta = LazyMetadata::new(path.clone(), false);

        assert_eq!(meta.pending_path(), Some(&path));
    }

    #[test]
    fn test_into_metadata() {
        let (_dir, path) = create_test_file();
        let meta = LazyMetadata::new(path, false);

        let result = meta.into_metadata();
        assert!(result.is_ok());
    }

    #[test]
    fn test_into_metadata_error() {
        let meta = LazyMetadata::new(PathBuf::from("/nonexistent"), false);
        let result = meta.into_metadata();
        assert!(result.is_err());
    }
}
