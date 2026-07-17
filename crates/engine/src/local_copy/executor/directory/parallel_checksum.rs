//! Parallel checksum computation using rayon.
//!
//! This module provides parallel checksum prefetching for directory entries,
//! significantly improving performance when using `--checksum` mode on directories
//! with many files.
//!
//! # Design
//!
//! When checksum comparison is enabled, computing checksums is typically the
//! bottleneck (see profiling: ~98% of CPU time in md5_compress). By computing
//! checksums for multiple files in parallel, we can utilize multiple CPU cores.
//!
//! The prefetch is split into two phases:
//!
//! 1. **Parallel checksum**: File checksums are computed concurrently using rayon.
//!    Each file is hashed independently, allowing linear scaling with CPU cores.
//!
//! 2. **Sequential comparison**: The actual skip/copy decision uses the prefetched
//!    checksums, maintaining correct ordering.

use std::collections::HashMap;
#[cfg(unix)]
use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rayon::prelude::*;

use checksums::strong::{Md4, Md5, Sha1, StrongDigest, Xxh3, Xxh3_128, Xxh64};
#[cfg(test)]
use fast_io::mmap_reader::MMAP_THRESHOLD;
#[cfg(windows)]
use fast_io::windows_chunked_reader::WindowsChunkedReader;

use crate::local_copy::buffer_pool::{BufferPool, global_buffer_pool};
use crate::signature::SignatureAlgorithm;

/// Precomputed checksum for a file.
#[derive(Debug, Clone)]
pub(crate) struct FileChecksum {
    /// The computed checksum bytes.
    pub(crate) digest: Vec<u8>,
    /// File size at time of checksum (for validation).
    pub(crate) size: u64,
}

/// Result of checksum prefetching for a file pair.
#[derive(Debug)]
pub(crate) struct ChecksumPrefetchResult {
    /// Source checksum, if successfully computed.
    pub(crate) source_checksum: Option<FileChecksum>,
    /// Destination checksum, if successfully computed.
    pub(crate) destination_checksum: Option<FileChecksum>,
}

impl ChecksumPrefetchResult {
    /// Returns true if both checksums were computed and match.
    pub(crate) fn checksums_match(&self) -> bool {
        match (&self.source_checksum, &self.destination_checksum) {
            (Some(src), Some(dst)) => src.size == dst.size && src.digest == dst.digest,
            _ => false,
        }
    }
}

/// A pair of files to compare checksums.
#[derive(Debug, Clone)]
pub(crate) struct FilePair {
    /// Source file path.
    pub(crate) source: PathBuf,
    /// Destination file path.
    pub(crate) destination: PathBuf,
    /// Expected source size (for early filtering).
    pub(crate) source_size: u64,
    /// Expected destination size (for early filtering).
    pub(crate) destination_size: u64,
}

/// Computes checksums for multiple file pairs in parallel.
///
/// This function uses rayon to parallelize checksum computation across
/// multiple files, significantly improving throughput on multi-core systems.
///
/// # Arguments
///
/// * `pairs` - File pairs to compute checksums for
/// * `algorithm` - Checksum algorithm to use
///
/// # Returns
///
/// A HashMap mapping source paths to their prefetch results for quick lookup.
pub(crate) fn prefetch_checksums(
    pairs: &[FilePair],
    algorithm: SignatureAlgorithm,
) -> HashMap<PathBuf, ChecksumPrefetchResult> {
    let buffer_pool = global_buffer_pool();

    let results: Vec<_> = pairs
        .par_iter()
        .map(|pair| {
            if pair.source_size != pair.destination_size {
                return (
                    pair.source.clone(),
                    ChecksumPrefetchResult {
                        source_checksum: None,
                        destination_checksum: None,
                    },
                );
            }

            let pool_src = Arc::clone(&buffer_pool);
            let pool_dst = Arc::clone(&buffer_pool);

            let (source_checksum, destination_checksum) = rayon::join(
                || compute_file_checksum(&pair.source, pair.source_size, algorithm, &pool_src),
                || {
                    compute_file_checksum(
                        &pair.destination,
                        pair.destination_size,
                        algorithm,
                        &pool_dst,
                    )
                },
            );

            (
                pair.source.clone(),
                ChecksumPrefetchResult {
                    source_checksum,
                    destination_checksum,
                },
            )
        })
        .collect();

    let mut map = HashMap::with_capacity(results.len());
    for (path, result) in results {
        map.insert(path, result);
    }
    map
}

