//! Lazy file list entry with deferred metadata loading.
//!
//! This module provides [`LazyFileListEntry`] which stores path information
//! without immediately fetching metadata. This enables efficient filtering
//! before incurring the cost of `stat()` syscalls.
//!
//! # Design
//!
//! The entry stores only path and depth information initially. Metadata is
//! fetched lazily when [`metadata()`](LazyFileListEntry::metadata) is called,
//! then cached for subsequent accesses.
//!
//! # Example
//!
//! ```ignore
//! use flist::lazy_entry::LazyFileListEntry;
//! use std::path::PathBuf;
//!
//! let entry = LazyFileListEntry::new(
//!     PathBuf::from("/full/path"),
//!     PathBuf::from("relative"),
//!     1,  // depth
//!     false,  // is_root
//!     false,  // follow_symlinks
//! );
//!
//! // Filter by path without stat() call
//! if entry.relative_path().extension() == Some(std::ffi::OsStr::new("tmp")) {
//!     return; // Skip without metadata fetch
//! }
//!
//! // Fetch metadata only for non-filtered entries
//! if let Ok(metadata) = entry.metadata() {
//!     println!("Size: {}", metadata.len());
//! }
//! ```

use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::entry::FileListEntry;
use crate::lazy_metadata::LazyMetadata;

/// A file list entry with lazy metadata loading.
///
/// Stores path information immediately but defers metadata fetching until
/// explicitly requested. This allows efficient filtering by path before
/// incurring the overhead of filesystem metadata queries.
#[derive(Debug)]
pub struct LazyFileListEntry {
    /// Absolute path to the file.
    full_path: PathBuf,
    /// Path relative to traversal root.
    relative_path: PathBuf,
    /// Lazy metadata wrapper.
    metadata: LazyMetadata,
    /// Depth from traversal root (root = 0).
    depth: usize,
    /// Whether this is the traversal root entry.
    is_root: bool,
}

impl LazyFileListEntry {
    /// Creates a new lazy file list entry.
    ///
    /// No filesystem access occurs until [`metadata()`](Self::metadata) is called.
    ///
    /// # Arguments
    ///
    /// * `full_path` - Absolute path to the file.
    /// * `relative_path` - Path relative to traversal root.
    /// * `depth` - Depth from root (root = 0, direct children = 1).
    /// * `is_root` - Whether this is the traversal root.
    /// * `follow_symlinks` - Whether to follow symlinks when fetching metadata.
    #[must_use]
    pub fn new(
        full_path: PathBuf,
        relative_path: PathBuf,
        depth: usize,
        is_root: bool,
        follow_symlinks: bool,
    ) -> Self {
        Self {
            metadata: LazyMetadata::new(full_path.clone(), follow_symlinks),
            full_path,
            relative_path,
            depth,
            is_root,
        }
    }

    /// Creates a lazy entry with pre-resolved metadata.
    ///
    /// Useful when metadata is already available (e.g., from `DirEntry`).
    #[must_use]
    pub fn with_metadata(
        full_path: PathBuf,
        relative_path: PathBuf,
        metadata: fs::Metadata,
        depth: usize,
        is_root: bool,
    ) -> Self {
        Self {
            full_path,
            relative_path,
            metadata: LazyMetadata::from_metadata(metadata),
            depth,
            is_root,
        }
    }

    /// Returns the absolute path to the filesystem entry.
    #[must_use]
    pub fn full_path(&self) -> &Path {
        &self.full_path
    }

    /// Returns the path relative to the traversal root.
    #[must_use]
    pub fn relative_path(&self) -> &Path {
        &self.relative_path
    }

    /// Returns the file name, if not the root entry.
    #[must_use]
    pub fn file_name(&self) -> Option<&OsStr> {
        if self.is_root {
            None
        } else {
            self.relative_path.file_name()
        }
    }

    /// Returns the depth relative to the root (root = 0).
    #[must_use]
    pub const fn depth(&self) -> usize {
        self.depth
    }

    /// Returns true if this is the traversal root entry.
    #[must_use]
    pub const fn is_root(&self) -> bool {
        self.is_root
    }

    /// Returns true if metadata has been fetched.
    #[must_use]
    pub fn is_resolved(&self) -> bool {
        self.metadata.is_resolved()
    }

    /// Gets the metadata, fetching it if not yet resolved.
    ///
    /// # Errors
    ///
    /// Returns an error if metadata cannot be fetched.
    pub fn metadata(&mut self) -> Result<&fs::Metadata, &io::Error> {
        self.metadata.get()
    }

    /// Gets the metadata if already resolved, without triggering a fetch.
    #[must_use]
    pub fn metadata_if_resolved(&self) -> Option<Result<&fs::Metadata, &io::Error>> {
        self.metadata.get_if_resolved()
    }

    /// Converts this lazy entry into a resolved [`FileListEntry`].
    ///
    /// Fetches metadata if not already resolved.
    ///
    /// # Errors
    ///
    /// Returns an error if metadata cannot be fetched.
    pub fn into_resolved(mut self) -> Result<FileListEntry, io::Error> {
        // Ensure metadata is resolved
        if let Err(e) = self.metadata.get() {
            // Return owned error
            return Err(io::Error::new(e.kind(), e.to_string()));
        }

        // Now we can extract the metadata
        let metadata = self.metadata.into_metadata()?;

        Ok(FileListEntry {
            full_path: self.full_path,
            relative_path: self.relative_path,
            metadata,
            depth: self.depth,
            is_root: self.is_root,
        })
    }

