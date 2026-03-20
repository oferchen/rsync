//! Batched metadata syscalls for reduced overhead during directory traversal.
//!
//! This module provides high-performance metadata fetching by batching `stat()`
//! operations and using efficient syscalls like `statx()` and `fstatat()`.
//!
//! # Design
//!
//! - **Parallel fetching** using rayon to saturate I/O
//! - **Path-relative stats** with `openat`/`fstatat` to reduce path resolution
//! - **Modern syscalls** using `statx` on Linux 4.11+ for better performance
//! - **Caching** to avoid redundant syscalls for already-stat'd paths
//!
//! # Performance
//!
//! On large directory trees, batched metadata fetching can provide 2-4x speedup
//! compared to sequential stat operations, especially on:
//! - Network filesystems (NFS, CIFS)
//! - SSDs with high IOPS
//! - Multi-core systems
//!
//! # Example
//!
//! ```ignore
//! use flist::batched_stat::{BatchedStatCache, StatBatch};
//! use std::path::Path;
//!
//! let mut cache = BatchedStatCache::new();
//! let paths = vec![
//!     Path::new("/tmp/file1.txt"),
//!     Path::new("/tmp/file2.txt"),
//!     Path::new("/tmp/file3.txt"),
//! ];
//!
//! // Fetch metadata in parallel
//! let results = cache.stat_batch(&paths);
//! for (path, result) in paths.iter().zip(results) {
//!     if let Ok(metadata) = result {
//!         println!("{}: {} bytes", path.display(), metadata.len());
//!     }
//! }
//! ```

mod cache;
#[cfg(unix)]
mod dir_stat;
mod statx_support;
mod types;

#[cfg(test)]
mod tests;

pub use cache::BatchedStatCache;
#[cfg(unix)]
pub use dir_stat::DirectoryStatBatch;
pub use statx_support::has_statx_support;
#[cfg(all(target_os = "linux", not(target_env = "musl")))]
pub use statx_support::{statx, statx_mtime, statx_size_and_mtime};
#[cfg(unix)]
pub use types::FstatResult;
#[cfg(all(target_os = "linux", not(target_env = "musl")))]
pub use types::StatxResult;
