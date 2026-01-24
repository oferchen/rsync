//! Rayon integration for parallel MD5 hashing.
//!
//! This module provides parallel iteration support and file hashing utilities.

use rayon::prelude::*;
use std::fs;
use std::io;
use std::path::Path;

use crate::{digest, digest_batch, Digest};

/// Extension trait for parallel MD5 hashing.
///
/// Provides a method to compute MD5 digests from a parallel iterator,
/// using SIMD batching when beneficial.
///
/// # Example
///
/// ```
/// use rayon::prelude::*;
/// use md5_simd::ParallelMd5;
///
/// let data: Vec<Vec<u8>> = vec![
///     b"hello".to_vec(),
///     b"world".to_vec(),
///     b"test".to_vec(),
/// ];
///
/// let digests = data.par_iter().md5_digest();
/// assert_eq!(digests.len(), 3);
/// ```
pub trait ParallelMd5<T> {
    /// Compute MD5 digests in parallel using SIMD when beneficial.
    fn md5_digest(self) -> Vec<Digest>;
}

impl<I, T> ParallelMd5<T> for I
where
    I: ParallelIterator<Item = T>,
    T: AsRef<[u8]> + Send,
{
    fn md5_digest(self) -> Vec<Digest> {
        // Collect and batch for SIMD
        let items: Vec<T> = self.collect();
        digest_batch(&items)
    }
}

/// Compute MD5 digests for multiple files in parallel.
///
/// Reads each file and computes its MD5 digest. Files are read and hashed
/// in parallel using rayon's thread pool.
///
/// # Example
///
/// ```no_run
/// use md5_simd::digest_files;
///
/// let paths = ["file1.txt", "file2.txt", "file3.txt"];
/// let results = digest_files(&paths);
///
/// for (path, result) in paths.iter().zip(results.iter()) {
///     match result {
///         Ok(digest) => println!("{}: {:02x?}", path, digest),
///         Err(e) => println!("{}: error - {}", path, e),
///     }
/// }
/// ```
pub fn digest_files<P: AsRef<Path> + Sync>(paths: &[P]) -> Vec<io::Result<Digest>> {
    paths
        .par_iter()
        .map(|path| {
            let data = fs::read(path.as_ref())?;
            Ok(digest(&data))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::tempdir;

    #[test]
    fn parallel_md5_matches_sequential() {
        let data: Vec<Vec<u8>> = vec![
            b"hello".to_vec(),
            b"world".to_vec(),
            b"test".to_vec(),
            b"data".to_vec(),
            b"more".to_vec(),
            b"inputs".to_vec(),
            b"for".to_vec(),
            b"testing".to_vec(),
        ];

        let parallel: Vec<Digest> = data.par_iter().md5_digest();
        let sequential: Vec<Digest> = data.iter().map(|d| digest(d)).collect();

        assert_eq!(parallel, sequential);
    }

    #[test]
    fn digest_files_works() {
        let dir = tempdir().unwrap();

        // Create test files
        let mut paths = Vec::new();
        for i in 0..4 {
            let path = dir.path().join(format!("file{i}.txt"));
            let mut file = std::fs::File::create(&path).unwrap();
            writeln!(file, "content of file {i}").unwrap();
            paths.push(path);
        }

        let results = digest_files(&paths);

        // All should succeed
        for result in &results {
            assert!(result.is_ok());
        }

        // All digests should be unique
        let digests: Vec<_> = results.iter().map(|r| r.as_ref().unwrap()).collect();
        for (i, d1) in digests.iter().enumerate() {
            for (j, d2) in digests.iter().enumerate() {
                if i != j {
                    assert_ne!(d1, d2);
                }
            }
        }
    }

    #[test]
    fn digest_files_handles_missing() {
        let results = digest_files(&["nonexistent_file_12345.txt"]);
        assert_eq!(results.len(), 1);
        assert!(results[0].is_err());
    }
}
