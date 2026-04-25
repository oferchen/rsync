//! Parallel file hashing and signature computation.
//!
//! Provides concurrent file I/O with digest computation using rayon.
//! For files above the mmap threshold, memory-mapping is attempted first
//! to avoid per-chunk read syscalls, with buffered I/O as fallback.

use fast_io::mmap_reader::{MMAP_THRESHOLD, MmapReader};
use rayon::prelude::*;
use std::fs::File;
use std::io::{self, BufReader, Read};
use std::path::{Path, PathBuf};

use crate::rolling::RollingChecksum;
use crate::strong::StrongDigest;

use super::types::{BlockSignature, FileHashConfig, FileHashResult, FileSignatureResult};

/// Hashes a single file using the specified digest algorithm.
///
/// For files above [`MMAP_THRESHOLD`], memory-mapping is attempted first to
/// avoid per-chunk read syscalls. Falls back to buffered I/O on mmap failure.
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

        // For large files, try mmap for zero-copy hashing
        if size >= MMAP_THRESHOLD {
            if let Ok(mmap) = MmapReader::open(path) {
                let _ = mmap.advise_sequential();
                return Ok((D::digest(mmap.as_slice()), size));
            }
        }

        // Fallback: stream in chunks via buffered I/O
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
/// ```
/// use checksums::parallel::hash_files_parallel;
/// use checksums::strong::Sha256;
///
/// let dir = tempfile::tempdir().unwrap();
/// let file1 = dir.path().join("file1.txt");
/// let file2 = dir.path().join("file2.txt");
/// std::fs::write(&file1, b"hello").unwrap();
/// std::fs::write(&file2, b"world").unwrap();
///
/// let files = vec![file1, file2];
/// let results = hash_files_parallel::<Sha256>(&files, 64 * 1024);
///
/// assert_eq!(results.len(), 2);
/// assert!(results[0].digest.is_ok());
/// assert!(results[1].digest.is_ok());
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
/// ```
/// use checksums::parallel::{hash_files_parallel_with_config, FileHashConfig};
/// use checksums::strong::Md5;
///
/// let dir = tempfile::tempdir().unwrap();
/// let path = dir.path().join("data.bin");
/// std::fs::write(&path, b"test data").unwrap();
///
/// let config = FileHashConfig::new()
///     .with_buffer_size(128 * 1024)
///     .with_max_memory_file_size(4 * 1024 * 1024);
///
/// let files = vec![path];
/// let results = hash_files_parallel_with_config::<Md5>(&files, &config);
/// assert_eq!(results.len(), 1);
/// assert!(results[0].digest.is_ok());
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
    // Ordering: results must correspond 1:1 with input paths for whole-file checksum comparison.
    // Preserved by par_iter().map().collect() (rayon preserves index order).
    // Violation mismatches file hashes with paths, causing incorrect transfer decisions.
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
/// ```
/// use checksums::parallel::hash_files_with_seed_parallel;
/// use checksums::strong::Xxh64;
///
/// let dir = tempfile::tempdir().unwrap();
/// let path = dir.path().join("data.bin");
/// std::fs::write(&path, b"seeded hash input").unwrap();
///
/// let files = vec![path];
/// let seed = 0x12345678u64;
/// let results = hash_files_with_seed_parallel::<Xxh64>(&files, seed, 64 * 1024);
/// assert_eq!(results.len(), 1);
/// assert!(results[0].digest.is_ok());
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
///
/// Uses the same mmap-first strategy as [`hash_file_internal`].
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

        // For large files, try mmap for zero-copy hashing
        if size >= MMAP_THRESHOLD {
            if let Ok(mmap) = MmapReader::open(path) {
                let _ = mmap.advise_sequential();
                return Ok((D::digest_with_seed(seed, mmap.as_slice()), size));
            }
        }

        // Fallback: stream in chunks via buffered I/O
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
/// ```
/// use checksums::parallel::compute_file_signatures_parallel;
/// use checksums::strong::Md5;
///
/// let dir = tempfile::tempdir().unwrap();
/// let path = dir.path().join("test.bin");
/// std::fs::write(&path, b"block signature test data").unwrap();
///
/// let files = vec![path];
/// let results = compute_file_signatures_parallel::<Md5>(&files, 8192, 64 * 1024);
/// assert_eq!(results.len(), 1);
/// assert!(results[0].signatures.is_ok());
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
    // Ordering: results must correspond 1:1 with input paths by position.
    // Preserved by par_iter().map().collect() (rayon preserves index order).
    // Violation mismatches file signatures with paths.
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

        // For files above the mmap threshold, map the entire file and slice
        // blocks directly from mapped memory - zero read syscalls.
        if size >= MMAP_THRESHOLD {
            if let Ok(mmap) = MmapReader::open(path) {
                let _ = mmap.advise_sequential();
                let data = mmap.as_slice();
                let mut signatures = Vec::with_capacity(estimated_blocks);

                for chunk in data.chunks(block_size) {
                    let mut rolling = RollingChecksum::new();
                    rolling.update(chunk);
                    signatures.push(BlockSignature {
                        rolling: rolling.value(),
                        strong: D::digest(chunk),
                    });
                }

                let block_count = signatures.len();
                return Ok((signatures, size, block_count));
            }
        }

        // Fallback: buffered I/O block-by-block
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
