//! crates/checksums/src/parallel.rs
//!
//! Parallel checksum computation utilities using rayon.
//!
//! This module provides parallel versions of checksum operations,
//! enabling concurrent digest computation for improved performance
//! when processing multiple data blocks or files.
//!
//! # File Hashing
//!
//! The module provides functions for hashing multiple files concurrently:
//!
//! ```ignore
//! use checksums::parallel::{hash_files_parallel, FileHashResult};
//! use checksums::strong::Sha256;
//! use std::path::PathBuf;
//!
//! let files = vec![
//!     PathBuf::from("/path/to/file1"),
//!     PathBuf::from("/path/to/file2"),
//! ];
//!
//! let results = hash_files_parallel::<Sha256>(&files, 64 * 1024);
//!
//! for result in results {
//!     match result.digest {
//!         Ok(digest) => println!("{}: {:?}", result.path.display(), digest.as_ref()),
//!         Err(e) => eprintln!("{}: error - {}", result.path.display(), e),
//!     }
//! }
//! ```

use rayon::prelude::*;
use std::fs::File;
use std::io::{self, BufReader, Read};
use std::path::{Path, PathBuf};

use crate::rolling::RollingChecksum;
use crate::strong::StrongDigest;

/// Minimum number of blocks at which parallel computation becomes beneficial.
///
/// Below this threshold, the overhead of rayon's work-stealing scheduler
/// outweighs the benefits of parallelism. The runtime-selecting `compute_*_auto`
/// functions use this value to choose between sequential and parallel paths.
pub const PARALLEL_BLOCK_THRESHOLD: usize = 8;

/// Computes strong digests for multiple data blocks, automatically choosing
/// parallel or sequential execution based on the block count.
///
/// When `blocks.len() >= PARALLEL_BLOCK_THRESHOLD`, computation is performed
/// in parallel using rayon. Otherwise, sequential iteration is used to avoid
/// thread-pool overhead.
///
/// # Example
///
/// ```rust
/// use checksums::parallel::compute_digests_auto;
/// use checksums::strong::Md5;
///
/// let blocks: Vec<&[u8]> = vec![b"block1", b"block2", b"block3"];
/// let digests = compute_digests_auto::<Md5, _>(&blocks);
/// assert_eq!(digests.len(), 3);
/// ```
pub fn compute_digests_auto<D, T>(blocks: &[T]) -> Vec<D::Digest>
where
    D: StrongDigest + Send,
    D::Seed: Default + Clone + Send + Sync,
    D::Digest: Send,
    T: AsRef<[u8]> + Sync,
{
    if blocks.len() >= PARALLEL_BLOCK_THRESHOLD {
        compute_digests_parallel::<D, T>(blocks)
    } else {
        blocks
            .iter()
            .map(|block| D::digest(block.as_ref()))
            .collect()
    }
}

/// Computes block signatures (rolling + strong checksums) for multiple blocks,
/// automatically choosing parallel or sequential execution based on block count.
///
/// When `blocks.len() >= PARALLEL_BLOCK_THRESHOLD`, computation is performed
/// in parallel using rayon. Otherwise, sequential iteration is used.
///
/// # Example
///
/// ```rust
/// use checksums::parallel::{compute_block_signatures_auto, BlockSignature};
/// use checksums::strong::Sha256;
///
/// let blocks: Vec<&[u8]> = vec![b"block1", b"block2", b"block3"];
/// let signatures = compute_block_signatures_auto::<Sha256, _>(&blocks);
/// assert_eq!(signatures.len(), 3);
/// ```
pub fn compute_block_signatures_auto<D, T>(blocks: &[T]) -> Vec<BlockSignature<D::Digest>>
where
    D: StrongDigest + Send,
    D::Seed: Default + Clone + Send + Sync,
    D::Digest: Send,
    T: AsRef<[u8]> + Sync,
{
    if blocks.len() >= PARALLEL_BLOCK_THRESHOLD {
        compute_block_signatures_parallel::<D, T>(blocks)
    } else {
        blocks
            .iter()
            .map(|block| {
                let data = block.as_ref();
                let mut rolling = RollingChecksum::new();
                rolling.update(data);
                BlockSignature {
                    rolling: rolling.value(),
                    strong: D::digest(data),
                }
            })
            .collect()
    }
}

/// Computes strong digests for multiple data blocks in parallel.
///
/// Each block is hashed independently using the specified digest algorithm.
/// This is useful for computing file block signatures during delta detection.
///
/// # Type Parameters
///
/// - `D`: The digest algorithm implementing [`StrongDigest`]
///
/// # Example
///
/// ```ignore
/// use checksums::{parallel::compute_digests_parallel, strong::Md5};
///
/// let blocks: Vec<&[u8]> = vec![b"block1", b"block2", b"block3"];
/// let digests = compute_digests_parallel::<Md5, _>(&blocks);
/// assert_eq!(digests.len(), 3);
/// ```
pub fn compute_digests_parallel<D, T>(blocks: &[T]) -> Vec<D::Digest>
where
    D: StrongDigest + Send,
    D::Seed: Default + Clone + Send + Sync,
    D::Digest: Send,
    T: AsRef<[u8]> + Sync,
{
    blocks
        .par_iter()
        .map(|block| D::digest(block.as_ref()))
        .collect()
}