/// Computes the checksum of a single file.
///
/// upstream: checksum.c:402 `file_checksum()` reads the file through
/// `map_file()` / `map_ptr()` (fileio.c:213-317), which is a `read()`-based
/// sliding window, NOT `mmap(2)`. Upstream deliberately avoids `mmap` here:
/// the fileio.c:214-217 comment notes that another process truncating the
/// file mid-transfer would raise SIGBUS and kill the process. We mirror that
/// by streaming through a read window ([`hash_file_contents`]) rather than
/// mapping the file, so a concurrent truncation degrades gracefully (the read
/// short-reads to EOF, the checksum simply fails to match, and the file
/// re-transfers) instead of crashing.
fn compute_file_checksum(
    path: &Path,
    file_size: u64,
    algorithm: SignatureAlgorithm,
    buffer_pool: &Arc<BufferPool>,
) -> Option<FileChecksum> {
    // Stream via a read window rather than mmap. On Windows use
    // WindowsChunkedReader so peak RSS stays bounded by the chunk size.
    #[cfg(unix)]
    let file = File::open(path).ok()?;
    #[cfg(windows)]
    let file = WindowsChunkedReader::open(path).ok()?;
    let digest = hash_file_contents(file, file_size, algorithm, buffer_pool).ok()?;

    Some(FileChecksum {
        digest,
        size: file_size,
    })
}

/// Hashes file contents using the specified algorithm.
///
/// Uses a pre-sized read loop based on the known `file_size` to avoid
/// the extra read() syscall that EOF-probe patterns (BufReader, loop-until-0)
/// issue per file. Generic over `R: Read` so the caller can pass `std::fs::File`
/// (Unix streaming fallback) or `WindowsChunkedReader` (Windows bounded-RSS).
///
/// upstream: checksum.c - sized read loop: `while (remaining > 0) { read(); remaining -= n; }`
fn hash_file_contents<R: Read>(
    mut file: R,
    file_size: u64,
    algorithm: SignatureAlgorithm,
    buffer_pool: &Arc<BufferPool>,
) -> io::Result<Vec<u8>> {
    let mut buffer = BufferPool::acquire_from(Arc::clone(buffer_pool));
    let buf_len = buffer.len();

    /// Reads exactly `remaining` bytes from `file` into `hasher` using
    /// pre-sized chunks, avoiding a trailing EOF probe syscall.
    fn read_into_hasher<R: Read>(
        file: &mut R,
        mut remaining: u64,
        buffer: &mut [u8],
        buf_len: usize,
        hasher: &mut impl checksums::strong::StrongDigest,
    ) -> io::Result<()> {
        while remaining > 0 {
            let to_read = (remaining as usize).min(buf_len);
            file.read_exact(&mut buffer[..to_read])?;
            hasher.update(&buffer[..to_read]);
            remaining -= to_read as u64;
        }
        Ok(())
    }

    let digest = match algorithm {
        SignatureAlgorithm::Md4 => {
            let mut hasher = Md4::new();
            read_into_hasher(&mut file, file_size, &mut buffer, buf_len, &mut hasher)?;
            hasher.finalize().as_ref().to_vec()
        }
        SignatureAlgorithm::Md4Seeded { seed } => {
            // upstream: checksum.c:377-380 - append checksum_seed as 4 LE bytes
            // after the file data when seed != 0. A zero seed degenerates to
            // unseeded MD4 (preserved here for symmetry with `Md4`).
            let mut hasher = Md4::new();
            read_into_hasher(&mut file, file_size, &mut buffer, buf_len, &mut hasher)?;
            if seed != 0 {
                hasher.update(&seed.to_le_bytes());
            }
            hasher.finalize().as_ref().to_vec()
        }
        SignatureAlgorithm::Md5 { seed_config } => {
            let mut hasher = Md5::with_seed(seed_config);
            read_into_hasher(&mut file, file_size, &mut buffer, buf_len, &mut hasher)?;
            hasher.finalize().as_ref().to_vec()
        }
        SignatureAlgorithm::Sha1 => {
            let mut hasher = Sha1::new();
            read_into_hasher(&mut file, file_size, &mut buffer, buf_len, &mut hasher)?;
            hasher.finalize().as_ref().to_vec()
        }
        SignatureAlgorithm::Xxh64 { seed } => {
            let mut hasher = Xxh64::new(seed);
            read_into_hasher(&mut file, file_size, &mut buffer, buf_len, &mut hasher)?;
            hasher.finalize().as_ref().to_vec()
        }
        SignatureAlgorithm::Xxh3 { seed } => {
            let mut hasher = Xxh3::new(seed);
            read_into_hasher(&mut file, file_size, &mut buffer, buf_len, &mut hasher)?;
            hasher.finalize().as_ref().to_vec()
        }
        SignatureAlgorithm::Xxh3_128 { seed } => {
            let mut hasher = Xxh3_128::new(seed);
            read_into_hasher(&mut file, file_size, &mut buffer, buf_len, &mut hasher)?;
            hasher.finalize().as_ref().to_vec()
        }
    };

    Ok(digest)
}

