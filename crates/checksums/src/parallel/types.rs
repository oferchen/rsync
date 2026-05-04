//! Core types for parallel checksum computation.

use std::io;
use std::path::PathBuf;

/// Minimum number of blocks at which parallel computation becomes beneficial.
///
/// Below this threshold, the overhead of rayon's work-stealing scheduler
/// outweighs the benefits of parallelism. The runtime-selecting `compute_*_auto`
/// functions use this value to choose between sequential and parallel paths.
pub const PARALLEL_BLOCK_THRESHOLD: usize = 8;

/// Result of computing both rolling and strong checksums for a single block.
///
/// Mirrors the upstream rsync `sum_struct` sent by the generator during
/// delta-transfer. The `rolling` value is used for hash table lookup in
/// `hash_search()`, and `strong` confirms matches found by the weak hash.
#[derive(Clone, Debug)]
pub struct BlockSignature<D> {
    /// Packed 32-bit rolling checksum (`(s2 << 16) | s1`) for hash table lookup.
    pub rolling: u32,
    /// Strong digest for collision verification after a rolling checksum match.
    pub strong: D,
}

/// Result of hashing a single file during parallel file processing.
///
/// Contains both the digest and original path so callers can correlate
/// results with file list entries after parallel computation.
#[derive(Debug)]
pub struct FileHashResult<D> {
    /// Path of the hashed file.
    pub path: PathBuf,
    /// Computed digest, or the I/O error encountered when reading the file.
    pub digest: Result<D, io::Error>,
    /// File size in bytes (0 if the file could not be read).
    pub size: u64,
}

impl<D: Clone> Clone for FileHashResult<D> {
    fn clone(&self) -> Self {
        Self {
            path: self.path.clone(),
            digest: self
                .digest
                .as_ref()
                .map(|d| d.clone())
                .map_err(|e| io::Error::new(e.kind(), e.to_string())),
            size: self.size,
        }
    }
}

/// Result of computing file signatures (rolling + strong checksums).
#[derive(Debug)]
pub struct FileSignatureResult<D> {
    /// The path of the file.
    pub path: PathBuf,
    /// Block signatures for the file, or an error.
    pub signatures: Result<Vec<BlockSignature<D>>, io::Error>,
    /// Total file size in bytes.
    pub size: u64,
    /// Number of blocks in the file.
    pub block_count: usize,
}

/// Configuration for parallel file hashing operations.
#[derive(Clone, Copy, Debug)]
pub struct FileHashConfig {
    /// Size of the read buffer in bytes.
    /// Larger buffers improve throughput but increase memory usage.
    /// Default: 64 KiB
    pub buffer_size: usize,

    /// Minimum file size threshold for including in parallel processing.
    /// Files smaller than this are still processed but may be batched.
    /// Default: 0 (no minimum)
    pub min_file_size: u64,

    /// Maximum file size to read entirely into memory.
    /// Files larger than this are streamed in chunks.
    /// Default: 1 MiB
    pub max_memory_file_size: u64,
}

impl Default for FileHashConfig {
    fn default() -> Self {
        Self {
            buffer_size: 64 * 1024, // 64 KiB
            min_file_size: 0,
            max_memory_file_size: 1024 * 1024, // 1 MiB
        }
    }
}

impl FileHashConfig {
    /// Creates a new configuration with default settings.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the read buffer size.
    #[must_use]
    pub const fn with_buffer_size(mut self, size: usize) -> Self {
        self.buffer_size = size;
        self
    }

    /// Sets the minimum file size threshold.
    #[must_use]
    pub const fn with_min_file_size(mut self, size: u64) -> Self {
        self.min_file_size = size;
        self
    }

    /// Sets the maximum file size for in-memory processing.
    #[must_use]
    pub const fn with_max_memory_file_size(mut self, size: u64) -> Self {
        self.max_memory_file_size = size;
        self
    }
}
