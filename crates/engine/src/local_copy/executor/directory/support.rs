use std::cmp::Ordering;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};

use crate::local_copy::LocalCopyError;

#[derive(Debug)]
pub(crate) struct DirectoryEntry {
    pub(crate) file_name: OsString,
    pub(crate) path: PathBuf,
    pub(crate) metadata: fs::Metadata,
}

pub(crate) fn read_directory_entries_sorted(
    path: &Path,
) -> Result<Vec<DirectoryEntry>, LocalCopyError> {
    let mut entries = Vec::new();
    let read_dir = fs::read_dir(path)
        .map_err(|error| LocalCopyError::io("read directory", path.to_path_buf(), error))?;

    for entry in read_dir {
        let entry = entry.map_err(|error| {
            LocalCopyError::io("read directory entry", path.to_path_buf(), error)
        })?;
        let entry_path = entry.path();
        let metadata = fs::symlink_metadata(&entry_path).map_err(|error| {
            LocalCopyError::io("inspect directory entry", entry_path.to_path_buf(), error)
        })?;
        entries.push(DirectoryEntry {
            file_name: entry.file_name(),
            path: entry_path,
            metadata,
        });
    }

    entries.sort_by(|a, b| compare_file_names(&a.file_name, &b.file_name));
    Ok(entries)
}

fn compare_file_names(left: &OsStr, right: &OsStr) -> Ordering {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;

        left.as_bytes().cmp(right.as_bytes())
    }

    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;

        // Direct iterator comparison avoids two Vec allocations per comparison
        left.encode_wide().cmp(right.encode_wide())
    }

    #[cfg(not(any(unix, windows)))]
    {
        left.to_string_lossy().cmp(&right.to_string_lossy())
    }
}

pub(crate) fn is_fifo(file_type: &fs::FileType) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileTypeExt;

        file_type.is_fifo()
    }

    #[cfg(not(unix))]
    {
        let _ = file_type;
        false
    }
}

