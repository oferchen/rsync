//! Directory entry representation for traversal results.

use std::fs::{FileType, Metadata};
use std::path::{Path, PathBuf};

/// A single entry yielded during directory traversal.
///
/// Contains the path, metadata, and depth information for a file or
/// directory encountered during traversal. The entry provides efficient
/// access to commonly-needed attributes without additional syscalls.
///
/// # Examples
///
/// ```no_run
/// use engine::walk::{WalkConfig, WalkdirWalker};
/// use std::path::Path;
///
/// let walker = WalkdirWalker::new(Path::new("/tmp"), WalkConfig::default());
/// for entry in walker.flatten() {
///     if entry.file_type().is_file() {
///         println!("File: {} ({} bytes)", entry.path().display(), entry.len());
///     }
/// }
/// ```
#[derive(Debug)]
pub struct WalkEntry {
    path: PathBuf,
    metadata: Metadata,
    depth: usize,
}

impl WalkEntry {
    /// Creates a new walk entry from path and metadata.
    pub(crate) fn new(path: PathBuf, metadata: Metadata, depth: usize) -> Self {
        Self {
            path,
            metadata,
            depth,
        }
    }

    /// Returns the full path of this entry.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Consumes the entry and returns the owned path.
    #[must_use]
    pub fn into_path(self) -> PathBuf {
        self.path
    }

    /// Returns the file name of this entry.
    ///
    /// Returns `None` if the path terminates in `..` or is the root.
    #[must_use]
    pub fn file_name(&self) -> Option<&std::ffi::OsStr> {
        self.path.file_name()
    }

    /// Returns the metadata for this entry.
    ///
    /// The metadata is obtained via `symlink_metadata`, so symlinks
    /// report their own metadata rather than their target's.
    #[must_use]
    pub fn metadata(&self) -> &Metadata {
        &self.metadata
    }

    /// Returns the file type for this entry.
    #[must_use]
    pub fn file_type(&self) -> FileType {
        self.metadata.file_type()
    }

    /// Returns `true` if this entry is a directory.
    #[must_use]
    pub fn is_dir(&self) -> bool {
        self.file_type().is_dir()
    }

    /// Returns `true` if this entry is a regular file.
    #[must_use]
    pub fn is_file(&self) -> bool {
        self.file_type().is_file()
    }

    /// Returns `true` if this entry is a symbolic link.
    #[must_use]
    pub fn is_symlink(&self) -> bool {
        self.file_type().is_symlink()
    }

    /// Returns the size in bytes of this entry.
    ///
    /// For directories and symlinks, this is the size of the directory
    /// entry or link itself, not the target's size.
    #[must_use]
    pub fn len(&self) -> u64 {
        self.metadata.len()
    }

    /// Returns `true` if this entry has zero size.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Returns the depth of this entry relative to the root.
    ///
    /// The root has depth 0, its immediate children have depth 1, etc.
    #[must_use]
    pub fn depth(&self) -> usize {
        self.depth
    }

    /// Returns the device ID for this entry (Unix only).
    #[cfg(unix)]
    #[must_use]
    pub fn dev(&self) -> u64 {
        use std::os::unix::fs::MetadataExt;
        self.metadata.dev()
    }

    /// Returns the inode number for this entry (Unix only).
    #[cfg(unix)]
    #[must_use]
    pub fn ino(&self) -> u64 {
        use std::os::unix::fs::MetadataExt;
        self.metadata.ino()
    }

    /// Returns the Unix mode bits for this entry (Unix only).
    #[cfg(unix)]
    #[must_use]
    pub fn mode(&self) -> u32 {
        use std::os::unix::fs::MetadataExt;
        self.metadata.mode()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn create_test_file(dir: &TempDir) -> (PathBuf, Metadata) {
        let path = dir.path().join("test.txt");
        fs::write(&path, b"hello").unwrap();
        let metadata = fs::symlink_metadata(&path).unwrap();
        (path, metadata)
    }

    #[test]
    fn entry_accessors_work() {
        let dir = TempDir::new().unwrap();
        let (path, metadata) = create_test_file(&dir);

        let entry = WalkEntry::new(path.clone(), metadata, 1);

        assert_eq!(entry.path(), path);
        assert_eq!(entry.file_name(), Some(std::ffi::OsStr::new("test.txt")));
        assert!(entry.is_file());
        assert!(!entry.is_dir());
        assert!(!entry.is_symlink());
        assert_eq!(entry.len(), 5);
        assert!(!entry.is_empty());
        assert_eq!(entry.depth(), 1);
    }

    #[test]
    fn into_path_consumes_entry() {
        let dir = TempDir::new().unwrap();
        let (path, metadata) = create_test_file(&dir);
        let expected = path.clone();

        let entry = WalkEntry::new(path, metadata, 0);
        let owned = entry.into_path();

        assert_eq!(owned, expected);
    }

    #[test]
    fn directory_entry_is_dir() {
        let dir = TempDir::new().unwrap();
        let metadata = fs::symlink_metadata(dir.path()).unwrap();

        let entry = WalkEntry::new(dir.path().to_path_buf(), metadata, 0);

        assert!(entry.is_dir());
        assert!(!entry.is_file());
    }

    #[cfg(unix)]
    #[test]
    fn unix_metadata_accessors() {
        let dir = TempDir::new().unwrap();
        let (path, metadata) = create_test_file(&dir);

        let entry = WalkEntry::new(path, metadata, 0);

        assert!(entry.dev() > 0);
        assert!(entry.ino() > 0);
        assert!(entry.mode() > 0);
    }

    #[test]
    fn entry_is_debug() {
        let dir = TempDir::new().unwrap();
        let (path, metadata) = create_test_file(&dir);

        let entry = WalkEntry::new(path, metadata, 0);
        let debug = format!("{entry:?}");

        assert!(debug.contains("WalkEntry"));
        assert!(debug.contains("test.txt"));
    }
}