/// Computes strong digests with a seed for multiple data blocks in parallel.
///
/// Similar to [`compute_digests_parallel`] but allows specifying a seed
/// value for algorithms that support seeded hashing (e.g., XXH64).
///
/// # Example
///
/// ```ignore
/// use checksums::{parallel::compute_digests_with_seed_parallel, strong::Xxh64};
///
/// let blocks: Vec<&[u8]> = vec![b"block1", b"block2", b"block3"];
/// let seed = 42u64;
/// let digests = compute_digests_with_seed_parallel::<Xxh64, _>(&blocks, seed);
/// assert_eq!(digests.len(), 3);
/// ```
pub fn compute_digests_with_seed_parallel<D, T>(blocks: &[T], seed: D::Seed) -> Vec<D::Digest>
where
    D: StrongDigest + Send,
    D::Seed: Clone + Send + Sync,
    D::Digest: Send,
    T: AsRef<[u8]> + Sync,
{
    blocks
        .par_iter()
        .map(|block| D::digest_with_seed(seed.clone(), block.as_ref()))
        .collect()
}

/// Computes rolling checksums for multiple data blocks in parallel.
///
/// Each block gets its own rolling checksum computed independently.
/// Returns the packed 32-bit checksum values suitable for hash table lookups.
///
/// # Example
///
/// ```ignore
/// use checksums::parallel::compute_rolling_checksums_parallel;
///
/// let blocks: Vec<&[u8]> = vec![b"block1", b"block2", b"block3"];
/// let checksums = compute_rolling_checksums_parallel(&blocks);
/// assert_eq!(checksums.len(), 3);
/// ```
pub fn compute_rolling_checksums_parallel<T>(blocks: &[T]) -> Vec<u32>
where
    T: AsRef<[u8]> + Sync,
{
    blocks
        .par_iter()
        .map(|block| {
            let mut checksum = RollingChecksum::new();
            checksum.update(block.as_ref());
            checksum.value()
        })
        .collect()
}

/// Result of computing both rolling and strong checksums for a block.
#[derive(Clone, Debug)]
pub struct BlockSignature<D> {
    /// The rolling checksum (weak hash) for fast matching.
    pub rolling: u32,
    /// The strong digest for collision verification.
    pub strong: D,
}

/// Computes both rolling and strong checksums for multiple blocks in parallel.
///
/// This is the primary function for building block signatures during
/// delta detection. Each block gets both a rolling checksum (for fast
/// hash table lookup) and a strong digest (for collision verification).
///
/// # Example
///
/// ```ignore
/// use checksums::{parallel::compute_block_signatures_parallel, strong::Md5};
///
/// let blocks: Vec<&[u8]> = vec![b"block1", b"block2", b"block3"];
/// let signatures = compute_block_signatures_parallel::<Md5, _>(&blocks);
///
/// for sig in &signatures {
///     println!("Rolling: {:08x}, Strong: {:?}", sig.rolling, sig.strong.as_ref());
/// }
/// ```
pub fn compute_block_signatures_parallel<D, T>(blocks: &[T]) -> Vec<BlockSignature<D::Digest>>
where
    D: StrongDigest + Send,
    D::Seed: Default + Clone + Send + Sync,
    D::Digest: Send,
    T: AsRef<[u8]> + Sync,
{
    blocks
        .par_iter()
        .map(|block| {
            let data = block.as_ref();

            let mut rolling = RollingChecksum::new();
            rolling.update(data);

            BlockSignature {
                rolling: rolling.value(),
                strong: D::digest(data),
            }
        })
        .collect()
}

/// Processes data blocks in parallel, applying a custom function to each.
///
/// This is a generic parallel processor for custom checksum operations.
/// Use this when the built-in functions don't match your needs.
///
/// # Example
///
/// ```ignore
/// use checksums::parallel::process_blocks_parallel;
/// use checksums::RollingChecksum;
///
/// let blocks: Vec<&[u8]> = vec![b"block1", b"block2", b"block3"];
///
/// // Custom processor that computes rolling checksum and length
/// let results: Vec<(u32, usize)> = process_blocks_parallel(&blocks, |block| {
///     let mut checksum = RollingChecksum::new();
///     checksum.update(block);
///     (checksum.value(), block.len())
/// });
/// ```
pub fn process_blocks_parallel<T, R, F>(blocks: &[T], f: F) -> Vec<R>
where
    T: AsRef<[u8]> + Sync,
    R: Send,
    F: Fn(&[u8]) -> R + Sync + Send,
{
    blocks.par_iter().map(|block| f(block.as_ref())).collect()
}

