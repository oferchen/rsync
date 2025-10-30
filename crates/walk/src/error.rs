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

#[cfg(test)]
mod tests {
    use super::*;

    fn io_error(message: &'static str) -> io::Error {
        io::Error::new(io::ErrorKind::Other, message)
    }

    #[test]
    fn walk_error_path_matches_variant_path() {
        let root = WalkError::root_metadata(PathBuf::from("root"), io_error("root"));
        assert_eq!(Path::new("root"), root.path());

        let read_dir = WalkError::read_dir(PathBuf::from("dir"), io_error("dir"));
        assert_eq!(Path::new("dir"), read_dir.path());

        let read_dir_entry = WalkError::read_dir_entry(PathBuf::from("entry"), io_error("entry"));
        assert_eq!(Path::new("entry"), read_dir_entry.path());

        let metadata = WalkError::metadata(PathBuf::from("meta"), io_error("meta"));
        assert_eq!(Path::new("meta"), metadata.path());

        let canonicalize = WalkError::canonicalize(PathBuf::from("canon"), io_error("canon"));
        assert_eq!(Path::new("canon"), canonicalize.path());
    }

    #[test]
    fn walk_error_display_is_specific_per_variant() {
        let root = WalkError::root_metadata(PathBuf::from("root"), io_error("boom"));
        assert_eq!(
            "failed to inspect traversal root 'root': boom",
            root.to_string()
        );

        let read_dir = WalkError::read_dir(PathBuf::from("dir"), io_error("boom"));
        assert_eq!(
            "failed to read directory 'dir': boom",
            read_dir.to_string()
        );

        let read_dir_entry = WalkError::read_dir_entry(PathBuf::from("entry"), io_error("boom"));
        assert_eq!(
            "failed to read entry in 'entry': boom",
            read_dir_entry.to_string()
        );

        let metadata = WalkError::metadata(PathBuf::from("meta"), io_error("boom"));
        assert_eq!(
            "failed to inspect metadata for 'meta': boom",
            metadata.to_string()
        );

        let canonicalize = WalkError::canonicalize(PathBuf::from("canon"), io_error("boom"));
        assert_eq!(
            "failed to canonicalize 'canon': boom",
            canonicalize.to_string()
        );
    }

    #[test]
    fn walk_error_kind_accessor_reveals_inner_variant() {
        let metadata = WalkError::metadata(PathBuf::from("meta"), io_error("meta"));
        match metadata.kind() {
            WalkErrorKind::Metadata { path, .. } => assert_eq!(Path::new("meta"), path),
            other => panic!("unexpected error kind: {other:?}"),
        }
    }

    #[test]
    fn walk_error_source_refers_to_underlying_io_error() {
        let error = WalkError::read_dir(PathBuf::from("dir"), io_error("source"));
        let source_ref = error
            .source()
            .and_then(|err| err.downcast_ref::<io::Error>())
            .expect("walk error should expose the underlying io::Error");
        assert_eq!(source_ref.to_string(), "source");
    }
}