    /// Attempts to convert to [`FileListEntry`] only if already resolved.
    ///
    /// Returns `None` if metadata has not been fetched yet.
    ///
    /// # Errors
    ///
    /// Returns an error if metadata fetch previously failed.
    pub fn try_into_resolved(self) -> Option<Result<FileListEntry, io::Error>> {
        if !self.metadata.is_resolved() {
            return None;
        }

        Some(self.into_resolved())
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
    fn test_new_entry_not_resolved() {
        let (_dir, path) = create_test_file();
        let entry =
            LazyFileListEntry::new(path.clone(), PathBuf::from("test.txt"), 1, false, false);
        assert!(!entry.is_resolved());
    }

    #[test]
    fn test_with_metadata_resolved() {
        let (_dir, path) = create_test_file();
        let metadata = fs::metadata(&path).unwrap();
        let entry = LazyFileListEntry::with_metadata(
            path.clone(),
            PathBuf::from("test.txt"),
            metadata,
            1,
            false,
        );
        assert!(entry.is_resolved());
    }

    #[test]
    fn test_full_path() {
        let (_dir, path) = create_test_file();
        let entry =
            LazyFileListEntry::new(path.clone(), PathBuf::from("test.txt"), 1, false, false);
        assert_eq!(entry.full_path(), &path);
    }

    #[test]
    fn test_relative_path() {
        let (_dir, path) = create_test_file();
        let entry = LazyFileListEntry::new(path, PathBuf::from("subdir/test.txt"), 2, false, false);
        assert_eq!(entry.relative_path(), Path::new("subdir/test.txt"));
    }

    #[test]
    fn test_file_name() {
        let (_dir, path) = create_test_file();
        let entry = LazyFileListEntry::new(path, PathBuf::from("test.txt"), 1, false, false);
        assert_eq!(entry.file_name(), Some(OsStr::new("test.txt")));
    }

    #[test]
    fn test_file_name_none_for_root() {
        let (_dir, path) = create_test_file();
        let entry = LazyFileListEntry::new(
            path,
            PathBuf::new(),
            0,
            true, // is_root
            false,
        );
        assert!(entry.file_name().is_none());
    }

    #[test]
    fn test_depth() {
        let (_dir, path) = create_test_file();
        let entry = LazyFileListEntry::new(path, PathBuf::from("a/b/test.txt"), 3, false, false);
        assert_eq!(entry.depth(), 3);
    }

    #[test]
    fn test_is_root() {
        let (_dir, path) = create_test_file();

        let root_entry = LazyFileListEntry::new(path.clone(), PathBuf::new(), 0, true, false);
        assert!(root_entry.is_root());

        let child_entry = LazyFileListEntry::new(path, PathBuf::from("test.txt"), 1, false, false);
        assert!(!child_entry.is_root());
    }

    #[test]
    fn test_metadata_resolves_on_access() {
        let (_dir, path) = create_test_file();
        let mut entry = LazyFileListEntry::new(path, PathBuf::from("test.txt"), 1, false, false);

        assert!(!entry.is_resolved());
        let result = entry.metadata();
        assert!(result.is_ok());
        assert!(entry.is_resolved());
    }

    #[test]
    fn test_metadata_if_resolved() {
        let (_dir, path) = create_test_file();
        let mut entry = LazyFileListEntry::new(path, PathBuf::from("test.txt"), 1, false, false);

        // Not resolved yet
        assert!(entry.metadata_if_resolved().is_none());

        // Resolve it
        let _ = entry.metadata();

        // Now should return Some
        assert!(entry.metadata_if_resolved().is_some());
    }

    #[test]
    fn test_into_resolved() {
        let (_dir, path) = create_test_file();
        let entry =
            LazyFileListEntry::new(path.clone(), PathBuf::from("test.txt"), 1, false, false);

        let resolved = entry.into_resolved();
        assert!(resolved.is_ok());

        let file_entry = resolved.unwrap();
        assert_eq!(file_entry.full_path(), &path);
        assert!(file_entry.metadata().is_file());
    }

    #[test]
    fn test_into_resolved_error() {
        let entry = LazyFileListEntry::new(
            PathBuf::from("/nonexistent/path"),
            PathBuf::from("nonexistent"),
            1,
            false,
            false,
        );

        let result = entry.into_resolved();
        assert!(result.is_err());
    }

    #[test]
    fn test_try_into_resolved_not_resolved() {
        let (_dir, path) = create_test_file();
        let entry = LazyFileListEntry::new(path, PathBuf::from("test.txt"), 1, false, false);

        // Not resolved, should return None
        assert!(entry.try_into_resolved().is_none());
    }

    #[test]
    fn test_try_into_resolved_already_resolved() {
        let (_dir, path) = create_test_file();
        let metadata = fs::metadata(&path).unwrap();
        let entry =
            LazyFileListEntry::with_metadata(path, PathBuf::from("test.txt"), metadata, 1, false);

        // Already resolved, should return Some
        let result = entry.try_into_resolved();
        assert!(result.is_some());
        assert!(result.unwrap().is_ok());
    }

    #[test]
    fn test_filtering_without_stat() {
        let (_dir, path) = create_test_file();

        // Create entry
        let entry = LazyFileListEntry::new(path, PathBuf::from("test.txt"), 1, false, false);

        // Filter by extension without fetching metadata
        let should_process = entry
            .relative_path()
            .extension()
            .map(|ext| ext != "tmp")
            .unwrap_or(true);

        assert!(should_process);
        // Metadata was never fetched
        assert!(!entry.is_resolved());
    }
}