/// Filters blocks in parallel based on their rolling checksum.
///
/// Returns indices of blocks whose rolling checksum matches the predicate.
/// Useful for finding candidate blocks during delta matching.
///
/// # Example
///
/// ```ignore
/// use checksums::parallel::filter_blocks_by_checksum;
///
/// let blocks: Vec<&[u8]> = vec![b"block1", b"block2", b"block3"];
/// let target_mask = 0xFFFF0000u32;
/// let target_value = 0x12340000u32;
///
/// // Find blocks whose upper 16 bits match
/// let matches = filter_blocks_by_checksum(&blocks, |checksum| {
///     (checksum & target_mask) == target_value
/// });
/// ```
pub fn filter_blocks_by_checksum<T, F>(blocks: &[T], predicate: F) -> Vec<usize>
where
    T: AsRef<[u8]> + Sync,
    F: Fn(u32) -> bool + Sync + Send,
{
    blocks
        .par_iter()
        .enumerate()
        .filter_map(|(i, block)| {
            let mut checksum = RollingChecksum::new();
            checksum.update(block.as_ref());
            if predicate(checksum.value()) {
                Some(i)
            } else {
                None
            }
        })
        .collect()
}

// ============================================================================
// File Hashing Functions
// ============================================================================

/// Result of hashing a single file.
#[derive(Debug)]
pub struct FileHashResult<D> {
    /// The path of the file that was hashed.
    pub path: PathBuf,
    /// The computed digest, or an error if the file could not be read.
    pub digest: Result<D, io::Error>,
    /// The size of the file in bytes (0 if file could not be read).
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

/// Hashes a single file using the specified digest algorithm.
///
/// This is the internal implementation used by parallel file hashing functions.
fn hash_file_internal<D>(path: &Path, config: &FileHashConfig) -> FileHashResult<D::Digest>
where
    D: StrongDigest,
    D::Seed: Default,
{
    let result = (|| -> io::Result<(D::Digest, u64)> {
        let file = File::open(path)?;
        let metadata = file.metadata()?;
        let size = metadata.len();

        // For small files, read entirely into memory
        if size <= config.max_memory_file_size {
            let mut data = Vec::with_capacity(size as usize);
            let mut reader = BufReader::with_capacity(config.buffer_size, file);
            reader.read_to_end(&mut data)?;
            return Ok((D::digest(&data), size));
        }

        // For large files, stream in chunks
        let mut hasher = D::new();
        let mut reader = BufReader::with_capacity(config.buffer_size, file);
        let mut buffer = vec![0u8; config.buffer_size];

        loop {
            let bytes_read = reader.read(&mut buffer)?;
            if bytes_read == 0 {
                break;
            }
            hasher.update(&buffer[..bytes_read]);
        }

        Ok((hasher.finalize(), size))
    })();

    match result {
        Ok((digest, size)) => FileHashResult {
            path: path.to_path_buf(),
            digest: Ok(digest),
            size,
        },
        Err(e) => FileHashResult {
            path: path.to_path_buf(),
            digest: Err(e),
            size: 0,
        },
    }
}

/// Hashes multiple files in parallel using the specified digest algorithm.
///
/// Each file is read and hashed independently using rayon's parallel iterator.
/// Results are returned in the same order as the input paths.
///
/// # Type Parameters
///
/// - `D`: The digest algorithm implementing [`StrongDigest`]
///
/// # Arguments
///
/// * `paths` - Slice of file paths to hash
/// * `buffer_size` - Size of the read buffer for each file (e.g., 64 * 1024)
///
/// # Returns
///
/// A vector of [`FileHashResult`] containing either the computed digest or an
/// error for each file. Results are in the same order as input paths.
///
/// # Example
///
/// ```ignore
/// use checksums::parallel::hash_files_parallel;
/// use checksums::strong::Sha256;
/// use std::path::PathBuf;
///
/// let files = vec![
///     PathBuf::from("file1.txt"),
///     PathBuf::from("file2.txt"),
/// ];
///
/// let results = hash_files_parallel::<Sha256>(&files, 64 * 1024);
///
/// for result in results {
///     match result.digest {
///         Ok(digest) => println!("{}: {:x?}", result.path.display(), digest.as_ref()),
///         Err(e) => eprintln!("{}: error - {}", result.path.display(), e),
///     }
/// }
/// ```
pub fn hash_files_parallel<D>(
    paths: &[PathBuf],
    buffer_size: usize,
) -> Vec<FileHashResult<D::Digest>>
where
    D: StrongDigest + Send,
    D::Seed: Default + Clone + Send + Sync,
    D::Digest: Send,
{
    let config = FileHashConfig::default().with_buffer_size(buffer_size);
    hash_files_parallel_with_config::<D>(paths, &config)
}

/// Hashes multiple files in parallel with custom configuration.
///
/// Similar to [`hash_files_parallel`] but allows specifying a [`FileHashConfig`]
/// for fine-grained control over buffering and memory usage.
///
/// # Example
///
/// ```ignore
/// use checksums::parallel::{hash_files_parallel_with_config, FileHashConfig};
/// use checksums::strong::Md5;
/// use std::path::PathBuf;
///
/// let config = FileHashConfig::new()
///     .with_buffer_size(128 * 1024)
///     .with_max_memory_file_size(4 * 1024 * 1024);
///
/// let files: Vec<PathBuf> = vec![/* ... */];
/// let results = hash_files_parallel_with_config::<Md5>(&files, &config);
/// ```
pub fn hash_files_parallel_with_config<D>(
    paths: &[PathBuf],
    config: &FileHashConfig,
) -> Vec<FileHashResult<D::Digest>>
where
    D: StrongDigest + Send,
    D::Seed: Default + Clone + Send + Sync,
    D::Digest: Send,
{
    paths
        .par_iter()
        .map(|path| hash_file_internal::<D>(path, config))
        .collect()
}

/// Hashes multiple files in parallel with a seed value.
///
/// For algorithms that support seeded hashing (e.g., XXH64), this allows
/// specifying a seed value used for all files.
///
/// # Example
///
/// ```ignore
/// use checksums::parallel::hash_files_with_seed_parallel;
/// use checksums::strong::Xxh64;
/// use std::path::PathBuf;
///
/// let files: Vec<PathBuf> = vec![/* ... */];
/// let seed = 0x12345678u64;
/// let results = hash_files_with_seed_parallel::<Xxh64>(&files, seed, 64 * 1024);
/// ```
pub fn hash_files_with_seed_parallel<D>(
    paths: &[PathBuf],
    seed: D::Seed,
    buffer_size: usize,
) -> Vec<FileHashResult<D::Digest>>
where
    D: StrongDigest + Send,
    D::Seed: Clone + Send + Sync,
    D::Digest: Send,
{
    let config = FileHashConfig::default().with_buffer_size(buffer_size);

    paths
        .par_iter()
        .map(|path| hash_file_with_seed_internal::<D>(path, seed.clone(), &config))
        .collect()
}

/// Hashes a single file with a seed value.
fn hash_file_with_seed_internal<D>(
    path: &Path,
    seed: D::Seed,
    config: &FileHashConfig,
) -> FileHashResult<D::Digest>
where
    D: StrongDigest,
    D::Seed: Clone,
{
    let result = (|| -> io::Result<(D::Digest, u64)> {
        let file = File::open(path)?;
        let metadata = file.metadata()?;
        let size = metadata.len();

        // For small files, read entirely into memory
        if size <= config.max_memory_file_size {
            let mut data = Vec::with_capacity(size as usize);
            let mut reader = BufReader::with_capacity(config.buffer_size, file);
            reader.read_to_end(&mut data)?;
            return Ok((D::digest_with_seed(seed, &data), size));
        }

        // For large files, stream in chunks
        let mut hasher = D::with_seed(seed);
        let mut reader = BufReader::with_capacity(config.buffer_size, file);
        let mut buffer = vec![0u8; config.buffer_size];

        loop {
            let bytes_read = reader.read(&mut buffer)?;
            if bytes_read == 0 {
                break;
            }
            hasher.update(&buffer[..bytes_read]);
        }

        Ok((hasher.finalize(), size))
    })();

    match result {
        Ok((digest, size)) => FileHashResult {
            path: path.to_path_buf(),
            digest: Ok(digest),
            size,
        },
        Err(e) => FileHashResult {
            path: path.to_path_buf(),
            digest: Err(e),
            size: 0,
        },
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

/// Computes block signatures for multiple files in parallel.
///
/// For each file, computes both rolling and strong checksums for every block.
/// This is the primary function for building signatures during delta detection.
///
/// # Arguments
///
/// * `paths` - Files to process
/// * `block_size` - Size of each block to hash
/// * `buffer_size` - Read buffer size for I/O
///
/// # Example
///
/// ```ignore
/// use checksums::parallel::compute_file_signatures_parallel;
/// use checksums::strong::Md5;
/// use std::path::PathBuf;
///
/// let files: Vec<PathBuf> = vec![/* ... */];
/// let results = compute_file_signatures_parallel::<Md5>(&files, 8192, 64 * 1024);
///
/// for result in results {
///     if let Ok(sigs) = result.signatures {
///         println!("{}: {} blocks", result.path.display(), sigs.len());
///     }
/// }
/// ```
pub fn compute_file_signatures_parallel<D>(
    paths: &[PathBuf],
    block_size: usize,
    buffer_size: usize,
) -> Vec<FileSignatureResult<D::Digest>>
where
    D: StrongDigest + Send,
    D::Seed: Default + Clone + Send + Sync,
    D::Digest: Send,
{
    paths
        .par_iter()
        .map(|path| compute_file_signatures_internal::<D>(path, block_size, buffer_size))
        .collect()
}

/// Internal result type for file signature computation.
type SignatureComputeResult<D> = (Vec<BlockSignature<D>>, u64, usize);

/// Computes block signatures for a single file.
fn compute_file_signatures_internal<D>(
    path: &Path,
    block_size: usize,
    buffer_size: usize,
) -> FileSignatureResult<D::Digest>
where
    D: StrongDigest,
    D::Seed: Default,
{
    let result = (|| -> io::Result<SignatureComputeResult<D::Digest>> {
        let file = File::open(path)?;
        let metadata = file.metadata()?;
        let size = metadata.len();
        let estimated_blocks = (size as usize).div_ceil(block_size);

        let mut signatures = Vec::with_capacity(estimated_blocks);
        let mut reader = BufReader::with_capacity(buffer_size, file);
        let mut buffer = vec![0u8; block_size];

        loop {
            let mut total_read = 0;
            while total_read < block_size {
                let bytes_read = reader.read(&mut buffer[total_read..])?;
                if bytes_read == 0 {
                    break;
                }
                total_read += bytes_read;
            }

            if total_read == 0 {
                break;
            }

            let block = &buffer[..total_read];
            let mut rolling = RollingChecksum::new();
            rolling.update(block);

            signatures.push(BlockSignature {
                rolling: rolling.value(),
                strong: D::digest(block),
            });
        }

        let block_count = signatures.len();
        Ok((signatures, size, block_count))
    })();

    match result {
        Ok((signatures, size, block_count)) => FileSignatureResult {
            path: path.to_path_buf(),
            signatures: Ok(signatures),
            size,
            block_count,
        },
        Err(e) => FileSignatureResult {
            path: path.to_path_buf(),
            signatures: Err(e),
            size: 0,
            block_count: 0,
        },
    }
}

/// Iterator adapter for processing files in parallel batches.
///
/// This is useful when you have a large number of files and want to
/// process them in manageable batches while still utilizing parallelism.
pub struct ParallelFileHasher<'a, D>
where
    D: StrongDigest,
{
    paths: &'a [PathBuf],
    config: FileHashConfig,
    batch_size: usize,
    current_index: usize,
    _phantom: std::marker::PhantomData<D>,
}

impl<'a, D> ParallelFileHasher<'a, D>
where
    D: StrongDigest + Send,
    D::Seed: Default + Clone + Send + Sync,
    D::Digest: Send,
{
    /// Creates a new parallel file hasher.
    ///
    /// # Arguments
    ///
    /// * `paths` - Files to hash
    /// * `config` - Hashing configuration
    /// * `batch_size` - Number of files to process per batch
    #[must_use]
    pub fn new(paths: &'a [PathBuf], config: FileHashConfig, batch_size: usize) -> Self {
        Self {
            paths,
            config,
            batch_size: batch_size.max(1),
            current_index: 0,
            _phantom: std::marker::PhantomData,
        }
    }

    /// Processes the next batch of files.
    ///
    /// Returns `None` when all files have been processed.
    pub fn next_batch(&mut self) -> Option<Vec<FileHashResult<D::Digest>>> {
        if self.current_index >= self.paths.len() {
            return None;
        }

        let end = (self.current_index + self.batch_size).min(self.paths.len());
        let batch = &self.paths[self.current_index..end];
        self.current_index = end;

        Some(hash_files_parallel_with_config::<D>(batch, &self.config))
    }

    /// Returns the number of remaining files to process.
    #[must_use]
    pub fn remaining(&self) -> usize {
        self.paths.len().saturating_sub(self.current_index)
    }

    /// Returns the total number of files.
    #[must_use]
    pub fn total(&self) -> usize {
        self.paths.len()
    }

    /// Returns the number of files already processed.
    #[must_use]
    pub fn processed(&self) -> usize {
        self.current_index
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::strong::{Md5, Sha256, Xxh3, Xxh64};
    use std::io::Write;
    use tempfile::TempDir;

    fn create_test_blocks() -> Vec<Vec<u8>> {
        vec![
            b"block one content".to_vec(),
            b"block two content".to_vec(),
            b"block three content".to_vec(),
            b"block four content".to_vec(),
        ]
    }

    #[test]
    fn compute_digests_parallel_matches_sequential() {
        let blocks = create_test_blocks();

        let parallel_digests = compute_digests_parallel::<Md5, _>(&blocks);

        let sequential_digests: Vec<_> = blocks.iter().map(|b| Md5::digest(b.as_slice())).collect();

        assert_eq!(parallel_digests.len(), sequential_digests.len());
        for (p, s) in parallel_digests.iter().zip(sequential_digests.iter()) {
            assert_eq!(p.as_ref(), s.as_ref());
        }
    }

    #[test]
    fn compute_digests_with_seed_parallel_works() {
        let blocks = create_test_blocks();
        let seed = 12345u64;

        let parallel_digests = compute_digests_with_seed_parallel::<Xxh64, _>(&blocks, seed);

        let sequential_digests: Vec<_> = blocks
            .iter()
            .map(|b| Xxh64::digest(seed, b.as_slice()))
            .collect();

        assert_eq!(parallel_digests.len(), sequential_digests.len());
        for (p, s) in parallel_digests.iter().zip(sequential_digests.iter()) {
            assert_eq!(p.as_ref(), s.as_ref());
        }
    }

    #[test]
    fn compute_rolling_checksums_parallel_matches_sequential() {
        let blocks = create_test_blocks();

        let parallel_checksums = compute_rolling_checksums_parallel(&blocks);

        let sequential_checksums: Vec<_> = blocks
            .iter()
            .map(|b| {
                let mut checksum = RollingChecksum::new();
                checksum.update(b.as_slice());
                checksum.value()
            })
            .collect();

        assert_eq!(parallel_checksums, sequential_checksums);
    }

    #[test]
    fn compute_block_signatures_parallel_works() {
        let blocks = create_test_blocks();

        let signatures = compute_block_signatures_parallel::<Sha256, _>(&blocks);

        assert_eq!(signatures.len(), blocks.len());

        // Verify each signature matches sequential computation
        for (i, sig) in signatures.iter().enumerate() {
            let mut rolling = RollingChecksum::new();
            rolling.update(blocks[i].as_slice());
            assert_eq!(sig.rolling, rolling.value());

            let strong = Sha256::digest(blocks[i].as_slice());
            assert_eq!(sig.strong.as_ref(), strong.as_ref());
        }
    }

    #[test]
    fn process_blocks_parallel_with_custom_function() {
        let blocks = create_test_blocks();

        let results: Vec<(u32, usize)> = process_blocks_parallel(&blocks, |block| {
            let mut checksum = RollingChecksum::new();
            checksum.update(block);
            (checksum.value(), block.len())
        });

        assert_eq!(results.len(), blocks.len());

        for (i, (checksum, len)) in results.iter().enumerate() {
            assert_eq!(*len, blocks[i].len());

            let mut expected = RollingChecksum::new();
            expected.update(blocks[i].as_slice());
            assert_eq!(*checksum, expected.value());
        }
    }

    #[test]
    fn filter_blocks_by_checksum_works() {
        let blocks = create_test_blocks();

        // Get all checksums first
        let checksums = compute_rolling_checksums_parallel(&blocks);

        // Filter for blocks whose checksum has bit 0 set
        let matching_indices = filter_blocks_by_checksum(&blocks, |c| c & 1 == 1);

        // Verify results
        for &i in &matching_indices {
            assert!(checksums[i] & 1 == 1);
        }

        // Verify non-matching blocks
        for (i, &c) in checksums.iter().enumerate() {
            if c & 1 == 0 {
                assert!(!matching_indices.contains(&i));
            }
        }
    }

    #[test]
    fn parallel_handles_empty_input() {
        let blocks: Vec<Vec<u8>> = vec![];

        let digests = compute_digests_parallel::<Md5, _>(&blocks);
        assert!(digests.is_empty());

        let checksums = compute_rolling_checksums_parallel(&blocks);
        assert!(checksums.is_empty());

        let signatures = compute_block_signatures_parallel::<Md5, _>(&blocks);
        assert!(signatures.is_empty());
    }

    #[test]
    fn parallel_handles_single_block() {
        let blocks = vec![b"single block".to_vec()];

        let digests = compute_digests_parallel::<Md5, _>(&blocks);
        assert_eq!(digests.len(), 1);

        let expected = Md5::digest(b"single block");
        assert_eq!(digests[0].as_ref(), expected.as_ref());
    }

    // ========================================================================
    // File Hashing Tests
    // ========================================================================

    fn create_test_files(dir: &TempDir) -> Vec<PathBuf> {
        let files: Vec<(&str, &[u8])> = vec![
            ("file1.txt", b"Content of file one"),
            ("file2.txt", b"Content of file two"),
            ("file3.txt", b"Content of file three with more data"),
            ("empty.txt", b""),
        ];

        files
            .into_iter()
            .map(|(name, content)| {
                let path = dir.path().join(name);
                let mut file = File::create(&path).unwrap();
                file.write_all(content).unwrap();
                path
            })
            .collect()
    }

    #[test]
    fn hash_files_parallel_basic() {
        let dir = TempDir::new().unwrap();
        let files = create_test_files(&dir);

        let results = hash_files_parallel::<Md5>(&files, 64 * 1024);

        assert_eq!(results.len(), files.len());

        // Verify each result
        for (result, path) in results.iter().zip(files.iter()) {
            assert_eq!(&result.path, path);
            assert!(result.digest.is_ok());

            // Verify digest matches direct computation
            let content = std::fs::read(path).unwrap();
            let expected = Md5::digest(&content);
            assert_eq!(result.digest.as_ref().unwrap().as_ref(), expected.as_ref());
            assert_eq!(result.size, content.len() as u64);
        }
    }

    #[test]
    fn hash_files_parallel_with_sha256() {
        let dir = TempDir::new().unwrap();
        let files = create_test_files(&dir);

        let results = hash_files_parallel::<Sha256>(&files, 32 * 1024);

        for result in &results {
            assert!(result.digest.is_ok());

            let content = std::fs::read(&result.path).unwrap();
            let expected = Sha256::digest(&content);
            assert_eq!(result.digest.as_ref().unwrap().as_ref(), expected.as_ref());
        }
    }

    #[test]
    fn hash_files_parallel_handles_missing_file() {
        let dir = TempDir::new().unwrap();
        let mut files = create_test_files(&dir);
        files.push(dir.path().join("nonexistent.txt"));

        let results = hash_files_parallel::<Md5>(&files, 64 * 1024);

        assert_eq!(results.len(), files.len());

        // Last file should have an error
        let last_result = results.last().unwrap();
        assert!(last_result.digest.is_err());
        assert_eq!(last_result.size, 0);
    }

    #[test]
    fn hash_files_parallel_empty_list() {
        let files: Vec<PathBuf> = vec![];
        let results = hash_files_parallel::<Md5>(&files, 64 * 1024);
        assert!(results.is_empty());
    }

    #[test]
    fn hash_files_parallel_single_file() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("single.txt");
        let content = b"single file content";
        std::fs::write(&path, content).unwrap();

        let results = hash_files_parallel::<Sha256>(&[path.clone()], 64 * 1024);

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].path, path);
        assert!(results[0].digest.is_ok());
        assert_eq!(results[0].size, content.len() as u64);

        let expected = Sha256::digest(content);
        assert_eq!(
            results[0].digest.as_ref().unwrap().as_ref(),
            expected.as_ref()
        );
    }

    #[test]
    fn hash_files_with_seed_parallel_works() {
        let dir = TempDir::new().unwrap();
        let files = create_test_files(&dir);
        let seed = 0xDEADBEEFu64;

        let results = hash_files_with_seed_parallel::<Xxh64>(&files, seed, 64 * 1024);

        for result in &results {
            assert!(result.digest.is_ok());

            let content = std::fs::read(&result.path).unwrap();
            let expected = Xxh64::digest(seed, &content);
            assert_eq!(result.digest.as_ref().unwrap().as_ref(), expected.as_ref());
        }
    }

    #[test]
    fn hash_files_with_different_seeds_produces_different_digests() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("seeded.txt");
        std::fs::write(&path, b"test content").unwrap();

        let files = vec![path];

        let results1 = hash_files_with_seed_parallel::<Xxh64>(&files, 1u64, 64 * 1024);
        let results2 = hash_files_with_seed_parallel::<Xxh64>(&files, 2u64, 64 * 1024);

        assert_ne!(
            results1[0].digest.as_ref().unwrap().as_ref(),
            results2[0].digest.as_ref().unwrap().as_ref()
        );
    }

