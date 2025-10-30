use std::path::{Path, PathBuf};

/// Identifies the strategy applied to a reference directory entry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReferenceDirectoryKind {
    /// Skip creating the destination when the referenced file matches.
    Compare,
    /// Copy data from the reference directory when the file matches.
    Copy,
    /// Create a hard link to the reference directory when the file matches.
    Link,
}

/// Describes a reference directory consulted during local copy execution.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReferenceDirectory {
    kind: ReferenceDirectoryKind,
    path: PathBuf,
}

impl ReferenceDirectory {
    /// Creates a new reference directory entry.
    #[must_use]
    pub fn new(kind: ReferenceDirectoryKind, path: impl Into<PathBuf>) -> Self {
        Self {
            kind,
            path: path.into(),
        }
    }

    /// Returns the kind associated with the reference directory entry.
    #[must_use]
    pub const fn kind(&self) -> ReferenceDirectoryKind {
        self.kind
    }

    /// Returns the base path of the reference directory.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}
