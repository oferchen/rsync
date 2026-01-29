//! Async signature pre-computation for pipelined transfers.
//!
//! This module integrates async signature generation with the pipeline,
//! pre-computing signatures for upcoming files while waiting for current
//! transfer responses.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::num::NonZeroU8;
use std::path::PathBuf;

use protocol::ProtocolVersion;

use engine::signature::{FileSignature, SignatureAlgorithm};
use signature::async_gen::{AsyncSignatureConfig, AsyncSignatureGenerator, SignatureRequest};

/// Cache for pre-computed signatures.
///
/// Stores signatures that have been computed asynchronously,
/// indexed by file path for fast lookup.
#[derive(Debug)]
pub struct SignatureCache {
    /// Generator for async signature computation.
    generator: Option<AsyncSignatureGenerator>,
    /// Pending request tracking: request_id -> file_path.
    pending_requests: HashMap<u64, PathBuf>,
    /// Completed signatures: file_path -> signature.
    completed: HashMap<PathBuf, FileSignature>,
    /// Failed requests: file_path -> error message.
    failed: HashMap<PathBuf, String>,
}

impl SignatureCache {
    /// Creates a new signature cache with async generation enabled.
    #[must_use]
    pub fn new(config: AsyncSignatureConfig) -> Self {
        Self {
            generator: Some(AsyncSignatureGenerator::new(config)),
            pending_requests: HashMap::new(),
            completed: HashMap::new(),
            failed: HashMap::new(),
        }
    }

    /// Creates a disabled cache (no async generation).
    #[must_use]
    pub fn disabled() -> Self {
        Self {
            generator: None,
            pending_requests: HashMap::new(),
            completed: HashMap::new(),
            failed: HashMap::new(),
        }
    }

    /// Returns true if async generation is enabled.
    #[must_use]
    pub fn is_enabled(&self) -> bool {
        self.generator.is_some()
    }

    /// Requests signature generation for a file.
    ///
    /// The signature will be computed in the background.
    ///
    /// # Errors
    ///
    /// Returns an error if the worker threads have failed.
    pub fn request_signature(
        &mut self,
        file_path: PathBuf,
        basis_size: u64,
        protocol: ProtocolVersion,
        checksum_length: NonZeroU8,
        checksum_algorithm: SignatureAlgorithm,
    ) -> io::Result<()> {
        let Some(ref mut generator) = self.generator else {
            return Ok(());
        };

        let request_id = generator.next_request_id();

        let request = SignatureRequest {
            request_id,
            basis_path: file_path.clone(),
            basis_size,
            protocol,
            checksum_length,
            checksum_algorithm,
        };

        generator.request_signature(request)?;
        self.pending_requests.insert(request_id, file_path);

        Ok(())
    }

    /// Polls for completed signatures without blocking.
    ///
    /// Call this periodically to collect completed async work.
    pub fn poll_results(&mut self) {
        let Some(ref generator) = self.generator else {
            return;
        };

        while let Some(result) = generator.try_get_result() {
            if let Some(file_path) = self.pending_requests.remove(&result.request_id) {
                if let Some(signature) = result.signature {
                    self.completed.insert(file_path, signature);
                } else if let Some(error) = result.error {
                    self.failed.insert(file_path, error);
                }
            }
        }
    }

    /// Tries to get a pre-computed signature for a file.
    ///
    /// Returns `Some(signature)` if available, `None` otherwise.
    #[must_use]
    pub fn get_signature(&mut self, file_path: &PathBuf) -> Option<FileSignature> {
        self.completed.remove(file_path)
    }

    /// Checks if a signature generation failed.
    ///
    /// Returns the error message if the generation failed.
    #[must_use]
    pub fn get_error(&mut self, file_path: &PathBuf) -> Option<String> {
        self.failed.remove(file_path)
    }

    /// Returns statistics about cache usage.
    #[must_use]
    pub fn stats(&self) -> CacheStats {
        CacheStats {
            pending: self.pending_requests.len(),
            completed: self.completed.len(),
            failed: self.failed.len(),
        }
    }