    #[test]
    fn hash_files_parallel_with_config_custom_buffer() {
        let dir = TempDir::new().unwrap();
        let files = create_test_files(&dir);

        let config = FileHashConfig::new()
            .with_buffer_size(4096)
            .with_max_memory_file_size(512);

        let results = hash_files_parallel_with_config::<Md5>(&files, &config);

        for result in &results {
            assert!(result.digest.is_ok());

            let content = std::fs::read(&result.path).unwrap();
            let expected = Md5::digest(&content);
            assert_eq!(result.digest.as_ref().unwrap().as_ref(), expected.as_ref());
        }
    }

    #[test]
    fn hash_files_parallel_large_file_streaming() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("large.bin");

        // Create a file larger than max_memory_file_size
        let size = 2 * 1024 * 1024; // 2 MB
        let data: Vec<u8> = (0..size).map(|i| (i % 256) as u8).collect();
        std::fs::write(&path, &data).unwrap();

        let config = FileHashConfig::new()
            .with_buffer_size(64 * 1024)
            .with_max_memory_file_size(1024 * 1024); // 1 MB threshold

        let results = hash_files_parallel_with_config::<Sha256>(&[path], &config);

        assert_eq!(results.len(), 1);
        assert!(results[0].digest.is_ok());
        assert_eq!(results[0].size, size as u64);