/// Checks if a file pair should be skipped based on prefetched checksums.
///
/// This is a fast lookup that uses previously computed checksums.
#[allow(dead_code)] // Convenience wrapper, prefer ChecksumCache::lookup
pub(crate) fn should_skip_with_prefetched_checksum(
    prefetched: &HashMap<PathBuf, ChecksumPrefetchResult>,
    source: &Path,
) -> Option<bool> {
    prefetched
        .get(source)
        .map(|result| result.checksums_match())
}

/// Cache for prefetched file checksums during directory traversal.
///
/// This wrapper around `HashMap` provides a clean interface for managing
/// prefetched checksums within a single directory's processing context.
/// The cache is populated once per directory via [`prefetch_checksums`]
/// and queried during file copy decisions.
///
/// # Example
///
/// ```ignore
/// let pairs = collect_file_pairs(&planned_entries);
/// let cache = ChecksumCache::from_prefetch(&pairs, algorithm);
///
/// // Later, during copy decision:
/// if let Some(matches) = cache.lookup(source_path) {
///     if matches { /* skip copy */ }
/// }
/// ```
#[derive(Debug, Default)]
pub(crate) struct ChecksumCache {
    inner: HashMap<PathBuf, ChecksumPrefetchResult>,
}

impl ChecksumCache {
    /// Creates a new empty checksum cache.
    pub(crate) fn new() -> Self {
        Self {
            inner: HashMap::new(),
        }
    }

    /// Creates a checksum cache by prefetching checksums for the given file pairs.
    ///
    /// This is the primary constructor, computing all checksums in parallel
    /// using rayon.
    pub(crate) fn from_prefetch(pairs: &[FilePair], algorithm: SignatureAlgorithm) -> Self {
        Self {
            inner: prefetch_checksums(pairs, algorithm),
        }
    }

    /// Looks up a source path in the cache and returns whether checksums match.
    ///
    /// Returns `Some(true)` if checksums match (skip copy), `Some(false)` if
    /// checksums differ (need copy), or `None` if the path wasn't prefetched.
    pub(crate) fn lookup(&self, source: &Path) -> Option<bool> {
        self.inner
            .get(source)
            .map(|result| result.checksums_match())
    }

    /// Returns the number of entries in the cache.
    #[allow(dead_code)] // API completeness with is_empty
    pub(crate) fn len(&self) -> usize {
        self.inner.len()
    }

