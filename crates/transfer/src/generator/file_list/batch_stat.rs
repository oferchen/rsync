//! Batched metadata resolution for directory entries.
//!
//! Parallelizes `stat()` / `lstat()` syscalls across directory children using
//! rayon when the entry count exceeds the configured stat threshold. For small
//! directories the overhead of thread-pool dispatch is avoided by falling back
//! to sequential iteration.
//!
//! # Upstream Reference
//!
//! - `flist.c:send_directory()` - iterates `readdir()` results and stats each

use std::fs;
use std::path::PathBuf;

use crate::parallel_io::{ParallelThresholds, map_blocking};

/// Result of batched metadata resolution for a single directory entry.
///
/// Pairs the entry's full path with either its resolved metadata or the
/// I/O error encountered during `stat()` / `lstat()`.
pub(in crate::generator) struct StatResult {
    /// Full filesystem path of the entry.
    pub path: PathBuf,
    /// Resolved metadata, or the error from the stat call.
    pub metadata: Result<fs::Metadata, std::io::Error>,
}

/// Collects `read_dir()` entries into paths and batch-resolves their metadata.
///
/// When `follow_symlinks` is true, uses `fs::metadata()` (follows symlinks).
/// Otherwise uses `fs::symlink_metadata()` (lstat). The caller is responsible
/// for applying more nuanced symlink resolution (e.g. `--copy-unsafe-links`)
/// after receiving the raw metadata.
///
/// Uses [`map_blocking`] which dispatches to rayon's work-stealing pool when
/// the entry count meets the configured stat threshold from [`ParallelThresholds`],
/// otherwise falls back to sequential iteration.
pub(in crate::generator) fn batch_stat_dir_entries(
    paths: Vec<PathBuf>,
    follow_symlinks: bool,
    thresholds: &ParallelThresholds,
) -> Vec<StatResult> {
    map_blocking(paths, thresholds.stat, move |path| {
        let metadata = if follow_symlinks {
            fs::metadata(&path)
        } else {
            fs::symlink_metadata(&path)
        };
        StatResult { path, metadata }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use tempfile::TempDir;

    #[test]
    fn batch_stat_sequential_small_dir() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();

        // Create a few files - below threshold, sequential path
        let mut paths = Vec::new();
        for i in 0..5 {
            let p = root.join(format!("file{i}.txt"));
            File::create(&p).unwrap();
            paths.push(p);
        }

        let results = batch_stat_dir_entries(paths, false, &ParallelThresholds::default());
        assert_eq!(results.len(), 5);
        for r in &results {
            assert!(
                r.metadata.is_ok(),
                "stat should succeed for {}",
                r.path.display()
            );
        }
    }

    #[test]
    fn batch_stat_parallel_large_dir() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();

        // Create enough files to exceed the parallel threshold
        let count = ParallelThresholds::default().stat + 10;
        let mut paths = Vec::new();
        for i in 0..count {
            let p = root.join(format!("file{i}.txt"));
            File::create(&p).unwrap();
            paths.push(p);
        }

        let results = batch_stat_dir_entries(paths, false, &ParallelThresholds::default());
        assert_eq!(results.len(), count);
        for r in &results {
            assert!(
                r.metadata.is_ok(),
                "stat should succeed for {}",
                r.path.display()
            );
        }
    }

    #[test]
    fn batch_stat_preserves_order() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();

        let count = ParallelThresholds::default().stat + 20;
        let mut paths = Vec::new();
        for i in 0..count {
            let p = root.join(format!("entry_{i:04}.dat"));
            File::create(&p).unwrap();
            paths.push(p);
        }

        let original_paths: Vec<PathBuf> = paths.clone();
        let results = batch_stat_dir_entries(paths, false, &ParallelThresholds::default());

        // Results must be in the same order as input
        for (i, r) in results.iter().enumerate() {
            assert_eq!(r.path, original_paths[i]);
        }
    }

    #[test]
    fn batch_stat_nonexistent_returns_error() {
        let paths = vec![PathBuf::from("/nonexistent/path/abc123")];
        let results = batch_stat_dir_entries(paths, false, &ParallelThresholds::default());
        assert_eq!(results.len(), 1);
        assert!(results[0].metadata.is_err());
    }

    #[test]
    fn batch_stat_follow_symlinks_mode() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();

        let p = root.join("regular.txt");
        File::create(&p).unwrap();

        // With follow_symlinks=true, uses fs::metadata (follows symlinks)
        let results_follow =
            batch_stat_dir_entries(vec![p.clone()], true, &ParallelThresholds::default());
        assert!(results_follow[0].metadata.is_ok());

        // With follow_symlinks=false, uses fs::symlink_metadata (lstat)
        let results_lstat = batch_stat_dir_entries(vec![p], false, &ParallelThresholds::default());
        assert!(results_lstat[0].metadata.is_ok());
    }

    #[test]
    fn batch_stat_empty_input() {
        let results = batch_stat_dir_entries(Vec::new(), false);
        assert!(results.is_empty());
    }

    #[test]
    fn batch_stat_identical_to_sequential() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();

        // Create files and a subdirectory
        let mut paths = Vec::new();
        for i in 0..10 {
            let p = root.join(format!("f{i}.txt"));
            File::create(&p).unwrap();
            paths.push(p);
        }
        let sub = root.join("subdir");
        std::fs::create_dir(&sub).unwrap();
        paths.push(sub);

        // Sequential reference
        let sequential: Vec<(PathBuf, bool, u64)> = paths
            .iter()
            .map(|p| {
                let m = std::fs::symlink_metadata(p).unwrap();
                (p.clone(), m.is_dir(), m.len())
            })
            .collect();

        // Batched
        let batched_results = batch_stat_dir_entries(paths, false);
        let batched: Vec<(PathBuf, bool, u64)> = batched_results
            .into_iter()
            .map(|r| {
                let m = r.metadata.unwrap();
                (r.path, m.is_dir(), m.len())
            })
            .collect();

        assert_eq!(sequential, batched);
    }
}
