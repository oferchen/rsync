use std::error::Error;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

/// Error returned when traversal fails.
#[derive(Debug)]
pub struct FileListError {
    kind: FileListErrorKind,
}

impl FileListError {
    pub(crate) fn new(kind: FileListErrorKind) -> Self {
        Self { kind }
    }

    pub(crate) fn root_metadata(path: PathBuf, source: io::Error) -> Self {
        Self::new(FileListErrorKind::RootMetadata { path, source })
    }

    pub(crate) fn read_dir(path: PathBuf, source: io::Error) -> Self {
        Self::new(FileListErrorKind::ReadDir { path, source })
    }

    pub(crate) fn read_dir_entry(path: PathBuf, source: io::Error) -> Self {
        Self::new(FileListErrorKind::ReadDirEntry { path, source })
    }

    pub(crate) fn metadata(path: PathBuf, source: io::Error) -> Self {
        Self::new(FileListErrorKind::Metadata { path, source })
    }

    pub(crate) fn canonicalize(path: PathBuf, source: io::Error) -> Self {
        Self::new(FileListErrorKind::Canonicalize { path, source })
    }

    /// Returns the specific failure that terminated traversal.
    #[must_use]
    pub fn kind(&self) -> &FileListErrorKind {
        &self.kind
    }

    /// Returns the filesystem path associated with the error.
    ///
    /// The helper mirrors upstream diagnostics, which always include the
    /// offending path in walker failures. Callers can forward the returned path
    /// directly into higher-level error messages without having to pattern match
    /// on [`FileListErrorKind`].
    ///
    /// # Examples
    ///
    /// ```
    /// use flist::FileListBuilder;
    ///
    /// let result = FileListBuilder::new("./definitely_missing_root").build();
    /// let error = match result {
    ///     Ok(_) => panic!("missing root yields error"),
    ///     Err(error) => error,
    /// };
    /// assert!(error.path().ends_with("definitely_missing_root"));
    /// ```
    #[must_use]
    pub fn path(&self) -> &Path {
        self.kind.path()
    }
}

impl fmt::Display for FileListError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.kind {
            FileListErrorKind::RootMetadata { path, source } => {
                write!(
                    f,
                    "failed to inspect traversal root '{}': {}",
                    path.display(),
                    source
                )
            }
            FileListErrorKind::ReadDir { path, source } => {
                write!(
                    f,
                    "failed to read directory '{}': {}",
                    path.display(),
                    source
                )
            }
            FileListErrorKind::ReadDirEntry { path, source } => {
                write!(
                    f,
                    "failed to read entry in '{}': {}",
                    path.display(),
                    source
                )
            }
            FileListErrorKind::Metadata { path, source } => {
                write!(
                    f,
                    "failed to inspect metadata for '{}': {}",
                    path.display(),
                    source
                )
            }
            FileListErrorKind::Canonicalize { path, source } => {
                write!(f, "failed to canonicalize '{}': {}", path.display(), source)
            }
        }
    }
}

impl Error for FileListError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match &self.kind {
            FileListErrorKind::RootMetadata { source, .. }
            | FileListErrorKind::ReadDir { source, .. }
            | FileListErrorKind::ReadDirEntry { source, .. }
            | FileListErrorKind::Metadata { source, .. }
            | FileListErrorKind::Canonicalize { source, .. } => Some(source),
        }
    }
}

/// Classification of traversal failures.
#[derive(Debug)]
pub enum FileListErrorKind {
    /// Failed to query metadata for the traversal root.
    RootMetadata {
        /// Path that failed to provide metadata.
        path: PathBuf,
        /// Underlying error emitted by the operating system.
        source: io::Error,
    },
    /// Failed to read the contents of a directory.
    ReadDir {
        /// Directory whose contents could not be read.
        path: PathBuf,
        /// Underlying error emitted by the operating system.
        source: io::Error,
    },
    /// Failed to obtain a directory entry during iteration.
    ReadDirEntry {
        /// Directory containing the problematic entry.
        path: PathBuf,
        /// Underlying error emitted by the operating system.
        source: io::Error,
    },
    /// Failed to retrieve metadata for an entry.
    Metadata {
        /// Path whose metadata could not be retrieved.
        path: PathBuf,
        /// Underlying error emitted by the operating system.
        source: io::Error,
    },
    /// Failed to canonicalize a directory path while preventing cycles.
    Canonicalize {
        /// Directory path that failed to canonicalize.
        path: PathBuf,
        /// Underlying error emitted by the operating system.
        source: io::Error,
    },
}

impl FileListErrorKind {
    /// Returns the filesystem path tied to the failure.
    #[must_use]
    pub fn path(&self) -> &Path {
        match self {
            FileListErrorKind::RootMetadata { path, .. }
            | FileListErrorKind::ReadDir { path, .. }
            | FileListErrorKind::ReadDirEntry { path, .. }
            | FileListErrorKind::Metadata { path, .. }
            | FileListErrorKind::Canonicalize { path, .. } => path,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn io_failure() -> io::Error {
        io::Error::other("synthetic failure")
    }

    #[test]
    fn constructors_capture_paths_and_sources() {
        let root_path = PathBuf::from("/tmp/root");
        let read_dir_path = PathBuf::from("/tmp/read_dir");
        let entry_path = PathBuf::from("/tmp/entry");
        let metadata_path = PathBuf::from("/tmp/metadata");
        let canonicalize_path = PathBuf::from("/tmp/canonicalize");

        let root = FileListError::root_metadata(root_path.clone(), io_failure());
        let read_dir = FileListError::read_dir(read_dir_path.clone(), io_failure());
        let entry = FileListError::read_dir_entry(entry_path.clone(), io_failure());
        let metadata = FileListError::metadata(metadata_path.clone(), io_failure());
        let canonicalize = FileListError::canonicalize(canonicalize_path.clone(), io_failure());

        assert!(matches!(
            root.kind(),
            FileListErrorKind::RootMetadata { path, .. } if path == &root_path
        ));
        assert!(matches!(
            read_dir.kind(),
            FileListErrorKind::ReadDir { path, .. } if path == &read_dir_path
        ));
        assert!(matches!(
            entry.kind(),
            FileListErrorKind::ReadDirEntry { path, .. } if path == &entry_path
        ));
        assert!(matches!(
            metadata.kind(),
            FileListErrorKind::Metadata { path, .. } if path == &metadata_path
        ));
        assert!(matches!(
            canonicalize.kind(),
            FileListErrorKind::Canonicalize { path, .. } if path == &canonicalize_path
        ));

        for (error, path_fragment, message_fragment) in [
            (root, "root", "inspect traversal root"),
            (read_dir, "read_dir", "read directory"),
            (entry, "entry", "read entry"),
            (metadata, "metadata", "inspect metadata"),
            (canonicalize, "canonicalize", "canonicalize"),
        ] {
            let display = error.to_string();
            assert!(display.contains(message_fragment));
            assert!(display.contains(path_fragment));

            let source = error.source().expect("io::Error should be preserved");
            assert_eq!(source.to_string(), "synthetic failure");

            let path = error.path();
            assert!(path.to_string_lossy().contains(path_fragment));
        }
    }
}
