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
use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use rayon::prelude::*;

use checksums::strong::{Md4, Md5, Sha1, StrongDigest, Xxh3, Xxh3_128, Xxh64};

use crate::local_copy::COPY_BUFFER_SIZE;
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
    /// Source file path.
    pub(crate) source: PathBuf,
    /// Destination file path.
    pub(crate) destination: PathBuf,
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
    pairs
        .par_iter()
        .map(|pair| {
            // Skip if sizes don't match (no need to hash)
            if pair.source_size != pair.destination_size {
                return (
                    pair.source.clone(),
                    ChecksumPrefetchResult {
                        source: pair.source.clone(),
                        destination: pair.destination.clone(),
                        source_checksum: None,
                        destination_checksum: None,
                    },
                );
            }

            // Compute checksums in parallel for source and destination
            let (source_checksum, destination_checksum) = rayon::join(
                || compute_file_checksum(&pair.source, algorithm),
                || compute_file_checksum(&pair.destination, algorithm),
            );

            (
                pair.source.clone(),
                ChecksumPrefetchResult {
                    source: pair.source.clone(),
                    destination: pair.destination.clone(),
                    source_checksum,
                    destination_checksum,
                },
            )
        })
        .collect()
}

/// Computes the checksum of a single file.
fn compute_file_checksum(path: &Path, algorithm: SignatureAlgorithm) -> Option<FileChecksum> {
    let file = File::open(path).ok()?;
    let metadata = file.metadata().ok()?;
    let size = metadata.len();

    let digest = hash_file_contents(file, algorithm).ok()?;

    Some(FileChecksum { digest, size })
}

/// Hashes file contents using the specified algorithm.
fn hash_file_contents(mut file: File, algorithm: SignatureAlgorithm) -> io::Result<Vec<u8>> {
    let mut buffer = vec![0u8; COPY_BUFFER_SIZE];

    let digest = match algorithm {
        SignatureAlgorithm::Md4 => {
            let mut hasher = Md4::new();
            loop {
                let n = file.read(&mut buffer)?;
                if n == 0 {
                    break;
                }
                hasher.update(&buffer[..n]);
            }
            hasher.finalize().as_ref().to_vec()
        }
        SignatureAlgorithm::Md5 { seed_config } => {
            let mut hasher = Md5::with_seed(seed_config);
            loop {
                let n = file.read(&mut buffer)?;
                if n == 0 {
                    break;
                }
                hasher.update(&buffer[..n]);
            }
            hasher.finalize().as_ref().to_vec()
        }
        SignatureAlgorithm::Sha1 => {
            let mut hasher = Sha1::new();
            loop {
                let n = file.read(&mut buffer)?;
                if n == 0 {
                    break;
                }
                hasher.update(&buffer[..n]);
            }
            hasher.finalize().as_ref().to_vec()
        }
        SignatureAlgorithm::Xxh64 { seed } => {
            let mut hasher = Xxh64::new(seed);
            loop {
                let n = file.read(&mut buffer)?;
                if n == 0 {
                    break;
                }
                hasher.update(&buffer[..n]);
            }
            hasher.finalize().as_ref().to_vec()
        }
        SignatureAlgorithm::Xxh3 { seed } => {
            let mut hasher = Xxh3::new(seed);
            loop {
                let n = file.read(&mut buffer)?;
                if n == 0 {
                    break;
                }
                hasher.update(&buffer[..n]);
            }
            hasher.finalize().as_ref().to_vec()
        }
        SignatureAlgorithm::Xxh3_128 { seed } => {
            let mut hasher = Xxh3_128::new(seed);
            loop {
                let n = file.read(&mut buffer)?;
                if n == 0 {
                    break;
                }
                hasher.update(&buffer[..n]);
            }
            hasher.finalize().as_ref().to_vec()
        }
    };

    Ok(digest)
}

/// Checks if a file pair should be skipped based on prefetched checksums.
///
/// This is a fast lookup that uses previously computed checksums.
pub(crate) fn should_skip_with_prefetched_checksum(
    prefetched: &HashMap<PathBuf, ChecksumPrefetchResult>,
    source: &Path,
) -> Option<bool> {
    prefetched.get(source).map(|result| result.checksums_match())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::tempdir;

    #[test]
    fn prefetch_checksums_matches_identical_files() {
        let dir = tempdir().unwrap();
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
        let dir = tempdir().unwrap();
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
        let dir = tempdir().unwrap();
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

        // Size mismatch means no checksums computed
        assert!(result.source_checksum.is_none());
        assert!(result.destination_checksum.is_none());
        assert!(!result.checksums_match());
    }

    #[test]
    fn prefetch_checksums_handles_missing_destination() {
        let dir = tempdir().unwrap();
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
        let dir = tempdir().unwrap();
        let mut pairs = Vec::new();

        // Create 100 file pairs
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
        let dir = tempdir().unwrap();
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
    fn should_skip_with_prefetched_returns_none_for_unknown() {
        let prefetched = HashMap::new();
        let result = should_skip_with_prefetched_checksum(&prefetched, Path::new("/unknown"));
        assert!(result.is_none());
    }

    #[test]
    fn should_skip_with_prefetched_returns_match_status() {
        let dir = tempdir().unwrap();
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
}