    /// Shuts down the async generator.
    ///
    /// # Errors
    ///
    /// Returns an error if worker threads panicked.
    pub fn shutdown(mut self) -> io::Result<()> {
        if let Some(generator) = self.generator.take() {
            generator.shutdown()?;
        }
        Ok(())
    }
}

impl Drop for SignatureCache {
    fn drop(&mut self) {
        // Generator's Drop impl will shut down workers
    }
}

/// Statistics about signature cache usage.
#[derive(Debug, Clone, Copy, Default)]
pub struct CacheStats {
    /// Number of pending signature requests.
    pub pending: usize,
    /// Number of completed signatures available.
    pub completed: usize,
    /// Number of failed signature generations.
    pub failed: usize,
}

impl CacheStats {
    /// Returns the total number of in-flight operations.
    #[must_use]
    pub const fn total_in_flight(&self) -> usize {
        self.pending + self.completed + self.failed
    }
}

/// Tries to open a file and get its size for signature generation.
///
/// Returns `(file, size)` if successful, `None` otherwise.
pub fn try_open_file_for_signature(path: &PathBuf) -> Option<(fs::File, u64)> {
    let file = fs::File::open(path).ok()?;
    let metadata = file.metadata().ok()?;
    let size = metadata.len();
    Some((file, size))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn create_test_file(size: usize) -> io::Result<NamedTempFile> {
        let mut file = NamedTempFile::new()?;
        let data = vec![0xAB; size];
        file.write_all(&data)?;
        file.flush()?;
        Ok(file)
    }

    #[test]
    fn test_signature_cache_basic() {
        let test_file = create_test_file(1024).unwrap();
        let path = test_file.path().to_path_buf();

        let config = AsyncSignatureConfig::default().with_threads(1);
        let mut cache = SignatureCache::new(config);

        assert!(cache.is_enabled());

        // Request signature
        cache
            .request_signature(
                path.clone(),
                1024,
                ProtocolVersion::NEWEST,
                NonZeroU8::new(16).unwrap(),
                SignatureAlgorithm::Md4,
            )
            .unwrap();

        // Poll until completed
        for _ in 0..100 {
            cache.poll_results();
            if cache.get_signature(&path).is_some() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        panic!("Signature generation timed out");
    }

    #[test]
    fn test_signature_cache_disabled() {
        let cache = SignatureCache::disabled();
        assert!(!cache.is_enabled());

        let stats = cache.stats();
        assert_eq!(stats.pending, 0);
        assert_eq!(stats.completed, 0);
    }

    #[test]
    fn test_signature_cache_nonexistent_file() {
        let config = AsyncSignatureConfig::default().with_threads(1);
        let mut cache = SignatureCache::new(config);

        let path = PathBuf::from("/nonexistent/file");

        cache
            .request_signature(
                path.clone(),
                1024,
                ProtocolVersion::NEWEST,
                NonZeroU8::new(16).unwrap(),
                SignatureAlgorithm::Md4,
            )
            .unwrap();

        // Poll until error appears
        for _ in 0..100 {
            cache.poll_results();
            if let Some(error) = cache.get_error(&path) {
                assert!(error.contains("signature generation failed"));
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        panic!("Error detection timed out");
    }

    #[test]
    fn test_cache_stats() {
        let config = AsyncSignatureConfig::default().with_threads(1);
        let cache = SignatureCache::new(config);

        let stats = cache.stats();
        assert_eq!(stats.total_in_flight(), 0);
    }

    #[test]
    fn test_try_open_file_for_signature() {
        let test_file = create_test_file(512).unwrap();
        let path = test_file.path().to_path_buf();

        let result = try_open_file_for_signature(&path);
        assert!(result.is_some());

        let (_file, size) = result.unwrap();
        assert_eq!(size, 512);
    }

    #[test]
    fn test_try_open_file_nonexistent() {
        let path = PathBuf::from("/nonexistent/file");
        let result = try_open_file_for_signature(&path);
        assert!(result.is_none());
    }
}
