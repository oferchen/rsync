//! Parallel file transfer execution for many-small-files scenarios.
//!
//! This module provides parallel file transfer capabilities optimized for workloads
//! with many small files. It uses rayon for parallel execution while maintaining
//! deterministic ordering of results.
//!
//! # Design
//!
//! - Small files (< 64KB) benefit from parallel transfer
//! - Large files are transferred sequentially for cache efficiency
//! - Both sequential and parallel paths are always available (dual-path)
//! - Results maintain original ordering for deterministic output
//!
//! # Examples
//!
//! ```ignore
//! use engine::local_copy::parallel_transfer::{TransferJob, execute_batch};
//! use std::path::PathBuf;
//!
//! let temp = tempfile::tempdir().unwrap();
//! let src = temp.path().join("src.txt");
//! let dst = temp.path().join("dst.txt");
//! std::fs::write(&src, b"data").unwrap();
//! let jobs = vec![
//!     TransferJob {
//!         src: src.clone(),
//!         dst: dst.clone(),
//!         size: 100,
//!     },
//! ];
//!
//! let (results, stats) = execute_batch(&jobs);
//! assert_eq!(stats.total_files, 1);
//! assert_eq!(stats.success_count, 1);
//! ```

#![allow(dead_code)]

use rayon::prelude::*;
use std::io;
use std::path::PathBuf;

/// Files smaller than this are eligible for parallel transfer.
pub const SMALL_FILE_THRESHOLD: u64 = 64 * 1024; // 64KB

/// Default concurrency limit for parallel transfers.
pub const DEFAULT_CONCURRENCY: usize = 4;

/// Minimum number of files to justify parallel transfer overhead.
pub const PARALLEL_THRESHOLD: usize = 8;

/// A file transfer job to execute.
#[derive(Debug, Clone)]
pub struct TransferJob {
    /// Source file path.
    pub src: PathBuf,
    /// Destination file path.
    pub dst: PathBuf,
    /// File size in bytes.
    pub size: u64,
}

/// Result of a single file transfer.
#[derive(Debug)]
pub struct TransferResult {
    /// Index of the job in the original batch.
    pub index: usize,
    /// Source path.
    pub src: PathBuf,
    /// Destination path.
    pub dst: PathBuf,
    /// Bytes copied.
    pub bytes_copied: u64,
    /// Error if transfer failed.
    pub error: Option<io::Error>,
}

/// Summary statistics for a batch transfer.
#[derive(Debug, Clone, Default)]
pub struct BatchStats {
    /// Total files processed.
    pub total_files: usize,
    /// Files transferred successfully.
    pub success_count: usize,
    /// Files that failed.
    pub error_count: usize,
    /// Total bytes copied.
    pub total_bytes: u64,
    /// Whether parallel mode was used.
    pub parallel_used: bool,
}

/// Copy a single file, creating parent directories as needed.
fn copy_single(job: &TransferJob, index: usize) -> TransferResult {
    // Ensure parent directory exists
    if let Some(parent) = job.dst.parent() {
        if !parent.exists() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                return TransferResult {
                    index,
                    src: job.src.clone(),
                    dst: job.dst.clone(),
                    bytes_copied: 0,
                    error: Some(e),
                };
            }
        }
    }

    // Copy the file
    match std::fs::copy(&job.src, &job.dst) {
        Ok(bytes) => TransferResult {
            index,
            src: job.src.clone(),
            dst: job.dst.clone(),
            bytes_copied: bytes,
            error: None,
        },
        Err(e) => TransferResult {
            index,
            src: job.src.clone(),
            dst: job.dst.clone(),
            bytes_copied: 0,
            error: Some(e),
        },
    }
}

/// Execute a batch of file transfers.
///
/// Uses parallel transfer for batches of small files, sequential for large files
/// or small batches.
///
/// Maintains result ordering matching input order.
pub fn execute_batch(jobs: &[TransferJob]) -> (Vec<TransferResult>, BatchStats) {
    if jobs.is_empty() {
        return (Vec::new(), BatchStats::default());
    }

    // If batch is too small, use sequential
    if jobs.len() < PARALLEL_THRESHOLD {
        return execute_sequential(jobs);
    }

    // Partition jobs by size
    let (small_jobs, large_jobs) = partition_by_size(jobs);

    let mut all_results = Vec::with_capacity(jobs.len());
    let mut parallel_used = false;

    // Process small files in parallel if there are enough
    if small_jobs.len() >= PARALLEL_THRESHOLD {
        parallel_used = true;
        let (small_results, _) =
            execute_parallel(&small_jobs.iter().map(|&j| j.clone()).collect::<Vec<_>>());
        all_results.extend(small_results);
    } else {
        // Process small files sequentially
        let (small_results, _) =
            execute_sequential(&small_jobs.iter().map(|&j| j.clone()).collect::<Vec<_>>());
        all_results.extend(small_results);
    }

    // Process large files sequentially
    let (large_results, _) =
        execute_sequential(&large_jobs.iter().map(|&j| j.clone()).collect::<Vec<_>>());
    all_results.extend(large_results);

    // Sort results by original index to maintain ordering
    all_results.sort_by_key(|r| r.index);

    // Compute statistics
    let stats = compute_stats(&all_results, parallel_used);

    (all_results, stats)
}

