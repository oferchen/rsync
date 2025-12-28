//! Handling of reference directories and link-dest decisions.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::local_copy::{CopyContext, LocalCopyError, ReferenceDirectoryKind};

use super::{CopyComparison, should_skip_copy};

pub(crate) enum ReferenceDecision {
    Skip,
    Copy(PathBuf),
    Link(PathBuf),
}

pub(crate) fn resolve_reference_candidate(
    base: &Path,
    relative: &Path,
    destination: &Path,
) -> PathBuf {
    if base.is_absolute() {
        base.join(relative)
    } else {
        let mut ancestor = destination.to_path_buf();
        let depth = relative.components().count();
        for _ in 0..depth {
            if !ancestor.pop() {
                break;
            }
        }
        ancestor.join(base).join(relative)
    }
}

pub(crate) struct ReferenceQuery<'a> {
    pub(crate) destination: &'a Path,
    pub(crate) relative: &'a Path,
    pub(crate) source: &'a Path,
    pub(crate) metadata: &'a fs::Metadata,
    pub(crate) size_only: bool,
    pub(crate) ignore_times: bool,
    pub(crate) checksum: bool,
}

pub(crate) fn find_reference_action(
    context: &CopyContext<'_>,
    query: ReferenceQuery<'_>,
) -> Result<Option<ReferenceDecision>, LocalCopyError> {
    let ReferenceQuery {
        destination,
        relative,
        source,
        metadata,
        size_only,
        ignore_times,
        checksum,
    } = query;
    for reference in context.reference_directories() {
        let candidate = resolve_reference_candidate(reference.path(), relative, destination);
        let candidate_metadata = match fs::symlink_metadata(&candidate) {
            Ok(meta) => meta,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(LocalCopyError::io(
                    "inspect reference file",
                    candidate,
                    error,
                ));
            }
        };

        if !candidate_metadata.file_type().is_file() {
            continue;
        }

        if should_skip_copy(CopyComparison {
            source_path: source,
            source: metadata,
            destination_path: &candidate,
            destination: &candidate_metadata,
            size_only,
            ignore_times,
            checksum,
            checksum_algorithm: context.options().checksum_algorithm(),
            modify_window: context.options().modify_window(),
        }) {
            return Ok(Some(match reference.kind() {
                ReferenceDirectoryKind::Compare => ReferenceDecision::Skip,
                ReferenceDirectoryKind::Copy => ReferenceDecision::Copy(candidate),
                ReferenceDirectoryKind::Link => ReferenceDecision::Link(candidate),
            }));
        }
    }

    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ==================== resolve_reference_candidate tests ====================

    #[test]
    fn resolve_absolute_base_ignores_destination() {
        let base = Path::new("/absolute/ref");
        let relative = Path::new("file.txt");
        let destination = Path::new("/some/other/dest");
        let result = resolve_reference_candidate(base, relative, destination);
        assert_eq!(result, PathBuf::from("/absolute/ref/file.txt"));
    }

    #[test]
    fn resolve_absolute_base_with_nested_relative() {
        let base = Path::new("/ref");
        let relative = Path::new("dir/subdir/file.txt");
        let destination = Path::new("/dest");
        let result = resolve_reference_candidate(base, relative, destination);
        assert_eq!(result, PathBuf::from("/ref/dir/subdir/file.txt"));
    }

    #[test]
    fn resolve_relative_base_computes_from_destination() {
        let base = Path::new("../backup");
        let relative = Path::new("file.txt");
        let destination = Path::new("/home/user/dest");
        let result = resolve_reference_candidate(base, relative, destination);
        // destination "/home/user/dest" -> pop 1 level (for relative depth 1) -> "/home/user"
        // then join "../backup" -> "/home/backup"
        // then join "file.txt" -> "/home/backup/file.txt"
        assert_eq!(result, PathBuf::from("/home/user/../backup/file.txt"));
    }

    #[test]
    fn resolve_relative_base_with_deeper_relative_path() {
        let base = Path::new("ref");
        let relative = Path::new("a/b/c/file.txt");
        let destination = Path::new("/x/y/z/dest");
        // depth of relative is 4, so pop 4 levels from destination
        // "/x/y/z/dest" -> "/x/y/z" -> "/x/y" -> "/x" -> "/"
        // then join "ref" -> "/ref"
        // then join "a/b/c/file.txt" -> "/ref/a/b/c/file.txt"
        let result = resolve_reference_candidate(base, relative, destination);
        assert_eq!(result, PathBuf::from("/ref/a/b/c/file.txt"));
    }

    #[test]
    fn resolve_relative_base_single_component() {
        let base = Path::new("backup");
        let relative = Path::new("file.txt");
        let destination = Path::new("/dest/path");
        // depth 1, pop 1 from "/dest/path" -> "/dest"
        // join "backup" -> "/dest/backup"
        // join "file.txt" -> "/dest/backup/file.txt"
        let result = resolve_reference_candidate(base, relative, destination);
        assert_eq!(result, PathBuf::from("/dest/backup/file.txt"));
    }

    #[test]
    fn resolve_empty_relative_path() {
        let base = Path::new("/ref");
        let relative = Path::new("");
        let destination = Path::new("/dest");
        let result = resolve_reference_candidate(base, relative, destination);
        assert_eq!(result, PathBuf::from("/ref"));
    }

    #[test]
    fn resolve_relative_base_with_empty_relative() {
        let base = Path::new("backup");
        let relative = Path::new("");
        let destination = Path::new("/dest");
        // empty relative has 0 components, pop 0 times
        let result = resolve_reference_candidate(base, relative, destination);
        assert_eq!(result, PathBuf::from("/dest/backup"));
    }

    #[test]
    fn resolve_dotdot_in_base() {
        let base = Path::new("/ref/../other");
        let relative = Path::new("file.txt");
        let destination = Path::new("/dest");
        let result = resolve_reference_candidate(base, relative, destination);
        // base is absolute, so just join
        assert_eq!(result, PathBuf::from("/ref/../other/file.txt"));
    }

    // ==================== ReferenceDecision tests ====================

    #[test]
    fn reference_decision_skip_variant() {
        let decision = ReferenceDecision::Skip;
        assert!(matches!(decision, ReferenceDecision::Skip));
    }

    #[test]
    fn reference_decision_copy_variant() {
        let path = PathBuf::from("/some/path");
        let decision = ReferenceDecision::Copy(path.clone());
        match decision {
            ReferenceDecision::Copy(p) => assert_eq!(p, path),
            _ => panic!("Expected Copy variant"),
        }
    }

    #[test]
    fn reference_decision_link_variant() {
        let path = PathBuf::from("/link/target");
        let decision = ReferenceDecision::Link(path.clone());
        match decision {
            ReferenceDecision::Link(p) => assert_eq!(p, path),
            _ => panic!("Expected Link variant"),
        }
    }

    // ==================== ReferenceQuery tests ====================

    #[test]
    fn reference_query_fields_accessible() {
        let dest = PathBuf::from("/dest");
        let rel = PathBuf::from("relative");
        let src = PathBuf::from("/src");
        let meta = fs::metadata(".").unwrap_or_else(|_| fs::metadata("/").unwrap());

        let query = ReferenceQuery {
            destination: &dest,
            relative: &rel,
            source: &src,
            metadata: &meta,
            size_only: true,
            ignore_times: false,
            checksum: true,
        };

        assert_eq!(query.destination, Path::new("/dest"));
        assert_eq!(query.relative, Path::new("relative"));
        assert_eq!(query.source, Path::new("/src"));
        assert!(query.size_only);
        assert!(!query.ignore_times);
        assert!(query.checksum);
    }
}
