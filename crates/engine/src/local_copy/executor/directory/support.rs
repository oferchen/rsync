use std::cmp::Ordering;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};

#[cfg(feature = "parallel")]
use rayon::prelude::*;

use crate::local_copy::LocalCopyError;

#[derive(Debug)]
pub(crate) struct DirectoryEntry {
    pub(crate) file_name: OsString,
    pub(crate) path: PathBuf,
    pub(crate) metadata: fs::Metadata,
}

/// Minimum number of entries to justify parallel metadata fetching.
/// Below this threshold, the overhead of thread synchronization exceeds
/// the benefit of parallelism.
#[cfg(feature = "parallel")]
const PARALLEL_THRESHOLD: usize = 32;

/// Reads directory entries and fetches metadata, using parallel stat calls
/// when the `parallel` feature is enabled and entry count exceeds threshold.
pub(crate) fn read_directory_entries_sorted(
    path: &Path,
) -> Result<Vec<DirectoryEntry>, LocalCopyError> {
    #[cfg(feature = "parallel")]
    {
        read_directory_entries_sorted_parallel(path)
    }
    #[cfg(not(feature = "parallel"))]
    {
        read_directory_entries_sorted_sequential(path)
    }
}

/// Sequential implementation: reads entries and fetches metadata one at a time.
#[cfg(any(not(feature = "parallel"), test))]
fn read_directory_entries_sorted_sequential(
    path: &Path,
) -> Result<Vec<DirectoryEntry>, LocalCopyError> {
    let mut entries = Vec::new();
    let read_dir =
        fs::read_dir(path).map_err(|error| LocalCopyError::io("read directory", path, error))?;

    for entry in read_dir {
        let entry =
            entry.map_err(|error| LocalCopyError::io("read directory entry", path, error))?;
        let entry_path = entry.path();
        let metadata = fs::symlink_metadata(&entry_path).map_err(|error| {
            LocalCopyError::io("inspect directory entry", entry_path.clone(), error)
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

/// Parallel implementation: enumerates paths first, then fetches metadata in parallel.
///
/// This splits the work into two phases:
/// 1. Sequential directory enumeration (read_dir must be sequential)
/// 2. Parallel metadata fetching using rayon's thread pool
///
/// For directories with many entries, this significantly reduces wall-clock time
/// by overlapping multiple stat() syscalls across CPU cores.
#[cfg(feature = "parallel")]
fn read_directory_entries_sorted_parallel(
    path: &Path,
) -> Result<Vec<DirectoryEntry>, LocalCopyError> {
    // Phase 1: Collect paths sequentially (read_dir iteration must be sequential)
    let read_dir =
        fs::read_dir(path).map_err(|error| LocalCopyError::io("read directory", path, error))?;

    let mut pending: Vec<(OsString, PathBuf)> = Vec::new();
    for entry in read_dir {
        let entry =
            entry.map_err(|error| LocalCopyError::io("read directory entry", path, error))?;
        pending.push((entry.file_name(), entry.path()));
    }

    // For small directories, use sequential metadata fetching to avoid overhead
    if pending.len() < PARALLEL_THRESHOLD {
        let mut entries = Vec::with_capacity(pending.len());
        for (file_name, entry_path) in pending {
            let metadata = fs::symlink_metadata(&entry_path).map_err(|error| {
                LocalCopyError::io("inspect directory entry", entry_path.clone(), error)
            })?;
            entries.push(DirectoryEntry {
                file_name,
                path: entry_path,
                metadata,
            });
        }
        entries.sort_by(|a, b| compare_file_names(&a.file_name, &b.file_name));
        return Ok(entries);
    }

    // Phase 2: Fetch metadata in parallel using rayon
    let results: Vec<Result<DirectoryEntry, (PathBuf, std::io::Error)>> = pending
        .into_par_iter()
        .map(|(file_name, entry_path)| match fs::symlink_metadata(&entry_path) {
            Ok(metadata) => Ok(DirectoryEntry {
                file_name,
                path: entry_path,
                metadata,
            }),
            Err(error) => Err((entry_path, error)),
        })
        .collect();

    // Collect results, returning first error encountered
    let mut entries = Vec::with_capacity(results.len());
    for result in results {
        match result {
            Ok(entry) => entries.push(entry),
            Err((entry_path, error)) => {
                return Err(LocalCopyError::io(
                    "inspect directory entry",
                    entry_path,
                    error,
                ));
            }
        }
    }

    // Sort to maintain deterministic ordering (parallel fetch order is non-deterministic)
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

pub(crate) fn is_fifo(file_type: fs::FileType) -> bool {
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

pub(crate) fn is_device(file_type: fs::FileType) -> bool {
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

    #[cfg(unix)]
    #[test]
    fn compare_file_names_case_sensitive() {
        let a = OsStr::new("A");
        let b = OsStr::new("a");
        // On Unix, 'A' (65) < 'a' (97) in byte comparison
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
        assert!(!is_fifo(metadata.file_type()));
    }

    #[test]
    fn is_fifo_returns_false_for_directory() {
        let temp = tempfile::tempdir().expect("tempdir");
        let metadata = std::fs::metadata(temp.path()).expect("metadata");
        assert!(!is_fifo(metadata.file_type()));
    }

    // ==================== is_device tests ====================

    #[test]
    fn is_device_returns_false_for_regular_file() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("file.txt");
        std::fs::write(&path, b"content").expect("write");

        let metadata = std::fs::metadata(&path).expect("metadata");
        assert!(!is_device(metadata.file_type()));
    }

    #[test]
    fn is_device_returns_false_for_directory() {
        let temp = tempfile::tempdir().expect("tempdir");
        let metadata = std::fs::metadata(temp.path()).expect("metadata");
        assert!(!is_device(metadata.file_type()));
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
            path,
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

    // ==================== Parallel implementation tests ====================

    #[cfg(feature = "parallel")]
    #[test]
    fn parallel_and_sequential_produce_identical_results() {
        let temp = tempfile::tempdir().expect("tempdir");

        // Create enough files to trigger parallel path (> PARALLEL_THRESHOLD)
        for i in 0..50 {
            let name = format!("file_{i:03}.txt");
            std::fs::write(temp.path().join(&name), format!("content {i}")).expect("write");
        }
        // Add some directories too
        for i in 0..10 {
            let name = format!("dir_{i:02}");
            std::fs::create_dir(temp.path().join(&name)).expect("mkdir");
        }

        let parallel = super::read_directory_entries_sorted_parallel(temp.path()).unwrap();
        let sequential = read_directory_entries_sorted_sequential(temp.path()).unwrap();

        assert_eq!(parallel.len(), sequential.len());
        for (p, s) in parallel.iter().zip(sequential.iter()) {
            assert_eq!(p.file_name, s.file_name);
            assert_eq!(p.path, s.path);
            assert_eq!(p.metadata.len(), s.metadata.len());
            assert_eq!(p.metadata.is_file(), s.metadata.is_file());
            assert_eq!(p.metadata.is_dir(), s.metadata.is_dir());
        }
    }

    #[cfg(feature = "parallel")]
    #[test]
    fn parallel_small_directory_uses_sequential_path() {
        let temp = tempfile::tempdir().expect("tempdir");

        // Create fewer files than PARALLEL_THRESHOLD
        for i in 0..5 {
            std::fs::write(temp.path().join(format!("file_{i}.txt")), b"content").expect("write");
        }

        // Should still work correctly (uses sequential path internally)
        let result = super::read_directory_entries_sorted_parallel(temp.path());
        assert!(result.is_ok());
        let entries = result.unwrap();
        assert_eq!(entries.len(), 5);
    }

    #[cfg(feature = "parallel")]
    #[test]
    fn parallel_maintains_sort_order() {
        let temp = tempfile::tempdir().expect("tempdir");

        // Create files with names that would be out of order if not sorted
        let names = ["zebra.txt", "apple.txt", "mango.txt", "banana.txt"];
        for name in &names {
            std::fs::write(temp.path().join(name), b"content").expect("write");
        }

        // Also create enough files to potentially trigger parallel path
        for i in 0..40 {
            std::fs::write(
                temp.path().join(format!("file_{i:02}.txt")),
                b"content",
            )
            .expect("write");
        }

        let result = super::read_directory_entries_sorted_parallel(temp.path());
        assert!(result.is_ok());
        let entries = result.unwrap();

        // Verify entries are sorted
        for i in 1..entries.len() {
            let prev = &entries[i - 1].file_name;
            let curr = &entries[i].file_name;
            assert!(
                compare_file_names(prev, curr) != Ordering::Greater,
                "entries not sorted: {:?} > {:?}",
                prev,
                curr
            );
        }
    }

    #[cfg(feature = "parallel")]
    #[test]
    fn parallel_handles_symlinks() {
        let temp = tempfile::tempdir().expect("tempdir");

        // Create target files
        for i in 0..35 {
            std::fs::write(temp.path().join(format!("target_{i}.txt")), b"content")
                .expect("write");
        }

        // Create symlinks on Unix
        #[cfg(unix)]
        {
            for i in 0..10 {
                let target = temp.path().join(format!("target_{i}.txt"));
                let link = temp.path().join(format!("link_{i}.txt"));
                std::os::unix::fs::symlink(&target, &link).expect("symlink");
            }
        }

        let result = super::read_directory_entries_sorted_parallel(temp.path());
        assert!(result.is_ok());
        let entries = result.unwrap();

        // Verify symlinks are included with correct metadata (symlink_metadata, not target)
        #[cfg(unix)]
        {
            let symlink_entries: Vec<_> = entries
                .iter()
                .filter(|e| e.file_name.to_string_lossy().starts_with("link_"))
                .collect();
            assert_eq!(symlink_entries.len(), 10);
            for entry in symlink_entries {
                assert!(entry.metadata.file_type().is_symlink());
            }
        }
    }
}
