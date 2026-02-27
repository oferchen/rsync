//! Async checksum computation using `spawn_blocking` for CPU-intensive work.

use std::path::Path;

use tokio::task;

use super::error::{AsyncIoError, IoResultExt};

/// Computes a checksum of a file asynchronously.
///
/// Uses `spawn_blocking` to run the CPU-intensive checksum computation
/// on a dedicated thread pool.
///
/// # Errors
///
/// Returns an error if the file cannot be read.
pub async fn compute_file_checksum(
    path: impl AsRef<Path>,
    algorithm: ChecksumAlgorithm,
) -> Result<Vec<u8>, AsyncIoError> {
    let path = path.as_ref().to_path_buf();

    task::spawn_blocking(move || {
        use std::io::Read;

        let mut file = std::fs::File::open(&path).with_path(&path)?;

        let mut buffer = vec![0u8; 64 * 1024];
        let mut hasher = algorithm.new_hasher();

        loop {
            let n = file.read(&mut buffer).with_path(&path)?;
            if n == 0 {
                break;
            }
            hasher.update(&buffer[..n]);
        }

        Ok(hasher.finalize())
    })
    .await?
}

/// Checksum algorithms supported for async computation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChecksumAlgorithm {
    /// MD5 checksum (128-bit).
    Md5,
    /// XXHash64 (64-bit).
    Xxh64,
}

impl ChecksumAlgorithm {
    fn new_hasher(self) -> Box<dyn Hasher> {
        match self {
            Self::Md5 => Box::new(Md5Hasher::new()),
            Self::Xxh64 => Box::new(Xxh64Hasher::new()),
        }
    }
}

trait Hasher: Send {
    fn update(&mut self, data: &[u8]);
    fn finalize(self: Box<Self>) -> Vec<u8>;
}

struct Md5Hasher {
    context: md5::Context,
}

impl Md5Hasher {
    fn new() -> Self {
        Self {
            context: md5::Context::new(),
        }
    }
}

impl Hasher for Md5Hasher {
    fn update(&mut self, data: &[u8]) {
        self.context.consume(data);
    }

    fn finalize(self: Box<Self>) -> Vec<u8> {
        self.context.compute().to_vec()
    }
}

struct Xxh64Hasher {
    hasher: xxhash_rust::xxh64::Xxh64,
}

impl Xxh64Hasher {
    fn new() -> Self {
        Self {
            hasher: xxhash_rust::xxh64::Xxh64::new(0),
        }
    }
}

impl Hasher for Xxh64Hasher {
    fn update(&mut self, data: &[u8]) {
        self.hasher.update(data);
    }

    fn finalize(self: Box<Self>) -> Vec<u8> {
        self.hasher.digest().to_le_bytes().to_vec()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_checksum_algorithm_eq() {
        assert_eq!(ChecksumAlgorithm::Md5, ChecksumAlgorithm::Md5);
        assert_eq!(ChecksumAlgorithm::Xxh64, ChecksumAlgorithm::Xxh64);
        assert_ne!(ChecksumAlgorithm::Md5, ChecksumAlgorithm::Xxh64);
    }

    #[test]
    fn test_checksum_algorithm_clone() {
        let algo = ChecksumAlgorithm::Md5;
        let cloned = algo;
        assert_eq!(algo, cloned);
    }

    #[test]
    fn test_checksum_algorithm_debug() {
        let algo = ChecksumAlgorithm::Md5;
        let debug = format!("{algo:?}");
        assert!(debug.contains("Md5"));
    }
}
