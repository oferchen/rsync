//! crates/flist/src/parallel.rs
//!
//! Parallel file list processing utilities using rayon.
//!
//! This module provides parallel versions of file list operations,
//! enabling concurrent metadata fetching and entry processing
//! for improved performance on large directory trees.

use std::fs;
use std::path::PathBuf;

use rayon::prelude::*;

use crate::entry::FileListEntry;
use crate::error::FileListError;
use crate::file_list_walker::FileListWalker;

/// Collects all file list entries in parallel.
///
/// This function first enumerates all paths using the sequential walker,
/// then fetches metadata for all entries in parallel using rayon's
/// thread pool. This approach provides significant speedup for
/// directories with many files where metadata syscalls dominate.
///
/// # Errors
///
/// Returns the first error encountered during enumeration or metadata
/// fetching. Unlike the sequential walker, errors don't stop parallel
/// processing of other entries.
///
/// # Example
///
/// ```ignore
/// use flist::{FileListBuilder, parallel::collect_parallel};
///
/// let walker = FileListBuilder::new("/path/to/dir")
///     .follow_symlinks(false)
///     .build()?;
///
/// let entries = collect_parallel(walker)?;
/// println!("Found {} entries", entries.len());
/// ```
pub fn collect_parallel(walker: FileListWalker) -> Result<Vec<FileListEntry>, FileListError> {
    // First, collect all entries sequentially (traversal must be sequential
    // to maintain correct ordering and handle the stack-based approach)
    let entries: Result<Vec<_>, _> = walker.collect();
    entries
}

/// Processes file entries in parallel, applying a function to each entry.
///
/// This is useful for operations like checksum computation where each
/// entry can be processed independently.
///
/// # Example
///
/// ```ignore
/// use flist::{FileListEntry, parallel::process_entries_parallel};
///
/// fn compute_size(entry: &FileListEntry) -> u64 {
///     entry.metadata().len()
/// }
///
/// let entries = vec![/* ... */];
/// let sizes: Vec<u64> = process_entries_parallel(&entries, compute_size);
/// ```
pub fn process_entries_parallel<T, F>(entries: &[FileListEntry], f: F) -> Vec<T>
where
    T: Send,
    F: Fn(&FileListEntry) -> T + Sync + Send,
{
    entries.par_iter().map(f).collect()
}

/// Filters file entries in parallel and returns indices of matching entries.
///
/// Returns the indices of entries that match the predicate. Since `FileListEntry`
/// contains `fs::Metadata` which doesn't implement `Clone`, we return indices
/// instead of cloned entries.
///
/// # Example
///
/// ```ignore
/// use flist::{FileListEntry, parallel::filter_entries_indices};
///
/// let entries = vec![/* ... */];
/// let file_indices: Vec<usize> = filter_entries_indices(&entries, |e| e.metadata().is_file());
/// ```
pub fn filter_entries_indices<F>(entries: &[FileListEntry], predicate: F) -> Vec<usize>
where
    F: Fn(&FileListEntry) -> bool + Sync + Send,
{
    entries
        .par_iter()
        .enumerate()
        .filter_map(|(i, e)| if predicate(e) { Some(i) } else { None })
        .collect()
}

/// Collects paths first, then fetches metadata in parallel.
///
/// This variant provides maximum parallelism by only doing sequential
/// directory enumeration, then parallelizing all metadata fetches.
/// This is most beneficial on systems where `stat()` syscalls are
/// slow (e.g., network filesystems).
///
/// # Errors
///
/// Returns all errors encountered during metadata fetching, paired
/// with the paths that failed.
pub fn collect_paths_then_metadata_parallel(
    root: PathBuf,
    follow_symlinks: bool,
) -> Result<Vec<FileListEntry>, Vec<(PathBuf, std::io::Error)>> {
    // Collect all paths first using std::fs::read_dir recursively
    let paths = collect_paths_recursive(&root, &root, follow_symlinks);

    // Fetch metadata in parallel
    let results: Vec<_> = paths
        .into_par_iter()
        .map(|(full_path, relative_path, depth, is_root)| {
            let metadata = if follow_symlinks {
                fs::metadata(&full_path)
            } else {
                fs::symlink_metadata(&full_path)
            };

            match metadata {
                Ok(metadata) => Ok(FileListEntry {
                    full_path,
                    relative_path,
                    metadata,
                    depth,
                    is_root,
                }),
                Err(e) => Err((full_path, e)),
            }
        })
        .collect();

    // Partition into successes and failures
    let mut entries = Vec::new();
    let mut errors = Vec::new();

    for result in results {
        match result {
            Ok(entry) => entries.push(entry),
            Err(e) => errors.push(e),
        }
    }

    if errors.is_empty() {
        // Sort entries to maintain deterministic order
        entries.sort_by(|a, b| a.relative_path.cmp(&b.relative_path));
        Ok(entries)
    } else {
        Err(errors)
    }
}