pub(crate) fn is_device(file_type: &fs::FileType) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileTypeExt;

        file_type.is_char_device() || file_type.is_block_device()
    }

    #[cfg(not(unix))]
    {
        let _ = file_type;
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ==================== compare_file_names tests ====================

    #[test]
    fn compare_file_names_equal() {
        let a = OsStr::new("file.txt");
        let b = OsStr::new("file.txt");
        assert_eq!(compare_file_names(a, b), Ordering::Equal);
    }

    #[test]
    fn compare_file_names_less() {
        let a = OsStr::new("a.txt");
        let b = OsStr::new("b.txt");
        assert_eq!(compare_file_names(a, b), Ordering::Less);
    }

    #[test]
    fn compare_file_names_greater() {
        let a = OsStr::new("z.txt");
        let b = OsStr::new("a.txt");
        assert_eq!(compare_file_names(a, b), Ordering::Greater);
    }

    #[test]
    fn compare_file_names_prefix_is_less() {
        let a = OsStr::new("file");
        let b = OsStr::new("file.txt");
        assert_eq!(compare_file_names(a, b), Ordering::Less);
    }

    #[test]
    fn compare_file_names_empty_is_less() {
        let a = OsStr::new("");
        let b = OsStr::new("a");
        assert_eq!(compare_file_names(a, b), Ordering::Less);
    }

    #[test]
    fn compare_file_names_both_empty() {
        let a = OsStr::new("");
        let b = OsStr::new("");
        assert_eq!(compare_file_names(a, b), Ordering::Equal);
    }

    #[test]
    fn compare_file_names_case_sensitive() {
        let a = OsStr::new("A");
        let b = OsStr::new("a");
        // On Unix, 'A' (65) < 'a' (97) in byte comparison
        #[cfg(unix)]
        assert_eq!(compare_file_names(a, b), Ordering::Less);
    }

    #[test]
    fn compare_file_names_numeric_order() {
        let a = OsStr::new("file1");
        let b = OsStr::new("file2");
        assert_eq!(compare_file_names(a, b), Ordering::Less);
    }

    // ==================== is_fifo tests ====================

    #[test]
    fn is_fifo_returns_false_for_regular_file() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("file.txt");
        std::fs::write(&path, b"content").expect("write");

        let metadata = std::fs::metadata(&path).expect("metadata");
        assert!(!is_fifo(&metadata.file_type()));
    }

    #[test]
    fn is_fifo_returns_false_for_directory() {
        let temp = tempfile::tempdir().expect("tempdir");
        let metadata = std::fs::metadata(temp.path()).expect("metadata");
        assert!(!is_fifo(&metadata.file_type()));
    }

    // ==================== is_device tests ====================

    #[test]
    fn is_device_returns_false_for_regular_file() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("file.txt");
        std::fs::write(&path, b"content").expect("write");

        let metadata = std::fs::metadata(&path).expect("metadata");
        assert!(!is_device(&metadata.file_type()));
    }

    #[test]
    fn is_device_returns_false_for_directory() {
        let temp = tempfile::tempdir().expect("tempdir");
        let metadata = std::fs::metadata(temp.path()).expect("metadata");
        assert!(!is_device(&metadata.file_type()));
    }

    // ==================== DirectoryEntry tests ====================

    #[test]
    fn directory_entry_debug_contains_file_name() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("test.txt");
        std::fs::write(&path, b"content").expect("write");

        let metadata = std::fs::metadata(&path).expect("metadata");
        let entry = DirectoryEntry {
            file_name: OsString::from("test.txt"),
            path: path.clone(),
            metadata,
        };

        let debug = format!("{entry:?}");
        assert!(debug.contains("DirectoryEntry"));
        assert!(debug.contains("test.txt"));
    }

    // ==================== read_directory_entries_sorted tests ====================

    #[test]
    fn read_directory_entries_sorted_empty_dir() {
        let temp = tempfile::tempdir().expect("tempdir");
        let result = read_directory_entries_sorted(temp.path());
        assert!(result.is_ok());
        assert!(result.unwrap().is_empty());
    }

    #[test]
    fn read_directory_entries_sorted_single_file() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("single.txt");
        std::fs::write(&path, b"content").expect("write");

        let result = read_directory_entries_sorted(temp.path());
        assert!(result.is_ok());
        let entries = result.unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].file_name, OsString::from("single.txt"));
    }

    #[test]
    fn read_directory_entries_sorted_multiple_files_sorted() {
        let temp = tempfile::tempdir().expect("tempdir");
        std::fs::write(temp.path().join("c.txt"), b"c").expect("write");
        std::fs::write(temp.path().join("a.txt"), b"a").expect("write");
        std::fs::write(temp.path().join("b.txt"), b"b").expect("write");

        let result = read_directory_entries_sorted(temp.path());
        assert!(result.is_ok());
        let entries = result.unwrap();
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].file_name, OsString::from("a.txt"));
        assert_eq!(entries[1].file_name, OsString::from("b.txt"));
        assert_eq!(entries[2].file_name, OsString::from("c.txt"));
    }

    #[test]
    fn read_directory_entries_sorted_includes_subdirectory() {
        let temp = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir(temp.path().join("subdir")).expect("mkdir");
        std::fs::write(temp.path().join("file.txt"), b"content").expect("write");

        let result = read_directory_entries_sorted(temp.path());
        assert!(result.is_ok());
        let entries = result.unwrap();
        assert_eq!(entries.len(), 2);
        // Check both entries exist (order depends on byte comparison)
        let names: Vec<_> = entries.iter().map(|e| e.file_name.clone()).collect();
        assert!(names.contains(&OsString::from("file.txt")));
        assert!(names.contains(&OsString::from("subdir")));
    }

    #[test]
    fn read_directory_entries_sorted_error_on_nonexistent() {
        let result = read_directory_entries_sorted(Path::new("/nonexistent/path/12345"));
        assert!(result.is_err());
    }

    #[test]
    fn read_directory_entries_sorted_metadata_is_populated() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("file.txt");
        std::fs::write(&path, b"test content").expect("write");

        let result = read_directory_entries_sorted(temp.path());
        assert!(result.is_ok());
        let entries = result.unwrap();
        assert_eq!(entries.len(), 1);

        // Check metadata is valid
        let entry = &entries[0];
        assert!(entry.metadata.is_file());
        assert_eq!(entry.metadata.len(), 12); // "test content" is 12 bytes
    }
}
