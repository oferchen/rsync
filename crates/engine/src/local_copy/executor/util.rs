//! Shared filesystem helpers for local copy execution.

use std::fs;
use std::path::Path;

use crate::local_copy::LocalCopyError;

pub(crate) fn non_empty_path(path: &Path) -> Option<&Path> {
    if path.as_os_str().is_empty() {
        None
    } else {
        Some(path)
    }
}

pub(crate) fn follow_symlink_metadata(path: &Path) -> Result<fs::Metadata, LocalCopyError> {
    fs::metadata(path)
        .map_err(|error| LocalCopyError::io("inspect symlink target", path.to_path_buf(), error))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn non_empty_path_returns_none_for_empty_path() {
        let empty = Path::new("");
        assert!(non_empty_path(empty).is_none());
    }

    #[test]
    fn non_empty_path_returns_some_for_regular_path() {
        let path = Path::new("/some/path");
        assert_eq!(non_empty_path(path), Some(path));
    }

    #[test]
    fn non_empty_path_returns_some_for_single_char_path() {
        let path = Path::new("a");
        assert_eq!(non_empty_path(path), Some(path));
    }

    #[test]
    fn non_empty_path_returns_some_for_dot() {
        let path = Path::new(".");
        assert_eq!(non_empty_path(path), Some(path));
    }

    #[test]
    fn non_empty_path_returns_some_for_root() {
        let path = Path::new("/");
        assert_eq!(non_empty_path(path), Some(path));
    }

    #[test]
    fn non_empty_path_returns_some_for_relative_path() {
        let path = Path::new("relative/path/to/file.txt");
        assert_eq!(non_empty_path(path), Some(path));
    }

    #[test]
    fn follow_symlink_metadata_returns_error_for_nonexistent_path() {
        let result = follow_symlink_metadata(Path::new("/nonexistent/path/12345"));
        assert!(result.is_err());
    }

    #[test]
    fn follow_symlink_metadata_returns_ok_for_existing_file() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("test.txt");
        std::fs::write(&path, b"content").expect("write");

        let result = follow_symlink_metadata(&path);
        assert!(result.is_ok());
        let metadata = result.unwrap();
        assert!(metadata.is_file());
    }

    #[test]
    fn follow_symlink_metadata_returns_ok_for_directory() {
        let temp = tempfile::tempdir().expect("tempdir");
        let result = follow_symlink_metadata(temp.path());
        assert!(result.is_ok());
        let metadata = result.unwrap();
        assert!(metadata.is_dir());
    }
}