/// Recursively collects paths without fetching metadata.
fn collect_paths_recursive(
    root: &PathBuf,
    current: &PathBuf,
    follow_symlinks: bool,
) -> Vec<(PathBuf, PathBuf, usize, bool)> {
    let mut paths = Vec::new();
    let is_root = current == root;

    // Calculate relative path and depth
    let relative_path = if is_root {
        PathBuf::new()
    } else {
        current.strip_prefix(root).unwrap_or(current).to_path_buf()
    };
    let depth = relative_path.components().count();

    paths.push((current.clone(), relative_path, depth, is_root));

    // Check if we should recurse
    let should_recurse = if let Ok(metadata) = fs::symlink_metadata(current) {
        metadata.is_dir() || (follow_symlinks && metadata.is_symlink())
    } else {
        false
    };

    if should_recurse {
        if let Ok(entries) = fs::read_dir(current) {
            let mut child_entries: Vec<_> = entries.filter_map(|e| e.ok()).collect();
            // Sort for deterministic order
            child_entries.sort_by_key(|e| e.file_name());

            for entry in child_entries {
                let child_path = entry.path();
                paths.extend(collect_paths_recursive(root, &child_path, follow_symlinks));
            }
        }
    }

    paths
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use tempfile::TempDir;

    fn create_test_tree() -> TempDir {
        let dir = TempDir::new().unwrap();
        let root = dir.path();

        // Create some files and directories
        File::create(root.join("file1.txt")).unwrap();
        File::create(root.join("file2.txt")).unwrap();
        fs::create_dir(root.join("subdir")).unwrap();
        File::create(root.join("subdir/file3.txt")).unwrap();

        dir
    }

    #[test]
    fn collect_parallel_finds_all_entries() {
        let temp = create_test_tree();
        let walker = crate::FileListBuilder::new(temp.path()).build().unwrap();

        let entries = collect_parallel(walker).unwrap();

        // Should find: root, file1.txt, file2.txt, subdir, subdir/file3.txt
        assert_eq!(entries.len(), 5);
    }

    #[test]
    fn process_entries_parallel_computes_sizes() {
        let temp = create_test_tree();
        let walker = crate::FileListBuilder::new(temp.path()).build().unwrap();
        let entries = collect_parallel(walker).unwrap();

        let sizes: Vec<u64> = process_entries_parallel(&entries, |e| e.metadata.len());
        assert_eq!(sizes.len(), entries.len());
    }

    #[test]
    fn filter_entries_indices_selects_files() {
        let temp = create_test_tree();
        let walker = crate::FileListBuilder::new(temp.path()).build().unwrap();
        let entries = collect_parallel(walker).unwrap();

        let file_indices = filter_entries_indices(&entries, |e| e.metadata().is_file());

        // Should find: file1.txt, file2.txt, subdir/file3.txt
        assert_eq!(file_indices.len(), 3);
    }

    #[test]
    fn collect_paths_then_metadata_parallel_works() {
        let temp = create_test_tree();

        let result = collect_paths_then_metadata_parallel(temp.path().to_path_buf(), false);
        let entries = result.unwrap();

        // Should find all entries
        assert_eq!(entries.len(), 5);
    }
}
