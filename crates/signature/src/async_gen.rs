//! Asynchronous signature generation for pipeline optimization.
//!
//! This module provides async signature pre-computation to overlap CPU-intensive
//! checksum calculation with network I/O during pipelined transfers.
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────────────────┐
//! │                    Async Signature Generation                           │
//! ├─────────────────────────────────────────────────────────────────────────┤
//! │                                                                         │
//! │  Main Thread                         Worker Thread Pool                 │
//! │  ┌─────────────────────┐             ┌─────────────────────┐           │
//! │  │ Process response N  │             │ Generate signature  │           │
//! │  │ from network        │             │ for file N+1        │           │
//! │  │                     │             │                     │           │
//! │  │ - Read delta        │  ────────▶  │ - Open basis file   │           │
//! │  │ - Apply to disk     │   Request   │ - Compute checksums │           │
//! │  │ - Set metadata      │   Channel   │ - Return signature  │           │
//! │  └─────────────────────┘             └─────────────────────┘           │
//! │           │                                    │                        │
//! │           │                                    │ Result                 │
//! │           │                                    ▼                        │
//! │           │                           ┌─────────────────────┐           │
//! │           └──────────────────────────▶│ Use pre-computed    │           │
//! │                   When ready          │ signature           │           │
//! │                                       └─────────────────────┘           │
//! │                                                                         │
//! └─────────────────────────────────────────────────────────────────────────┘
//! ```
//!
//! # Performance Benefits
//!
//! Without async signatures:
//! - File N: [Wait for response][Generate sig N+1][Send request N+1]
//! - File N+1: [Wait for response][Generate sig N+2][Send request N+2]
//!
//! With async signatures:
//! - File N: [Wait for response] (sig N+1 computed during wait)
//! - File N+1: [Wait for response] (sig N+2 computed during wait)
//!
//! For transfers with many files and CPU-intensive checksums (MD4/MD5/SHA1),
//! this can reduce total transfer time by overlapping computation with I/O.

use std::fs;
use std::io;
use std::num::NonZeroU8;
use std::path::PathBuf;
use std::sync::{mpsc::{self, Receiver, Sender}, Arc};
use std::thread::{self, JoinHandle};

use protocol::ProtocolVersion;

use crate::algorithm::SignatureAlgorithm;
use crate::file::FileSignature;
use crate::generation::generate_file_signature;
use crate::layout::{SignatureLayoutParams, calculate_signature_layout};

/// Request to generate a signature asynchronously.
#[derive(Debug)]
pub struct SignatureRequest {
    /// Unique ID for this request (for matching with results).
    pub request_id: u64,
    /// Path to the basis file.
    pub basis_path: PathBuf,
    /// Size of the basis file.
    pub basis_size: u64,
    /// Protocol version for signature layout.
    pub protocol: ProtocolVersion,
    /// Checksum length for signature.
    pub checksum_length: NonZeroU8,
    /// Checksum algorithm for signature.
    pub checksum_algorithm: SignatureAlgorithm,
}

/// Result of an asynchronous signature generation.
#[derive(Debug)]
pub struct SignatureResult {
    /// Request ID that this result corresponds to.
    pub request_id: u64,
    /// Path to the basis file.
    pub basis_path: PathBuf,
    /// Generated signature, or None if generation failed.
    pub signature: Option<FileSignature>,
    /// Error message if generation failed.
    pub error: Option<String>,
}

/// Message sent to worker threads.
enum WorkerMessage {
    /// Generate a signature.
    GenerateSignature(SignatureRequest),
    /// Shutdown signal.
    Shutdown,
}

/// Configuration for async signature generation.
#[derive(Debug, Clone)]
pub struct AsyncSignatureConfig {
    /// Number of worker threads to spawn.
    ///
    /// More threads allow more signatures to be computed in parallel,
    /// but increase memory and CPU usage.
    /// Default: Number of CPU cores, capped at 4.
    pub num_threads: usize,

    /// Maximum number of pending requests before blocking.
    ///
    /// This bounds memory usage and prevents overwhelming workers.
    /// Default: 16
    pub max_pending: usize,
}

