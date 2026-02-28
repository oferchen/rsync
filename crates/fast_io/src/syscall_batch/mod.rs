//! Batched metadata syscall operations with dual-path runtime selection.
//!
//! This module provides batched metadata operations that reduce syscall overhead when
//! processing many files. It uses Linux's `statx()` syscall for more efficient metadata
//! retrieval and groups operations for improved cache locality.
//!
//! # Dual-Path Strategy
//!
//! The module provides two execution paths that are ALWAYS compiled:
//! - **Individual path**: Processes operations one at a time using standard library calls
//! - **Batched path**: Groups operations by type and processes them together
//!
//! Runtime selection uses [`BATCH_THRESHOLD`]: below this, use individual path;
//! at or above, use batched path.
//!
//! # Platform Support
//!
//! - **Linux**: Uses `statx()` for improved metadata operations in batched path
//! - **Other Unix**: Batched path uses standard library calls with grouping optimization
//! - **Non-Unix (Windows)**: Portable fallbacks — `filetime` crate for timestamps,
//!   readonly attribute mapping for permissions
//!
//! # Performance Characteristics
//!
//! - Individual path: Lower overhead for small operation counts (< 8)
//! - Batched path: Better cache locality and reduced context switches for large batches
//! - Operations are reordered in batched mode but results match original input order
//!
//! # Example
//!
//! ```no_run
//! use std::path::PathBuf;
//! use fast_io::syscall_batch::{MetadataOp, execute_metadata_ops};
//!
//! # fn main() -> std::io::Result<()> {
//! let ops = vec![
//!     MetadataOp::Stat(PathBuf::from("/tmp/file1")),
//!     MetadataOp::Lstat(PathBuf::from("/tmp/file2")),
//!     MetadataOp::Stat(PathBuf::from("/tmp/file3")),
//! ];
//!
//! let results = execute_metadata_ops(&ops);
//! for result in results {
//!     match result {
//!         fast_io::syscall_batch::MetadataResult::Stat(Ok(metadata)) => {
//!             println!("File size: {}", metadata.len());
//!         }
//!         fast_io::syscall_batch::MetadataResult::Stat(Err(e)) => {
//!             eprintln!("Error: {}", e);
//!         }
//!         _ => {}
//!     }
//! }
//! # Ok(())
//! # }
//! ```

mod executor;
mod types;

pub use executor::{execute_metadata_ops_batched, execute_metadata_ops_individual};
pub use types::{MetadataOp, MetadataResult};

/// Threshold below which individual syscalls are used.
///
/// Operations below this count use the individual path for lower overhead.
/// Operations at or above this count use the batched path for better performance.
pub const BATCH_THRESHOLD: usize = 8;

/// Execute a batch of metadata operations.
///
/// Uses batched processing when `ops.len() >= BATCH_THRESHOLD`,
/// otherwise falls back to individual syscalls for lower overhead.
///
/// # Arguments
///
/// * `ops` - Slice of metadata operations to execute
///
/// # Returns
///
/// Vector of results in the same order as input operations.
///
/// # Example
///
/// ```no_run
/// use std::path::PathBuf;
/// use fast_io::syscall_batch::{MetadataOp, execute_metadata_ops};
///
/// let ops = vec![
///     MetadataOp::Stat(PathBuf::from("/etc/hosts")),
///     MetadataOp::Lstat(PathBuf::from("/tmp/link")),
/// ];
///
/// let results = execute_metadata_ops(&ops);
/// assert_eq!(results.len(), ops.len());
/// ```
pub fn execute_metadata_ops(ops: &[MetadataOp]) -> Vec<MetadataResult> {
    if ops.len() >= BATCH_THRESHOLD {
        execute_metadata_ops_batched(ops)
    } else {
        execute_metadata_ops_individual(ops)
    }
}

#[cfg(test)]
mod tests;
