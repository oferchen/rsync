use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

/// Result of a filesystem traversal step.
#[derive(Debug)]
pub struct WalkEntry {
    pub(crate) full_path: PathBuf,
    pub(crate) relative_path: PathBuf,
    pub(crate) metadata: fs::Metadata,
    pub(crate) depth: usize,
    pub(crate) is_root: bool,
}

impl WalkEntry {
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
    /// use oc_rsync_walk::WalkBuilder;
    /// # fn demo() -> Result<(), Box<dyn std::error::Error>> {
    /// let temp = tempfile::tempdir()?;
    /// let root = temp.path().join("root");
    /// std::fs::create_dir(&root)?;
    /// let mut walker = WalkBuilder::new(&root).build()?;
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
