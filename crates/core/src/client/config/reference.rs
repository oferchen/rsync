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

#[cfg(test)]
mod tests {
    use super::*;

    mod reference_directory_kind_tests {
        use super::*;

        #[test]
        fn clone_and_copy() {
            let kind = ReferenceDirectoryKind::Compare;
            let cloned = kind.clone();
            let copied = kind;
            assert_eq!(kind, cloned);
            assert_eq!(kind, copied);
        }

        #[test]
        fn debug_format() {
            assert_eq!(format!("{:?}", ReferenceDirectoryKind::Compare), "Compare");
            assert_eq!(format!("{:?}", ReferenceDirectoryKind::Copy), "Copy");
            assert_eq!(format!("{:?}", ReferenceDirectoryKind::Link), "Link");
        }

        #[test]
        fn equality() {
            assert_eq!(ReferenceDirectoryKind::Compare, ReferenceDirectoryKind::Compare);
            assert_eq!(ReferenceDirectoryKind::Copy, ReferenceDirectoryKind::Copy);
            assert_eq!(ReferenceDirectoryKind::Link, ReferenceDirectoryKind::Link);
            assert_ne!(ReferenceDirectoryKind::Compare, ReferenceDirectoryKind::Copy);
            assert_ne!(ReferenceDirectoryKind::Copy, ReferenceDirectoryKind::Link);
            assert_ne!(ReferenceDirectoryKind::Compare, ReferenceDirectoryKind::Link);
        }
    }

    mod reference_directory_tests {
        use super::*;

        #[test]
        fn new_with_string_path() {
            let dir = ReferenceDirectory::new(ReferenceDirectoryKind::Compare, "/tmp/ref");
            assert_eq!(dir.kind(), ReferenceDirectoryKind::Compare);
            assert_eq!(dir.path(), Path::new("/tmp/ref"));
        }

        #[test]
        fn new_with_pathbuf() {
            let path = PathBuf::from("/var/backup");
            let dir = ReferenceDirectory::new(ReferenceDirectoryKind::Copy, path);
            assert_eq!(dir.kind(), ReferenceDirectoryKind::Copy);
            assert_eq!(dir.path(), Path::new("/var/backup"));
        }

        #[test]
        fn new_with_link_kind() {
            let dir = ReferenceDirectory::new(ReferenceDirectoryKind::Link, "/home/user/link-dest");
            assert_eq!(dir.kind(), ReferenceDirectoryKind::Link);
            assert_eq!(dir.path(), Path::new("/home/user/link-dest"));
        }

        #[test]
        fn clone_and_debug() {
            let dir = ReferenceDirectory::new(ReferenceDirectoryKind::Compare, "/test");
            let cloned = dir.clone();
            assert_eq!(dir, cloned);

            let debug = format!("{:?}", dir);
            assert!(debug.contains("ReferenceDirectory"));
            assert!(debug.contains("Compare"));
            assert!(debug.contains("/test"));
        }

        #[test]
        fn equality() {
            let dir1 = ReferenceDirectory::new(ReferenceDirectoryKind::Compare, "/test");
            let dir2 = ReferenceDirectory::new(ReferenceDirectoryKind::Compare, "/test");
            let dir3 = ReferenceDirectory::new(ReferenceDirectoryKind::Copy, "/test");
            let dir4 = ReferenceDirectory::new(ReferenceDirectoryKind::Compare, "/other");

            assert_eq!(dir1, dir2);
            assert_ne!(dir1, dir3);  // Different kind
            assert_ne!(dir1, dir4);  // Different path
        }
    }
}