impl Default for AsyncSignatureConfig {
    fn default() -> Self {
        let num_cpus = thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(2);

        Self {
            // Cap at 4 threads - signature generation has diminishing returns
            // beyond that due to disk I/O bottleneck
            num_threads: num_cpus.min(4),
            max_pending: 16,
        }
    }
}

impl AsyncSignatureConfig {
    /// Creates a new configuration with the specified number of threads.
    #[must_use]
    pub fn with_threads(mut self, threads: usize) -> Self {
        self.num_threads = threads.max(1);
        self
    }

    /// Sets the maximum number of pending requests.
    #[must_use]
    pub fn with_max_pending(mut self, max: usize) -> Self {
        self.max_pending = max.max(1);
        self
    }
}

/// Async signature generator using a thread pool.
///
/// Spawns worker threads that generate signatures in the background,
/// allowing the main transfer thread to overlap signature computation
/// with network I/O.
///
/// # Example
///
/// ```ignore
/// use signature::async_gen::{AsyncSignatureGenerator, SignatureRequest, AsyncSignatureConfig};
/// use signature::SignatureAlgorithm;
/// use protocol::ProtocolVersion;
/// use std::num::NonZeroU8;
/// use std::path::PathBuf;
///
/// let config = AsyncSignatureConfig::default();
/// let mut generator = AsyncSignatureGenerator::new(config);
///
/// // Queue signature generation for upcoming file
/// let request = SignatureRequest {
///     request_id: 1,
///     basis_path: PathBuf::from("/path/to/basis"),
///     basis_size: 1024,
///     protocol: ProtocolVersion::NEWEST,
///     checksum_length: NonZeroU8::new(16).unwrap(),
///     checksum_algorithm: SignatureAlgorithm::Md4,
/// };
/// generator.request_signature(request)?;
///
/// // Later, when needed...
/// if let Some(result) = generator.try_get_result() {
///     if let Some(signature) = result.signature {
///         // Use pre-computed signature
///     }
/// }
///
/// // Shutdown when done
/// generator.shutdown()?;
/// # Ok::<(), std::io::Error>(())
/// ```
pub struct AsyncSignatureGenerator {
    /// Sender for work requests.
    request_sender: Sender<WorkerMessage>,
    /// Receiver for results.
    result_receiver: Receiver<SignatureResult>,
    /// Worker thread handles.
    workers: Vec<JoinHandle<()>>,
    /// Configuration.
    config: AsyncSignatureConfig,
    /// Next request ID.
    next_request_id: u64,
}

// Manual Debug impl because JoinHandle doesn't implement Debug
impl std::fmt::Debug for AsyncSignatureGenerator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AsyncSignatureGenerator")
            .field("num_workers", &self.workers.len())
            .field("config", &self.config)
            .field("next_request_id", &self.next_request_id)
            .finish()
    }
}

impl AsyncSignatureGenerator {
    /// Creates a new async signature generator.
    ///
    /// Spawns worker threads according to the configuration.
    #[must_use]
    pub fn new(config: AsyncSignatureConfig) -> Self {
        let (request_sender, request_receiver) = mpsc::channel();
        let (result_sender, result_receiver) = mpsc::channel();

        let request_receiver = Arc::new(std::sync::Mutex::new(request_receiver));
        let mut workers = Vec::with_capacity(config.num_threads);

        for _ in 0..config.num_threads {
            let receiver = Arc::clone(&request_receiver);
            let sender = result_sender.clone();

            let handle = thread::spawn(move || {
                worker_thread_main_shared(receiver, sender);
            });

            workers.push(handle);
        }

        // Drop the original sender so workers can detect when all work is done
        drop(result_sender);

        Self {
            request_sender,
            result_receiver,
            workers,
            config,
            next_request_id: 0,
        }
    }

    /// Requests signature generation for a file.
    ///
    /// Returns the request ID that can be used to match the result.
    ///
    /// This may block if the request queue is full (backpressure).
    ///
    /// # Errors
    ///
    /// Returns an error if all worker threads have terminated.
    pub fn request_signature(&mut self, request: SignatureRequest) -> io::Result<u64> {
        let request_id = request.request_id;

        self.request_sender
            .send(WorkerMessage::GenerateSignature(request))
            .map_err(|_| {
                io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "signature worker threads have terminated",
                )
            })?;

