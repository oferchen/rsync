use std::error::Error;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

/// Error returned when traversal fails.
#[derive(Debug)]
pub struct WalkError {
    kind: WalkErrorKind,
}

impl WalkError {
    pub(crate) fn new(kind: WalkErrorKind) -> Self {
        Self { kind }
    }

    pub(crate) fn root_metadata(path: PathBuf, source: io::Error) -> Self {
        Self::new(WalkErrorKind::RootMetadata { path, source })
    }

    pub(crate) fn read_dir(path: PathBuf, source: io::Error) -> Self {
        Self::new(WalkErrorKind::ReadDir { path, source })
    }

    pub(crate) fn read_dir_entry(path: PathBuf, source: io::Error) -> Self {
        Self::new(WalkErrorKind::ReadDirEntry { path, source })
    }

    pub(crate) fn metadata(path: PathBuf, source: io::Error) -> Self {
        Self::new(WalkErrorKind::Metadata { path, source })
    }

    pub(crate) fn canonicalize(path: PathBuf, source: io::Error) -> Self {
        Self::new(WalkErrorKind::Canonicalize { path, source })
    }

    /// Returns the specific failure that terminated traversal.
    #[must_use]
    pub fn kind(&self) -> &WalkErrorKind {
        &self.kind
    }

    /// Returns the filesystem path associated with the error.
    ///
    /// The helper mirrors upstream diagnostics, which always include the
    /// offending path in walker failures. Callers can forward the returned path
    /// directly into higher-level error messages without having to pattern match
    /// on [`WalkErrorKind`].
    ///
    /// # Examples
    ///
    /// ```
    /// use rsync_walk::WalkBuilder;
    ///
    /// let result = WalkBuilder::new("./definitely_missing_root").build();
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

impl fmt::Display for WalkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.kind {
            WalkErrorKind::RootMetadata { path, source } => {
                write!(
                    f,
                    "failed to inspect traversal root '{}': {}",
                    path.display(),
                    source
                )
            }
            WalkErrorKind::ReadDir { path, source } => {
                write!(
                    f,
                    "failed to read directory '{}': {}",
                    path.display(),
                    source
                )
            }
            WalkErrorKind::ReadDirEntry { path, source } => {
                write!(
                    f,
                    "failed to read entry in '{}': {}",
                    path.display(),
                    source
                )
            }
            WalkErrorKind::Metadata { path, source } => {
                write!(
                    f,
                    "failed to inspect metadata for '{}': {}",
                    path.display(),
                    source
                )
            }
            WalkErrorKind::Canonicalize { path, source } => {
                write!(f, "failed to canonicalize '{}': {}", path.display(), source)
            }
        }
    }
}

impl Error for WalkError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match &self.kind {
            WalkErrorKind::RootMetadata { source, .. }
            | WalkErrorKind::ReadDir { source, .. }
            | WalkErrorKind::ReadDirEntry { source, .. }
            | WalkErrorKind::Metadata { source, .. }
            | WalkErrorKind::Canonicalize { source, .. } => Some(source),
        }
    }
}

/// Classification of traversal failures.
#[derive(Debug)]
pub enum WalkErrorKind {
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

impl WalkErrorKind {
    /// Returns the filesystem path tied to the failure.
    #[must_use]
    pub fn path(&self) -> &Path {
        match self {
            WalkErrorKind::RootMetadata { path, .. }
            | WalkErrorKind::ReadDir { path, .. }
            | WalkErrorKind::ReadDirEntry { path, .. }
            | WalkErrorKind::Metadata { path, .. }
            | WalkErrorKind::Canonicalize { path, .. } => path,
        }
    }
}
