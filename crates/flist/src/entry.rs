use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

/// Result of a filesystem traversal step.
#[derive(Debug)]
pub struct FileListEntry {
    pub(crate) full_path: PathBuf,
    pub(crate) relative_path: PathBuf,
    pub(crate) metadata: fs::Metadata,
    pub(crate) depth: usize,
    pub(crate) is_root: bool,
}

impl FileListEntry {
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

    /// Provides access to the [`fs::Metadata`] captured for the entry.
    #[must_use]
    pub fn metadata(&self) -> &fs::Metadata {
        &self.metadata
    }

    /// Returns the file name associated with the entry, if any.
    ///
    /// The root entry of a traversal has no parent directory and therefore
    /// yields `None`. All other entries return the final component of the
    /// relative path, matching upstream rsync's expectations when constructing
    /// file-list nodes.
    ///
    /// # Examples
    ///
    /// ```
    /// use flist::FileListBuilder;
    /// # fn demo() -> Result<(), Box<dyn std::error::Error>> {
    /// let temp = tempfile::tempdir()?;
    /// let root = temp.path().join("root");
    /// std::fs::create_dir(&root)?;
    /// let mut walker = FileListBuilder::new(&root).build()?;
    /// let entry = walker.next().unwrap()?;
    /// assert!(entry.metadata().is_dir());
    /// assert!(entry.file_name().is_none());
    /// # Ok(())
    /// # }
    /// # demo().unwrap();
    /// ```
    #[must_use]
    pub fn file_name(&self) -> Option<&OsStr> {
        if self.is_root {
            None
        } else {
            self.relative_path.file_name()
        }
    }

    /// Reports the depth of the entry relative to the root (root depth is `0`).
    #[must_use]
    pub const fn depth(&self) -> usize {
        self.depth
    }

    /// Indicates whether this entry corresponds to the traversal root.
    #[must_use]
    pub const fn is_root(&self) -> bool {
        self.is_root
    }
}

#[cfg(test)]
mod tests {
    use crate::FileListBuilder;

    #[test]
    fn entry_debug_format() {
        // Create a temporary directory for testing
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let mut walker = FileListBuilder::new(root).build().unwrap();
        let entry = walker.next().unwrap().unwrap();
        let debug = format!("{:?}", entry);
        assert!(debug.contains("FileListEntry"));
    }

    #[test]
    fn full_path_returns_absolute() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let mut walker = FileListBuilder::new(root).build().unwrap();
        let entry = walker.next().unwrap().unwrap();
        assert!(entry.full_path().is_absolute());
    }

    #[test]
    fn relative_path_returns_path() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let mut walker = FileListBuilder::new(root).build().unwrap();
        let entry = walker.next().unwrap().unwrap();
        let _ = entry.relative_path();
    }

    #[test]
    fn metadata_returns_metadata() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let mut walker = FileListBuilder::new(root).build().unwrap();
        let entry = walker.next().unwrap().unwrap();
        assert!(entry.metadata().is_dir());
    }

    #[test]
    fn file_name_none_for_root() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let mut walker = FileListBuilder::new(root).build().unwrap();
        let entry = walker.next().unwrap().unwrap();
        assert!(entry.is_root());
        assert!(entry.file_name().is_none());
    }

    #[test]
    fn depth_zero_for_root() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let mut walker = FileListBuilder::new(root).build().unwrap();
        let entry = walker.next().unwrap().unwrap();
        assert_eq!(entry.depth(), 0);
    }

    #[test]
    fn is_root_true_for_root_entry() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let mut walker = FileListBuilder::new(root).build().unwrap();
        let entry = walker.next().unwrap().unwrap();
        assert!(entry.is_root());
    }

    #[test]
    fn child_entry_has_file_name() {
        let temp = tempfile::tempdir().unwrap();
        let child = temp.path().join("child.txt");
        std::fs::write(&child, "content").unwrap();

        let mut walker = FileListBuilder::new(temp.path()).build().unwrap();
        // Skip root
        let _ = walker.next();
        if let Some(Ok(entry)) = walker.next() {
            assert!(!entry.is_root());
            assert!(entry.file_name().is_some());
        }
    }

    #[test]
    fn child_entry_has_depth_one() {
        let temp = tempfile::tempdir().unwrap();
        let child = temp.path().join("child.txt");
        std::fs::write(&child, "content").unwrap();

        let mut walker = FileListBuilder::new(temp.path()).build().unwrap();
        // Skip root
        let _ = walker.next();
        if let Some(Ok(entry)) = walker.next() {
            assert_eq!(entry.depth(), 1);
        }
    }
}
