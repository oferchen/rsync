//! Memory-mapped file abstraction for basis file access.
//!
//! This module implements the `map_file`/`map_ptr` pattern from upstream rsync,
//! providing efficient cached access to basis files during delta application.
//!
//! # Problem
//!
//! During delta application, block references require reading from the basis
//! file. Naive implementation opens the file for each block, causing:
//! - 2000+ open/close syscalls for a typical 16MB file
//! - No kernel page cache reuse across blocks
//! - 60-70% of transfer latency
//!
//! # Solution
//!
//! The `MapFile` struct maintains a sliding window buffer (256KB by default)
//! over the basis file. Sequential block accesses hit the cache, and the
//! window slides forward as needed.
//!
//! # Strategies
//!
//! This module provides three mapping strategies:
//!
//! - `BufferedMap`: Sliding window buffer for efficient sequential access
//! - `MmapStrategy`: Memory-mapped access for zero-copy large file access
//! - `AdaptiveMapStrategy`: Automatically selects optimal strategy based on file size
//!
//! # Upstream Reference
//!
//! See `fileio.c` in upstream rsync 3.4.1: `map_file()`, `map_ptr()`, `unmap_file()`.

mod buffered;
mod wrapper;

#[cfg(unix)]
mod adaptive;
#[cfg(unix)]
mod mmap;

#[cfg(test)]
mod tests;

use std::io;

pub use buffered::BufferedMap;
pub use wrapper::MapFile;

#[cfg(unix)]
pub use adaptive::AdaptiveMapStrategy;
#[cfg(unix)]
pub use mmap::MmapStrategy;

/// Threshold above which memory mapping is preferred over buffered I/O.
/// Files larger than 1MB benefit from mmap's zero-copy access.
pub const MMAP_THRESHOLD: u64 = 1024 * 1024; // 1 MB

/// Strategy trait for file mapping implementations.
///
/// Allows swapping between buffered and memory-mapped implementations
/// without changing the delta application code.
pub trait MapStrategy: Send {
    /// Returns a slice of file data at the given offset.
    ///
    /// The returned slice is valid until the next call to `map_ptr`.
    /// The implementation may cache data to avoid repeated reads.
    ///
    /// # Errors
    ///
    /// Returns an error if the read fails or the requested range
    /// is beyond the file size.
    fn map_ptr(&mut self, offset: u64, len: usize) -> io::Result<&[u8]>;

    /// Returns the size of the mapping window.
    fn window_size(&self) -> usize;

    /// Returns the total file size.
    fn file_size(&self) -> u64;
}