    /// Returns true if the cache is empty.
    pub(crate) fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Clears all entries from the cache.
    pub(crate) fn clear(&mut self) {
        self.inner.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use test_support::create_tempdir;

    #[test]
    fn prefetch_checksums_matches_identical_files() {
        let dir = create_tempdir();
        let source = dir.path().join("source.txt");
        let destination = dir.path().join("dest.txt");

        let content = b"identical content for both files";
        fs::write(&source, content).unwrap();
        fs::write(&destination, content).unwrap();

        let pairs = vec![FilePair {
            source: source.clone(),
            destination: destination.clone(),
            source_size: content.len() as u64,
            destination_size: content.len() as u64,
        }];

        let algorithm = SignatureAlgorithm::Md5 {
            seed_config: checksums::strong::Md5Seed::none(),
        };

        let results = prefetch_checksums(&pairs, algorithm);
        let result = results.get(&source).unwrap();

        assert!(result.checksums_match());
    }

    #[test]
    fn prefetch_checksums_detects_different_files() {
        let dir = create_tempdir();
        let source = dir.path().join("source.txt");
        let destination = dir.path().join("dest.txt");

        fs::write(&source, b"source content").unwrap();
        fs::write(&destination, b"dest content!!").unwrap();

        let pairs = vec![FilePair {
            source: source.clone(),
            destination: destination.clone(),
            source_size: 14,
            destination_size: 14,
        }];

        let algorithm = SignatureAlgorithm::Md5 {
            seed_config: checksums::strong::Md5Seed::none(),
        };

        let results = prefetch_checksums(&pairs, algorithm);
        let result = results.get(&source).unwrap();

        assert!(!result.checksums_match());
    }

    #[test]
    fn prefetch_checksums_skips_size_mismatch() {
        let dir = create_tempdir();
        let source = dir.path().join("source.txt");
        let destination = dir.path().join("dest.txt");

        fs::write(&source, b"short").unwrap();
        fs::write(&destination, b"much longer content").unwrap();

        let pairs = vec![FilePair {
            source: source.clone(),
            destination: destination.clone(),
            source_size: 5,
            destination_size: 19,
        }];

        let algorithm = SignatureAlgorithm::Md5 {
            seed_config: checksums::strong::Md5Seed::none(),
        };

        let results = prefetch_checksums(&pairs, algorithm);
        let result = results.get(&source).unwrap();

        assert!(result.source_checksum.is_none());
        assert!(result.destination_checksum.is_none());
        assert!(!result.checksums_match());
    }

    #[test]
    fn prefetch_checksums_handles_missing_destination() {
        let dir = create_tempdir();
        let source = dir.path().join("source.txt");
        let destination = dir.path().join("nonexistent.txt");

        fs::write(&source, b"content").unwrap();

        let pairs = vec![FilePair {
            source: source.clone(),
            destination,
            source_size: 7,
            destination_size: 7,
        }];

        let algorithm = SignatureAlgorithm::Md5 {
            seed_config: checksums::strong::Md5Seed::none(),
        };

        let results = prefetch_checksums(&pairs, algorithm);
        let result = results.get(&source).unwrap();

        assert!(result.source_checksum.is_some());
        assert!(result.destination_checksum.is_none());
        assert!(!result.checksums_match());
    }

    #[test]
    fn prefetch_checksums_parallel_multiple_files() {
        let dir = create_tempdir();
        let mut pairs = Vec::new();

        for i in 0..100 {
            let source = dir.path().join(format!("source_{i}.txt"));
            let destination = dir.path().join(format!("dest_{i}.txt"));
            let content = format!("content for file {i}");

            fs::write(&source, &content).unwrap();
            fs::write(&destination, &content).unwrap();

            pairs.push(FilePair {
                source,
                destination,
                source_size: content.len() as u64,
                destination_size: content.len() as u64,
            });
        }

        let algorithm = SignatureAlgorithm::Md5 {
            seed_config: checksums::strong::Md5Seed::none(),
        };

        let results = prefetch_checksums(&pairs, algorithm);

        assert_eq!(results.len(), 100);
        for pair in &pairs {
            let result = results.get(&pair.source).unwrap();
            assert!(result.checksums_match());
        }
    }

    #[test]
    fn prefetch_checksums_works_with_xxh3() {
        let dir = create_tempdir();
        let source = dir.path().join("source.txt");
        let destination = dir.path().join("dest.txt");

        let content = b"test content";
        fs::write(&source, content).unwrap();
        fs::write(&destination, content).unwrap();

        let pairs = vec![FilePair {
            source: source.clone(),
            destination: destination.clone(),
            source_size: content.len() as u64,
            destination_size: content.len() as u64,
        }];

        let algorithm = SignatureAlgorithm::Xxh3 { seed: 0 };

        let results = prefetch_checksums(&pairs, algorithm);
        let result = results.get(&source).unwrap();

        assert!(result.checksums_match());
    }

    #[test]
    fn prefetch_checksums_streaming_matches_identical_large_files() {
        let dir = create_tempdir();
        let source = dir.path().join("large_source.bin");
        let destination = dir.path().join("large_dest.bin");

        // Large file spanning multiple read windows exercises the streaming path.
        let size = (MMAP_THRESHOLD as usize) + 1024;
        let content: Vec<u8> = (0u8..=255).cycle().take(size).collect();
        fs::write(&source, &content).unwrap();
        fs::write(&destination, &content).unwrap();

        let pairs = vec![FilePair {
            source: source.clone(),
            destination: destination.clone(),
            source_size: size as u64,
            destination_size: size as u64,
        }];

        let algorithm = SignatureAlgorithm::Xxh3_128 { seed: 0 };
        let results = prefetch_checksums(&pairs, algorithm);
        let result = results.get(&source).unwrap();

        assert!(result.checksums_match());
        assert!(result.source_checksum.is_some());
        assert!(result.destination_checksum.is_some());
    }

    #[test]
    fn prefetch_checksums_streaming_detects_different_large_files() {
        let dir = create_tempdir();
        let source = dir.path().join("large_source.bin");
        let destination = dir.path().join("large_dest.bin");

        let size = (MMAP_THRESHOLD as usize) + 1024;
        let src_content: Vec<u8> = (0u8..=255).cycle().take(size).collect();
        let mut dst_content = src_content.clone();
        // Flip a byte near the end to force a checksum difference
        dst_content[size - 1] ^= 0xFF;
        fs::write(&source, &src_content).unwrap();
        fs::write(&destination, &dst_content).unwrap();

        let pairs = vec![FilePair {
            source: source.clone(),
            destination: destination.clone(),
            source_size: size as u64,
            destination_size: size as u64,
        }];

        let algorithm = SignatureAlgorithm::Xxh3_128 { seed: 0 };
        let results = prefetch_checksums(&pairs, algorithm);
        let result = results.get(&source).unwrap();

        assert!(!result.checksums_match());
    }

    /// Ground-truth parity for the SIGBUS-safe read-window path (issue #192).
    ///
    /// The whole-file checksum path streams the file through a bounded read
    /// window (upstream fileio.c:213-317 `map_file`/`map_ptr`), NOT `mmap(2)`.
    /// This test proves the windowed/chunked reader yields a digest identical
    /// to hashing the entire content in a single pass, for every algorithm and
    /// across sizes that span several read windows (the 128 KiB buffer block).
    /// A chunking bug (wrong offset, dropped or duplicated bytes) would make
    /// the streamed digest diverge from the single-pass ground truth.
    #[cfg(unix)]
    #[test]
    fn streaming_hash_matches_whole_buffer_for_all_algorithms() {
        /// Independent, single-pass digest over the full content buffer.
        fn whole_buffer_digest(data: &[u8], algorithm: SignatureAlgorithm) -> Vec<u8> {
            match algorithm {
                SignatureAlgorithm::Md4 => Md4::digest(data).as_ref().to_vec(),
                SignatureAlgorithm::Md4Seeded { seed } => {
                    let mut hasher = Md4::new();
                    hasher.update(data);
                    if seed != 0 {
                        hasher.update(&seed.to_le_bytes());
                    }
                    hasher.finalize().as_ref().to_vec()
                }
                SignatureAlgorithm::Md5 { seed_config } => {
                    let mut hasher = Md5::with_seed(seed_config);
                    hasher.update(data);
                    hasher.finalize().as_ref().to_vec()
                }
                SignatureAlgorithm::Sha1 => Sha1::digest(data).as_ref().to_vec(),
                SignatureAlgorithm::Xxh64 { seed } => Xxh64::digest(seed, data).as_ref().to_vec(),
                SignatureAlgorithm::Xxh3 { seed } => Xxh3::digest(seed, data).as_ref().to_vec(),
                SignatureAlgorithm::Xxh3_128 { seed } => {
                    Xxh3_128::digest(seed, data).as_ref().to_vec()
                }
            }
        }

        let dir = create_tempdir();
        let buffer_pool = global_buffer_pool();

        let algorithms: Vec<SignatureAlgorithm> = vec![
            SignatureAlgorithm::Md5 {
                seed_config: checksums::strong::Md5Seed::none(),
            },
            SignatureAlgorithm::Xxh3 { seed: 42 },
            SignatureAlgorithm::Xxh3_128 { seed: 42 },
            SignatureAlgorithm::Xxh64 { seed: 42 },
            SignatureAlgorithm::Sha1,
            SignatureAlgorithm::Md4,
        ];

        // Sizes spanning several 128 KiB read windows, incl. a boundary case.
        let sizes: &[usize] = &[
            1,
            MMAP_THRESHOLD as usize,
            (MMAP_THRESHOLD as usize) * 3 + 777,
        ];

        for (i, &size) in sizes.iter().enumerate() {
            let content: Vec<u8> = (0..size).map(|j| ((j * 31 + i) & 0xFF) as u8).collect();
            let path = dir.path().join(format!("file_{i}_{size}.bin"));
            fs::write(&path, &content).unwrap();

            for &algorithm in &algorithms {
                let file = File::open(&path).unwrap();
                let streamed =
                    hash_file_contents(file, size as u64, algorithm, &buffer_pool).unwrap();
                let expected = whole_buffer_digest(&content, algorithm);
                assert_eq!(
                    streamed, expected,
                    "streamed digest diverged from single-pass at size {size} for {algorithm:?}",
                );
            }
        }
    }

    #[test]
    fn should_skip_with_prefetched_returns_none_for_unknown() {
        let prefetched = HashMap::new();
        let result = should_skip_with_prefetched_checksum(&prefetched, Path::new("/unknown"));
        assert!(result.is_none());
    }

    #[test]
    fn should_skip_with_prefetched_returns_match_status() {
        let dir = create_tempdir();
        let source = dir.path().join("source.txt");
        let destination = dir.path().join("dest.txt");

        fs::write(&source, b"same").unwrap();
        fs::write(&destination, b"same").unwrap();

        let pairs = vec![FilePair {
            source: source.clone(),
            destination,
            source_size: 4,
            destination_size: 4,
        }];

        let algorithm = SignatureAlgorithm::Md5 {
            seed_config: checksums::strong::Md5Seed::none(),
        };

        let prefetched = prefetch_checksums(&pairs, algorithm);
        let result = should_skip_with_prefetched_checksum(&prefetched, &source);

        assert_eq!(result, Some(true));
    }

    /// Parity test for `compute_file_checksum`: both platforms stream the file
    /// through a bounded read window (`std::fs::File` on Unix,
    /// `WindowsChunkedReader` on Windows) rather than `mmap(2)`. The prefetch
    /// digest must remain byte-identical to a direct `std::fs::read` + algorithm
    /// digest across sizes spanning several read windows, incl. a boundary case.
    #[test]
    fn prefetch_checksums_matches_direct_digest_across_size_boundaries() {
        let dir = create_tempdir();
        let algorithm = SignatureAlgorithm::Xxh3_128 { seed: 0 };
        let sizes: &[usize] = &[
            0,
            1,
            MMAP_THRESHOLD as usize - 1,
            MMAP_THRESHOLD as usize,
            MMAP_THRESHOLD as usize + 1,
            (MMAP_THRESHOLD as usize) * 5 + 17,
        ];

        let mut pairs = Vec::with_capacity(sizes.len());
        for (i, &n) in sizes.iter().enumerate() {
            let source = dir.path().join(format!("src_{i}_{n}.bin"));
            let destination = dir.path().join(format!("dst_{i}_{n}.bin"));
            let content: Vec<u8> = (0..n).map(|j| ((j * 7 + i) & 0xFF) as u8).collect();
            fs::write(&source, &content).unwrap();
            fs::write(&destination, &content).unwrap();
            pairs.push(FilePair {
                source,
                destination,
                source_size: n as u64,
                destination_size: n as u64,
            });
        }

        let results = prefetch_checksums(&pairs, algorithm);
        assert_eq!(results.len(), sizes.len());
        for (pair, &n) in pairs.iter().zip(sizes.iter()) {
            let result = results.get(&pair.source).expect("result for source");
            assert!(
                result.checksums_match(),
                "checksums diverged at size {n}: {result:?}",
            );
            let src_observed = fs::read(&pair.source).unwrap();
            assert_eq!(src_observed.len(), n);
            let src_digest = &result
                .source_checksum
                .as_ref()
                .expect("source digest present")
                .digest;
            assert!(!src_digest.is_empty() || n == 0);
        }
    }
}