/// Execute transfers sequentially (always available).
pub fn execute_sequential(jobs: &[TransferJob]) -> (Vec<TransferResult>, BatchStats) {
    let mut results = Vec::with_capacity(jobs.len());

    for (index, job) in jobs.iter().enumerate() {
        let result = copy_single(job, index);
        results.push(result);
    }

    let stats = compute_stats(&results, false);
    (results, stats)
}

/// Execute transfers in parallel using rayon (always available).
pub fn execute_parallel(jobs: &[TransferJob]) -> (Vec<TransferResult>, BatchStats) {
    let mut results: Vec<TransferResult> = jobs
        .par_iter()
        .enumerate()
        .map(|(index, job)| copy_single(job, index))
        .collect();

    // Sort by index to maintain original ordering
    results.sort_by_key(|r| r.index);

    let stats = compute_stats(&results, true);
    (results, stats)
}

/// Partition jobs into small-file and large-file groups.
/// Small files run in parallel, large files run sequentially.
pub fn partition_by_size(jobs: &[TransferJob]) -> (Vec<&TransferJob>, Vec<&TransferJob>) {
    let mut small = Vec::new();
    let mut large = Vec::new();

    for job in jobs {
        if job.size < SMALL_FILE_THRESHOLD {
            small.push(job);
        } else {
            large.push(job);
        }
    }

    (small, large)
}