        Ok(request_id)
    }

    /// Tries to get a completed signature result without blocking.
    ///
    /// Returns `None` if no results are available.
    #[must_use]
    pub fn try_get_result(&self) -> Option<SignatureResult> {
        self.result_receiver.try_recv().ok()
    }

    /// Blocks until a signature result is available.
    ///
    /// # Errors
    ///
    /// Returns an error if all worker threads have terminated.
    pub fn wait_for_result(&self) -> io::Result<SignatureResult> {
        self.result_receiver.recv().map_err(|_| {
            io::Error::new(
                io::ErrorKind::BrokenPipe,
                "signature worker threads have terminated",
            )
        })
    }

    /// Returns the number of worker threads.
    #[must_use]
    pub fn num_threads(&self) -> usize {
        self.config.num_threads
    }

    /// Returns the next request ID and increments the counter.
    pub fn next_request_id(&mut self) -> u64 {
        let id = self.next_request_id;
        self.next_request_id = id.wrapping_add(1);
        id
    }

    /// Shuts down the generator and waits for all workers to finish.
    ///
    /// # Errors
    ///
    /// Returns an error if any worker thread panicked.
    pub fn shutdown(mut self) -> io::Result<()> {
        // Send shutdown to all workers
        for _ in 0..self.workers.len() {
            let _ = self.request_sender.send(WorkerMessage::Shutdown);
        }

        // Wait for all workers to finish
        let workers = std::mem::take(&mut self.workers);
        for handle in workers {
            handle.join().map_err(|_| {
                io::Error::new(
                    io::ErrorKind::Other,
                    "signature worker thread panicked",
                )
            })?;
        }

        Ok(())
    }
}

impl Drop for AsyncSignatureGenerator {
    fn drop(&mut self) {
        // Send shutdown signal to workers
        for _ in 0..self.workers.len() {
            let _ = self.request_sender.send(WorkerMessage::Shutdown);
        }
    }
}

/// Main loop for a signature worker thread with shared receiver.
///
/// Processes signature generation requests until shutdown.
fn worker_thread_main_shared(
    request_receiver: Arc<std::sync::Mutex<Receiver<WorkerMessage>>>,
    result_sender: Sender<SignatureResult>,
) {
    loop {
        let msg = {
            let receiver = request_receiver.lock().unwrap();
            receiver.recv()
        };

        match msg {
            Ok(WorkerMessage::GenerateSignature(request)) => {
                let result = generate_signature_sync(request);

                // If sending fails, main thread has dropped receiver - just exit
                if result_sender.send(result).is_err() {
                    break;
                }
            }
            Ok(WorkerMessage::Shutdown) | Err(_) => {
                // Shutdown requested or channel closed
                break;
            }
        }
    }
}

