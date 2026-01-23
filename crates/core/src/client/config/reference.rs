// Re-export reference directory types from engine.
// Engine is the canonical source of these types, used by both local and remote transfers.
pub use engine::{ReferenceDirectory, ReferenceDirectoryKind};

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    mod reference_directory_kind_tests {
        use super::*;

        #[test]
        fn clone_and_copy() {
            let kind = ReferenceDirectoryKind::Compare;
            let cloned = kind;
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
            assert_eq!(
                ReferenceDirectoryKind::Compare,
                ReferenceDirectoryKind::Compare
            );
            assert_eq!(ReferenceDirectoryKind::Copy, ReferenceDirectoryKind::Copy);
            assert_eq!(ReferenceDirectoryKind::Link, ReferenceDirectoryKind::Link);
            assert_ne!(
                ReferenceDirectoryKind::Compare,
                ReferenceDirectoryKind::Copy
            );
            assert_ne!(ReferenceDirectoryKind::Copy, ReferenceDirectoryKind::Link);
            assert_ne!(
                ReferenceDirectoryKind::Compare,
                ReferenceDirectoryKind::Link
            );
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

            let debug = format!("{dir:?}");
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
            assert_ne!(dir1, dir3); // Different kind
            assert_ne!(dir1, dir4); // Different path
        }
    }
}