/// Compute statistics from transfer results.
fn compute_stats(results: &[TransferResult], parallel_used: bool) -> BatchStats {
    let mut stats = BatchStats {
        total_files: results.len(),
        parallel_used,
        ..Default::default()
    };

    for result in results {
        if result.error.is_none() {
            stats.success_count += 1;
            stats.total_bytes += result.bytes_copied;
        } else {
            stats.error_count += 1;
        }
    }

    stats
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    /// Create a test file with given size.
    fn create_test_file(dir: &TempDir, name: &str, size: u64) -> PathBuf {
        let path = dir.path().join(name);
        let data = vec![b'x'; size as usize];
        fs::write(&path, data).unwrap();
        path
    }

    #[test]
    fn test_partition_by_size() {
        let temp = TempDir::new().unwrap();
        let jobs = vec![
            TransferJob {
                src: temp.path().join("small1.txt"),
                dst: temp.path().join("dst1.txt"),
                size: 1024, // Small
            },
            TransferJob {
                src: temp.path().join("large1.txt"),
                dst: temp.path().join("dst2.txt"),
                size: 128 * 1024, // Large
            },
            TransferJob {
                src: temp.path().join("small2.txt"),
                dst: temp.path().join("dst3.txt"),
                size: 32 * 1024, // Small
            },
        ];

        let (small, large) = partition_by_size(&jobs);
        assert_eq!(small.len(), 2);
        assert_eq!(large.len(), 1);
        assert_eq!(small[0].size, 1024);
        assert_eq!(small[1].size, 32 * 1024);
        assert_eq!(large[0].size, 128 * 1024);
    }

    #[test]
    fn test_execute_sequential_empty() {
        let jobs: Vec<TransferJob> = vec![];
        let (results, stats) = execute_sequential(&jobs);
        assert_eq!(results.len(), 0);
        assert_eq!(stats.total_files, 0);
        assert_eq!(stats.success_count, 0);
        assert_eq!(stats.error_count, 0);
        assert_eq!(stats.total_bytes, 0);
        assert!(!stats.parallel_used);
    }

    #[test]
    fn test_execute_sequential_single() {
        let temp = TempDir::new().unwrap();
        let src = create_test_file(&temp, "src.txt", 100);
        let dst = temp.path().join("dst.txt");

        let jobs = vec![TransferJob {
            src: src.clone(),
            dst: dst.clone(),
            size: 100,
        }];

        let (results, stats) = execute_sequential(&jobs);
        assert_eq!(results.len(), 1);
        assert_eq!(stats.total_files, 1);
        assert_eq!(stats.success_count, 1);
        assert_eq!(stats.error_count, 0);
        assert_eq!(stats.total_bytes, 100);
        assert!(!stats.parallel_used);
        assert!(results[0].error.is_none());
        assert_eq!(results[0].bytes_copied, 100);
        assert!(dst.exists());
    }

    #[test]
    fn test_execute_sequential_multiple() {
        let temp = TempDir::new().unwrap();
        let src1 = create_test_file(&temp, "src1.txt", 50);
        let src2 = create_test_file(&temp, "src2.txt", 75);
        let src3 = create_test_file(&temp, "src3.txt", 100);
        let dst1 = temp.path().join("dst1.txt");
        let dst2 = temp.path().join("dst2.txt");
        let dst3 = temp.path().join("dst3.txt");

        let jobs = vec![
            TransferJob {
                src: src1,
                dst: dst1.clone(),
                size: 50,
            },
            TransferJob {
                src: src2,
                dst: dst2.clone(),
                size: 75,
            },
            TransferJob {
                src: src3,
                dst: dst3.clone(),
                size: 100,
            },
        ];

        let (results, stats) = execute_sequential(&jobs);
        assert_eq!(results.len(), 3);
        assert_eq!(stats.total_files, 3);
        assert_eq!(stats.success_count, 3);
        assert_eq!(stats.error_count, 0);
        assert_eq!(stats.total_bytes, 225);
        assert!(!stats.parallel_used);

        assert!(dst1.exists());
        assert!(dst2.exists());
        assert!(dst3.exists());
    }

    #[test]
    fn test_execute_parallel_multiple() {
        let temp = TempDir::new().unwrap();
        let src1 = create_test_file(&temp, "src1.txt", 50);
        let src2 = create_test_file(&temp, "src2.txt", 75);
        let src3 = create_test_file(&temp, "src3.txt", 100);
        let dst1 = temp.path().join("dst1.txt");
        let dst2 = temp.path().join("dst2.txt");
        let dst3 = temp.path().join("dst3.txt");

        let jobs = vec![
            TransferJob {
                src: src1,
                dst: dst1.clone(),
                size: 50,
            },
            TransferJob {
                src: src2,
                dst: dst2.clone(),
                size: 75,
            },
            TransferJob {
                src: src3,
                dst: dst3.clone(),
                size: 100,
            },
        ];

        let (results, stats) = execute_parallel(&jobs);
        assert_eq!(results.len(), 3);
        assert_eq!(stats.total_files, 3);
        assert_eq!(stats.success_count, 3);
        assert_eq!(stats.error_count, 0);
        assert_eq!(stats.total_bytes, 225);
        assert!(stats.parallel_used);

        assert!(dst1.exists());
        assert!(dst2.exists());
        assert!(dst3.exists());
    }

    #[test]
    fn test_execute_batch_routes_to_sequential() {
        let temp = TempDir::new().unwrap();
        // Create fewer files than PARALLEL_THRESHOLD (8)
        let mut jobs = vec![];
        for i in 0..5 {
            let src = create_test_file(&temp, &format!("src{i}.txt"), 100);
            let dst = temp.path().join(format!("dst{i}.txt"));
            jobs.push(TransferJob {
                src,
                dst,
                size: 100,
            });
        }

        let (results, stats) = execute_batch(&jobs);
        assert_eq!(results.len(), 5);
        assert_eq!(stats.total_files, 5);
        assert_eq!(stats.success_count, 5);
        assert!(!stats.parallel_used); // Should use sequential for small batches
    }

    #[test]
    fn test_execute_batch_routes_to_parallel() {
        let temp = TempDir::new().unwrap();
        // Create more files than PARALLEL_THRESHOLD (8), all small
        let mut jobs = vec![];
        for i in 0..10 {
            let src = create_test_file(&temp, &format!("src{i}.txt"), 1024);
            let dst = temp.path().join(format!("dst{i}.txt"));
            jobs.push(TransferJob {
                src,
                dst,
                size: 1024, // Small file
            });
        }

        let (results, stats) = execute_batch(&jobs);
        assert_eq!(results.len(), 10);
        assert_eq!(stats.total_files, 10);
        assert_eq!(stats.success_count, 10);
        assert!(stats.parallel_used); // Should use parallel for many small files
    }

    #[test]
    fn test_execute_batch_mixed_sizes() {
        let temp = TempDir::new().unwrap();
        let mut jobs = vec![];

        // Create 10 small files
        for i in 0..10 {
            let src = create_test_file(&temp, &format!("small{i}.txt"), 1024);
            let dst = temp.path().join(format!("dst_small{i}.txt"));
            jobs.push(TransferJob {
                src,
                dst,
                size: 1024,
            });
        }

        // Create 2 large files
        for i in 0..2 {
            let src = create_test_file(&temp, &format!("large{i}.txt"), 128 * 1024);
            let dst = temp.path().join(format!("dst_large{i}.txt"));
            jobs.push(TransferJob {
                src,
                dst,
                size: 128 * 1024,
            });
        }

        let (results, stats) = execute_batch(&jobs);
        assert_eq!(results.len(), 12);
        assert_eq!(stats.total_files, 12);
        assert_eq!(stats.success_count, 12);
        assert!(stats.parallel_used); // Small files processed in parallel
    }

    #[test]
    fn test_result_ordering_preserved() {
        let temp = TempDir::new().unwrap();
        let mut jobs = vec![];

        // Create files with different sizes to ensure parallel execution might reorder
        for i in 0..10 {
            let size = (i + 1) * 100;
            let src = create_test_file(&temp, &format!("src{i}.txt"), size);
            let dst = temp.path().join(format!("dst{i}.txt"));
            jobs.push(TransferJob { src, dst, size });
        }

        let (results, _stats) = execute_parallel(&jobs);

        // Verify results are in original order
        for (i, result) in results.iter().enumerate() {
            assert_eq!(result.index, i);
        }
    }

    #[test]
    fn test_batch_stats_correct() {
        let temp = TempDir::new().unwrap();
        let src1 = create_test_file(&temp, "src1.txt", 100);
        let src2 = create_test_file(&temp, "src2.txt", 200);
        let dst1 = temp.path().join("dst1.txt");
        let dst2 = temp.path().join("dst2.txt");

        let jobs = vec![
            TransferJob {
                src: src1,
                dst: dst1,
                size: 100,
            },
            TransferJob {
                src: src2,
                dst: dst2,
                size: 200,
            },
        ];

        let (_results, stats) = execute_sequential(&jobs);
        assert_eq!(stats.total_files, 2);
        assert_eq!(stats.success_count, 2);
        assert_eq!(stats.error_count, 0);
        assert_eq!(stats.total_bytes, 300);
    }

    #[test]
    fn test_nonexistent_source_error() {
        let temp = TempDir::new().unwrap();
        let src = temp.path().join("nonexistent.txt");
        let dst = temp.path().join("dst.txt");

        let jobs = vec![TransferJob {
            src,
            dst,
            size: 100,
        }];

        let (results, stats) = execute_sequential(&jobs);
        assert_eq!(results.len(), 1);
        assert_eq!(stats.total_files, 1);
        assert_eq!(stats.success_count, 0);
        assert_eq!(stats.error_count, 1);
        assert!(results[0].error.is_some());
        assert_eq!(results[0].bytes_copied, 0);
    }

    #[test]
    fn test_creates_parent_directories() {
        let temp = TempDir::new().unwrap();
        let src = create_test_file(&temp, "src.txt", 100);
        let dst = temp.path().join("nested/deep/dir/dst.txt");

        let jobs = vec![TransferJob {
            src,
            dst: dst.clone(),
            size: 100,
        }];

        let (results, stats) = execute_sequential(&jobs);
        assert_eq!(results.len(), 1);
        assert_eq!(stats.success_count, 1);
        assert!(results[0].error.is_none());
        assert!(dst.exists());
        assert!(dst.parent().unwrap().exists());
    }

    #[test]
    fn test_parity_sequential_vs_parallel() {
        let temp = TempDir::new().unwrap();
        let mut jobs = vec![];

        // Create test files
        for i in 0..5 {
            let src = create_test_file(&temp, &format!("src{i}.txt"), (i + 1) * 100);
            jobs.push(TransferJob {
                src,
                dst: temp.path().join(format!("seq{i}.txt")),
                size: (i + 1) * 100,
            });
        }

        // Execute sequentially
        let (_seq_results, seq_stats) = execute_sequential(&jobs);

        // Reset jobs for parallel execution
        let mut jobs_parallel = vec![];
        for i in 0..5 {
            let src = temp.path().join(format!("seq{i}.txt")); // Use sequential outputs as inputs
            jobs_parallel.push(TransferJob {
                src,
                dst: temp.path().join(format!("par{i}.txt")),
                size: (i + 1) * 100,
            });
        }

        // Execute in parallel
        let (_par_results, par_stats) = execute_parallel(&jobs_parallel);

        // Verify both produced same stats
        assert_eq!(seq_stats.total_files, par_stats.total_files);
        assert_eq!(seq_stats.success_count, par_stats.success_count);
        assert_eq!(seq_stats.total_bytes, par_stats.total_bytes);

        // Verify all output files exist and have correct content
        for i in 0..5 {
            let seq_path = temp.path().join(format!("seq{i}.txt"));
            let par_path = temp.path().join(format!("par{i}.txt"));
            assert!(seq_path.exists());
            assert!(par_path.exists());

            let seq_content = fs::read(&seq_path).unwrap();
            let par_content = fs::read(&par_path).unwrap();
            assert_eq!(seq_content, par_content);
        }
    }
}