/// Synchronously generates a signature for a request.
///
/// This is called by worker threads.
fn generate_signature_sync(request: SignatureRequest) -> SignatureResult {
    let result = (|| -> Result<FileSignature, Box<dyn std::error::Error>> {
        // Open basis file
        let basis_file = fs::File::open(&request.basis_path)?;

        // Calculate signature layout
        let params = SignatureLayoutParams::new(
            request.basis_size,
            None,
            request.protocol,
            request.checksum_length,
        );
        let layout = calculate_signature_layout(params)?;

        // Generate signature
        Ok(generate_file_signature(basis_file, layout, request.checksum_algorithm)?)
    })();

    match result {
        Ok(signature) => SignatureResult {
            request_id: request.request_id,
            basis_path: request.basis_path,
            signature: Some(signature),
            error: None,
        },
        Err(e) => SignatureResult {
            request_id: request.request_id,
            basis_path: request.basis_path,
            signature: None,
            error: Some(format!("signature generation failed: {e}")),
        },
    }
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
    fn test_async_signature_basic() {
        let test_file = create_test_file(1024).unwrap();
        let path = test_file.path().to_path_buf();

        let config = AsyncSignatureConfig::default().with_threads(1);
        let mut generator = AsyncSignatureGenerator::new(config);

        let request = SignatureRequest {
            request_id: 1,
            basis_path: path.clone(),
            basis_size: 1024,
            protocol: ProtocolVersion::NEWEST,
            checksum_length: NonZeroU8::new(16).unwrap(),
            checksum_algorithm: SignatureAlgorithm::Md4,
        };

        generator.request_signature(request).unwrap();

        // Wait for result
        let result = generator.wait_for_result().unwrap();
        assert_eq!(result.request_id, 1);
        assert!(result.signature.is_some());
        assert!(result.error.is_none());

        generator.shutdown().unwrap();
    }

    #[test]
    fn test_async_signature_multiple() {
        let files: Vec<_> = (0..5)
            .map(|_| create_test_file(512).unwrap())
            .collect();

        let config = AsyncSignatureConfig::default().with_threads(2);
        let mut generator = AsyncSignatureGenerator::new(config);

        // Queue multiple requests
        for (i, file) in files.iter().enumerate() {
            let request = SignatureRequest {
                request_id: i as u64,
                basis_path: file.path().to_path_buf(),
                basis_size: 512,
                protocol: ProtocolVersion::NEWEST,
                checksum_length: NonZeroU8::new(16).unwrap(),
                checksum_algorithm: SignatureAlgorithm::Md4,
            };
            generator.request_signature(request).unwrap();
        }

        // Collect results (may arrive in any order with multiple threads)
        let mut results = Vec::new();
        for _ in 0..5 {
            let result = generator.wait_for_result().unwrap();
            results.push(result);
        }

        // Sort by request_id to check all arrived
        results.sort_by_key(|r| r.request_id);

        assert_eq!(results.len(), 5);
        for (i, result) in results.iter().enumerate() {
            assert_eq!(result.request_id, i as u64);
            assert!(result.signature.is_some(), "Result {i} has no signature");
        }

        generator.shutdown().unwrap();
    }

    #[test]
    fn test_async_signature_nonexistent_file() {
        let config = AsyncSignatureConfig::default().with_threads(1);
        let mut generator = AsyncSignatureGenerator::new(config);

        let request = SignatureRequest {
            request_id: 1,
            basis_path: PathBuf::from("/nonexistent/file"),
            basis_size: 1024,
            protocol: ProtocolVersion::NEWEST,
            checksum_length: NonZeroU8::new(16).unwrap(),
            checksum_algorithm: SignatureAlgorithm::Md4,
        };

        generator.request_signature(request).unwrap();

        // Should get an error result
        let result = generator.wait_for_result().unwrap();
        assert_eq!(result.request_id, 1);
        assert!(result.signature.is_none());
        assert!(result.error.is_some());

        generator.shutdown().unwrap();
    }

    #[test]
    fn test_try_get_result_non_blocking() {
        let config = AsyncSignatureConfig::default().with_threads(1);
        let generator = AsyncSignatureGenerator::new(config);

        // No requests queued - should return None immediately
        assert!(generator.try_get_result().is_none());

        generator.shutdown().unwrap();
    }

    #[test]
    fn test_config_defaults() {
        let config = AsyncSignatureConfig::default();
        assert!(config.num_threads > 0);
        assert!(config.num_threads <= 4);
        assert_eq!(config.max_pending, 16);
    }

    #[test]
    fn test_config_customization() {
        let config = AsyncSignatureConfig::default()
            .with_threads(3)
            .with_max_pending(32);

        assert_eq!(config.num_threads, 3);
        assert_eq!(config.max_pending, 32);
    }

    #[test]
    fn test_next_request_id_increments() {
        let config = AsyncSignatureConfig::default();
        let mut generator = AsyncSignatureGenerator::new(config);

        assert_eq!(generator.next_request_id(), 0);
        assert_eq!(generator.next_request_id(), 1);
        assert_eq!(generator.next_request_id(), 2);

        generator.shutdown().unwrap();
    }
}