        let expected = Sha256::digest(&data);
        assert_eq!(
            results[0].digest.as_ref().unwrap().as_ref(),
            expected.as_ref()
        );
    }

    #[test]
    fn compute_file_signatures_parallel_basic() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("sigtest.bin");

        // Create a file that spans multiple blocks
        let data: Vec<u8> = (0..10000).map(|i| (i % 256) as u8).collect();
        std::fs::write(&path, &data).unwrap();

        let block_size = 1024;
        let results =
            compute_file_signatures_parallel::<Md5>(&[path.clone()], block_size, 64 * 1024);

        assert_eq!(results.len(), 1);
        let result = &results[0];
        assert!(result.signatures.is_ok());

        let signatures = result.signatures.as_ref().unwrap();
        let expected_blocks = data.len().div_ceil(block_size);
        assert_eq!(signatures.len(), expected_blocks);
        assert_eq!(result.block_count, expected_blocks);
        assert_eq!(result.size, data.len() as u64);

        // Verify each block signature
        for (i, sig) in signatures.iter().enumerate() {
            let start = i * block_size;
            let end = (start + block_size).min(data.len());
            let block = &data[start..end];

            let mut expected_rolling = RollingChecksum::new();
            expected_rolling.update(block);
            assert_eq!(sig.rolling, expected_rolling.value());

            let expected_strong = Md5::digest(block);
            assert_eq!(sig.strong.as_ref(), expected_strong.as_ref());
        }
    }

    #[test]
    fn compute_file_signatures_parallel_multiple_files() {
        let dir = TempDir::new().unwrap();
        let files = create_test_files(&dir);

        let results = compute_file_signatures_parallel::<Sha256>(&files, 16, 4096);

        assert_eq!(results.len(), files.len());

        for result in &results {
            assert!(result.signatures.is_ok());
        }
    }

    #[test]
    fn compute_file_signatures_handles_empty_file() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("empty.bin");
        std::fs::write(&path, b"").unwrap();

        let results = compute_file_signatures_parallel::<Md5>(&[path], 1024, 4096);

        assert_eq!(results.len(), 1);
        assert!(results[0].signatures.is_ok());
        assert!(results[0].signatures.as_ref().unwrap().is_empty());
        assert_eq!(results[0].size, 0);
        assert_eq!(results[0].block_count, 0);
    }

    #[test]
    fn parallel_file_hasher_batching() {
        let dir = TempDir::new().unwrap();

        // Create 10 test files
        let files: Vec<PathBuf> = (0..10)
            .map(|i| {
                let path = dir.path().join(format!("file{i}.txt"));
                std::fs::write(&path, format!("Content of file {i}")).unwrap();
                path
            })
            .collect();

        let config = FileHashConfig::default();
        let mut hasher = ParallelFileHasher::<Md5>::new(&files, config, 3);

        assert_eq!(hasher.total(), 10);
        assert_eq!(hasher.remaining(), 10);
        assert_eq!(hasher.processed(), 0);

        // Process first batch
        let batch1 = hasher.next_batch().unwrap();
        assert_eq!(batch1.len(), 3);
        assert_eq!(hasher.processed(), 3);
        assert_eq!(hasher.remaining(), 7);

        // Process second batch
        let batch2 = hasher.next_batch().unwrap();
        assert_eq!(batch2.len(), 3);
        assert_eq!(hasher.processed(), 6);

        // Process third batch
        let batch3 = hasher.next_batch().unwrap();
        assert_eq!(batch3.len(), 3);
        assert_eq!(hasher.processed(), 9);

        // Process last batch (partial)
        let batch4 = hasher.next_batch().unwrap();
        assert_eq!(batch4.len(), 1);
        assert_eq!(hasher.processed(), 10);

        // No more batches
        assert!(hasher.next_batch().is_none());
    }

    #[test]
    fn file_hash_config_builder() {
        let config = FileHashConfig::new()
            .with_buffer_size(128 * 1024)
            .with_min_file_size(1024)
            .with_max_memory_file_size(4 * 1024 * 1024);

        assert_eq!(config.buffer_size, 128 * 1024);
        assert_eq!(config.min_file_size, 1024);
        assert_eq!(config.max_memory_file_size, 4 * 1024 * 1024);
    }

    #[test]
    fn file_hash_result_includes_size() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("sized.txt");
        let content = b"precise content length";
        std::fs::write(&path, content).unwrap();

        let results = hash_files_parallel::<Md5>(&[path], 4096);

        assert_eq!(results[0].size, content.len() as u64);
    }

    #[test]
    fn parallel_file_hashing_preserves_order() {
        let dir = TempDir::new().unwrap();

        // Create files with different content to ensure distinct hashes
        let files: Vec<PathBuf> = (0..20)
            .map(|i| {
                let path = dir.path().join(format!("ordered{i:02}.txt"));
                std::fs::write(&path, format!("Unique content {i}")).unwrap();
                path
            })
            .collect();

        let results = hash_files_parallel::<Sha256>(&files, 4096);

        // Verify order is preserved
        for (result, expected_path) in results.iter().zip(files.iter()) {
            assert_eq!(&result.path, expected_path);
        }
    }

    #[test]
    fn parallel_file_hashing_with_xxh3() {
        let dir = TempDir::new().unwrap();
        let files = create_test_files(&dir);

        let results = hash_files_parallel::<Xxh3>(&files, 64 * 1024);

        for result in &results {
            assert!(result.digest.is_ok());

            let content = std::fs::read(&result.path).unwrap();
            let expected = Xxh3::digest(0, &content);
            assert_eq!(result.digest.as_ref().unwrap().as_ref(), expected.as_ref());
        }
    }
}
