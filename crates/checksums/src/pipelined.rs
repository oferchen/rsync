//! Pipelined checksum computation with double-buffering.
//!
//! This module provides a `DoubleBufferedReader` that overlaps I/O with checksum
//! computation by using two buffers in a producer-consumer pattern:
//!
//! ```text
//! ┌──────────────────────────────────────────────────────────────────────────┐
//! │                    Double-Buffered Checksum Pipeline                     │
//! ├──────────────────────────────────────────────────────────────────────────┤
//! │                                                                          │
//! │  Time →                                                                  │
//! │                                                                          │
//! │  Without pipelining (sequential):                                        │
//! │  ┌────────┐ ┌──────────────┐ ┌────────┐ ┌──────────────┐                │
//! │  │ Read 1 │ │ Checksum 1   │ │ Read 2 │ │ Checksum 2   │ ...            │
//! │  └────────┘ └──────────────┘ └────────┘ └──────────────┘                │
//! │                                                                          │
//! │  With pipelining (overlapped):                                           │
//! │  ┌────────┐ ┌────────┐ ┌────────┐                                       │
//! │  │ Read 1 │ │ Read 2 │ │ Read 3 │ ...                                   │
//! │  └────────┘ └──────────────┘ └──────────────┘                            │
//! │            │ Checksum 1   │ │ Checksum 2   │ ...                         │
//! │            └──────────────┘ └──────────────┘                            │
//! │                                                                          │
//! └──────────────────────────────────────────────────────────────────────────┘
//! ```
//!
//! # Performance Benefits
//!
//! For CPU-intensive checksums (MD4/MD5/SHA1), pipelining can provide 20-40%
//! throughput improvement by hiding I/O latency behind computation:
//!
//! - Sequential: `total_time = n * (read_time + checksum_time)`
//! - Pipelined: `total_time ≈ n * max(read_time, checksum_time)`
//!
//! The benefit is maximized when:
//! - I/O and computation times are balanced
//! - Block sizes are large enough to amortize synchronization overhead
//! - The underlying storage is fast (SSD/NVMe)
//!
//! # Example
//!
//! ```ignore
//! use checksums::pipelined::{DoubleBufferedReader, PipelineConfig};
//! use checksums::{RollingChecksum, RollingDigest};
//! use checksums::strong::{Md5, StrongDigest};
//! use std::fs::File;
//!
//! let file = File::open("large_file.dat")?;
//! let config = PipelineConfig::default().with_block_size(64 * 1024);
//! let mut reader = DoubleBufferedReader::new(file, config);
//!
//! while let Some(block) = reader.next_block()? {
//!     // Compute checksums on current block while next is being read
//!     let rolling = RollingDigest::from_bytes(&block);
//!     let strong = Md5::digest(&block);
//!     // Process checksums...
//! }
//! # Ok::<(), std::io::Error>(())
//! ```

use std::io::{self, Read};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};

use crate::strong::StrongDigest;
use crate::RollingDigest;

/// Configuration for the double-buffered checksum pipeline.
#[derive(Clone, Copy, Debug)]
pub struct PipelineConfig {
    /// Size of each buffer in bytes.
    ///
    /// Larger buffers improve throughput but increase memory usage.
    /// Default: 64 KiB
    pub block_size: usize,

    /// Minimum file size to enable pipelining.
    ///
    /// Files smaller than this will use direct (non-pipelined) reading
    /// to avoid thread overhead for trivial workloads.
    /// Default: 256 KiB
    pub min_file_size: u64,

    /// Whether to use pipelining.
    ///
    /// When false, reads are done synchronously without spawning a thread.
    /// Default: true
    pub enabled: bool,
}

impl Default for PipelineConfig {
    fn default() -> Self {
        Self {
            block_size: 64 * 1024,       // 64 KiB
            min_file_size: 256 * 1024,   // 256 KiB
            enabled: true,
        }
    }
}

impl PipelineConfig {
    /// Creates a new configuration with default settings.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the block size for each buffer.
    #[must_use]
    pub const fn with_block_size(mut self, size: usize) -> Self {
        self.block_size = size;
        self
    }

    /// Sets the minimum file size for enabling pipelining.
    #[must_use]
    pub const fn with_min_file_size(mut self, size: u64) -> Self {
        self.min_file_size = size;
        self
    }

    /// Enables or disables pipelining.
    #[must_use]
    pub const fn with_enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }
}

/// Message sent from the I/O thread to the main thread.
enum IoMessage {
    /// A block of data was successfully read.
    Block(Vec<u8>),
    /// End of input reached.
    Eof,
    /// An I/O error occurred.
    Error(io::Error),
}

/// Double-buffered reader for pipelined checksum computation.
///
/// Uses a background thread to read the next block while the main thread
/// processes the current block. This overlaps I/O with computation.
///
/// # Thread Safety
///
/// The reader spawns a background thread for I/O. The thread is automatically
/// joined when the reader is dropped or when EOF/error is encountered.
pub struct DoubleBufferedReader<R> {
    /// Configuration for buffer sizes.
    config: PipelineConfig,
    /// Receiver for data blocks from I/O thread.
    receiver: Option<Receiver<IoMessage>>,
    /// Handle to join the I/O thread.
    io_thread: Option<JoinHandle<()>>,
    /// Current block being processed.
    current_block: Option<Vec<u8>>,
    /// Prefetched next block (for synchronous mode).
    prefetched_block: Option<Vec<u8>>,
    /// Whether we've reached EOF.
    eof_reached: bool,
    /// Direct reader for synchronous mode (when pipelining disabled).
    direct_reader: Option<R>,
    /// Whether we're in synchronous mode.
    synchronous: bool,
}

impl<R: Read + Send + 'static> DoubleBufferedReader<R> {
    /// Creates a new double-buffered reader.
    ///
    /// If the file is smaller than `config.min_file_size` or pipelining is
    /// disabled, the reader operates in synchronous mode without spawning
    /// a background thread.
    ///
    /// # Arguments
    ///
    /// * `reader` - The underlying reader to read from
    /// * `config` - Configuration for buffer sizes and pipelining
    #[must_use]
    pub fn new(reader: R, config: PipelineConfig) -> Self {
        Self::with_size_hint(reader, config, None)
    }

    /// Creates a new double-buffered reader with a size hint.
    ///
    /// The size hint is used to decide whether to enable pipelining.
    /// If the size is smaller than `config.min_file_size`, synchronous
    /// mode is used.
    #[must_use]
    pub fn with_size_hint(mut reader: R, config: PipelineConfig, size_hint: Option<u64>) -> Self {
        let should_pipeline = config.enabled
            && size_hint.map_or(true, |size| size >= config.min_file_size);

        if !should_pipeline {
            // Synchronous mode: no background thread
            return Self {
                config,
                receiver: None,
                io_thread: None,
                current_block: None,
                prefetched_block: None,
                eof_reached: false,
                direct_reader: Some(reader),
                synchronous: true,
            };
        }

        // Pipelined mode: spawn I/O thread
        let (sender, receiver) = mpsc::channel();
        let block_size = config.block_size;

        // Read first block synchronously to have it ready immediately
        let mut first_block = vec![0u8; block_size];
        let first_read = match read_exact_or_eof(&mut reader, &mut first_block) {
            Ok(0) => {
                // Empty file - EOF immediately
                return Self {
                    config,
                    receiver: None,
                    io_thread: None,
                    current_block: None,
                    prefetched_block: None,
                    eof_reached: true,
                    direct_reader: None,
                    synchronous: false,
                };
            }
            Ok(n) => {
                first_block.truncate(n);
                Some(first_block)
            }
            Err(_) => {
                return Self {
                    config,
                    receiver: None,
                    io_thread: None,
                    current_block: None,
                    prefetched_block: None,
                    eof_reached: true,
                    direct_reader: Some(reader),
                    synchronous: true,
                };
            }
        };

        let io_thread = thread::spawn(move || {
            io_thread_main(reader, sender, block_size);
        });

        Self {
            config,
            receiver: Some(receiver),
            io_thread: Some(io_thread),
            current_block: first_read,
            prefetched_block: None,
            eof_reached: false,
            direct_reader: None,
            synchronous: false,
        }
    }

    /// Returns the next block of data, or `None` if EOF reached.
    ///
    /// This method returns data that was pre-read while the previous block
    /// was being processed, then initiates reading the next block.
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying reader encounters an I/O error.
    pub fn next_block(&mut self) -> io::Result<Option<&[u8]>> {
        if self.eof_reached {
            return Ok(None);
        }

        if self.synchronous {
            return self.next_block_sync();
        }

        // Take current block and get next from receiver
        let current = self.current_block.take();

        if current.is_none() {
            // First call after EOF was signaled
            self.eof_reached = true;
            return Ok(None);
        }

        // Try to get next block from I/O thread
        if let Some(ref receiver) = self.receiver {
            match receiver.recv() {
                Ok(IoMessage::Block(data)) => {
                    self.current_block = Some(data);
                }
                Ok(IoMessage::Eof) => {
                    self.eof_reached = true;
                    self.current_block = None;
                }
                Ok(IoMessage::Error(e)) => {
                    self.eof_reached = true;
                    return Err(e);
                }
                Err(_) => {
                    // Channel closed - I/O thread terminated
                    self.eof_reached = true;
                    self.current_block = None;
                }
            }
        }

        // Store the current block we just took and return reference to it
        self.prefetched_block = current;
        Ok(self.prefetched_block.as_deref())
    }

    /// Synchronous block reading for small files.
    fn next_block_sync(&mut self) -> io::Result<Option<&[u8]>> {
        if let Some(ref mut reader) = self.direct_reader {
            let mut buffer = vec![0u8; self.config.block_size];
            let bytes_read = read_exact_or_eof(reader, &mut buffer)?;

            if bytes_read == 0 {
                self.eof_reached = true;
                return Ok(None);
            }

            buffer.truncate(bytes_read);
            self.current_block = Some(buffer);
            Ok(self.current_block.as_deref())
        } else {
            self.eof_reached = true;
            Ok(None)
        }
    }

    /// Returns true if pipelining is active (background thread running).
    #[must_use]
    pub fn is_pipelined(&self) -> bool {
        !self.synchronous && self.io_thread.is_some()
    }

    /// Returns the configured block size.
    #[must_use]
    pub fn block_size(&self) -> usize {
        self.config.block_size
    }
}

impl<R> Drop for DoubleBufferedReader<R> {
    fn drop(&mut self) {
        // Drop receiver first to signal I/O thread to stop
        drop(self.receiver.take());

        // Wait for I/O thread to finish
        if let Some(handle) = self.io_thread.take() {
            // Ignore join errors (thread may have panicked)
            let _ = handle.join();
        }
    }
}

/// Main loop for the I/O thread.
fn io_thread_main<R: Read>(mut reader: R, sender: Sender<IoMessage>, block_size: usize) {
    loop {
        let mut buffer = vec![0u8; block_size];

        match read_exact_or_eof(&mut reader, &mut buffer) {
            Ok(0) => {
                // EOF reached
                let _ = sender.send(IoMessage::Eof);
                break;
            }
            Ok(n) => {
                buffer.truncate(n);
                if sender.send(IoMessage::Block(buffer)).is_err() {
                    // Receiver dropped - main thread no longer interested
                    break;
                }
            }
            Err(e) => {
                let _ = sender.send(IoMessage::Error(e));
                break;
            }
        }
    }
}

/// Reads up to `buffer.len()` bytes, returning the number of bytes read.
///
/// Unlike `read_exact`, this handles partial reads and EOF gracefully.
fn read_exact_or_eof<R: Read>(reader: &mut R, buffer: &mut [u8]) -> io::Result<usize> {
    let mut total_read = 0;

    while total_read < buffer.len() {
        match reader.read(&mut buffer[total_read..]) {
            Ok(0) => break, // EOF
            Ok(n) => total_read += n,
            Err(ref e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }

    Ok(total_read)
}

/// Result of computing checksums for a single block.
#[derive(Clone, Debug)]
pub struct BlockChecksums<D> {
    /// The rolling checksum (weak hash).
    pub rolling: RollingDigest,
    /// The strong checksum digest.
    pub strong: D,
    /// Number of bytes in this block.
    pub len: usize,
}

/// Computes checksums for all blocks in a reader using double-buffering.
///
/// This is a convenience function that combines `DoubleBufferedReader` with
/// checksum computation. It processes blocks as they're read, overlapping
/// I/O with computation.
///
/// # Type Parameters
///
/// * `D` - The strong digest algorithm to use (e.g., `Md5`, `Sha256`)
/// * `R` - The reader type
///
/// # Arguments
///
/// * `reader` - The input reader
/// * `config` - Pipeline configuration
///
/// # Returns
///
/// A vector of `BlockChecksums` for each block read from the input.
///
/// # Errors
///
/// Returns an error if reading from the input fails.
///
/// # Example
///
/// ```ignore
/// use checksums::pipelined::{compute_checksums_pipelined, PipelineConfig};
/// use checksums::strong::Md5;
/// use std::io::Cursor;
///
/// let data = vec![0u8; 256 * 1024];
/// let config = PipelineConfig::default().with_block_size(64 * 1024);
/// let checksums = compute_checksums_pipelined::<Md5, _>(
///     Cursor::new(data),
///     config,
///     None,
/// )?;
/// assert_eq!(checksums.len(), 4);
/// # Ok::<(), std::io::Error>(())
/// ```
pub fn compute_checksums_pipelined<D, R>(
    reader: R,
    config: PipelineConfig,
    size_hint: Option<u64>,
) -> io::Result<Vec<BlockChecksums<D::Digest>>>
where
    D: StrongDigest,
    D::Seed: Default,
    R: Read + Send + 'static,
{
    let mut buffered_reader = DoubleBufferedReader::with_size_hint(reader, config, size_hint);
    let mut results = Vec::new();

    while let Some(block) = buffered_reader.next_block()? {
        let rolling = RollingDigest::from_bytes(block);
        let strong = D::digest(block);
        results.push(BlockChecksums {
            rolling,
            strong,
            len: block.len(),
        });
    }

    Ok(results)
}

/// Streaming iterator for pipelined checksum computation.
///
/// Unlike `compute_checksums_pipelined`, this allows processing checksums
/// one at a time without collecting all results into a vector.
pub struct PipelinedChecksumIterator<D, R>
where
    D: StrongDigest,
{
    reader: DoubleBufferedReader<R>,
    _phantom: std::marker::PhantomData<D>,
}

impl<D, R> PipelinedChecksumIterator<D, R>
where
    D: StrongDigest,
    D::Seed: Default,
    R: Read + Send + 'static,
{
    /// Creates a new pipelined checksum iterator.
    #[must_use]
    pub fn new(reader: R, config: PipelineConfig) -> Self {
        Self::with_size_hint(reader, config, None)
    }

    /// Creates a new pipelined checksum iterator with a size hint.
    #[must_use]
    pub fn with_size_hint(reader: R, config: PipelineConfig, size_hint: Option<u64>) -> Self {
        Self {
            reader: DoubleBufferedReader::with_size_hint(reader, config, size_hint),
            _phantom: std::marker::PhantomData,
        }
    }

    /// Returns the next block's checksums, or `None` if EOF reached.
    ///
    /// # Errors
    ///
    /// Returns an error if reading fails.
    pub fn next(&mut self) -> io::Result<Option<BlockChecksums<D::Digest>>> {
        match self.reader.next_block()? {
            Some(block) => {
                let rolling = RollingDigest::from_bytes(block);
                let strong = D::digest(block);
                Ok(Some(BlockChecksums {
                    rolling,
                    strong,
                    len: block.len(),
                }))
            }
            None => Ok(None),
        }
    }

    /// Returns whether the iterator is using pipelined reading.
    #[must_use]
    pub fn is_pipelined(&self) -> bool {
        self.reader.is_pipelined()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::strong::Md5;
    use std::io::Cursor;

    #[test]
    fn double_buffered_reader_basic() {
        let data = vec![0xAB; 256 * 1024];
        let config = PipelineConfig::default().with_block_size(64 * 1024);
        let mut reader = DoubleBufferedReader::new(Cursor::new(data.clone()), config);

        let mut total_bytes = 0;
        let mut block_count = 0;

        while let Some(block) = reader.next_block().unwrap() {
            total_bytes += block.len();
            block_count += 1;
            assert!(block.iter().all(|&b| b == 0xAB));
        }

        assert_eq!(total_bytes, data.len());
        assert_eq!(block_count, 4);
    }

    #[test]
    fn double_buffered_reader_small_file_sync() {
        // File smaller than min_file_size should use sync mode
        let data = vec![0xCD; 64 * 1024];
        let config = PipelineConfig::default()
            .with_block_size(32 * 1024)
            .with_min_file_size(128 * 1024);

        let mut reader =
            DoubleBufferedReader::with_size_hint(Cursor::new(data.clone()), config, Some(64 * 1024));

        // Should be in synchronous mode
        assert!(!reader.is_pipelined());

        let mut total_bytes = 0;
        while let Some(block) = reader.next_block().unwrap() {
            total_bytes += block.len();
        }
        assert_eq!(total_bytes, data.len());
    }

    #[test]
    fn double_buffered_reader_pipelined_mode() {
        let data = vec![0xEF; 512 * 1024];
        let config = PipelineConfig::default()
            .with_block_size(64 * 1024)
            .with_min_file_size(128 * 1024);

        let reader =
            DoubleBufferedReader::with_size_hint(Cursor::new(data), config, Some(512 * 1024));

        // Should be in pipelined mode
        assert!(reader.is_pipelined());
    }

    #[test]
    fn double_buffered_reader_empty_input() {
        let data: Vec<u8> = vec![];
        let config = PipelineConfig::default();
        let mut reader = DoubleBufferedReader::new(Cursor::new(data), config);

        assert!(reader.next_block().unwrap().is_none());
    }

    #[test]
    fn double_buffered_reader_partial_last_block() {
        // 100 KB data with 64 KB blocks = 1 full + 1 partial
        let data = vec![0x12; 100 * 1024];
        let config = PipelineConfig::default().with_block_size(64 * 1024);
        let mut reader = DoubleBufferedReader::new(Cursor::new(data.clone()), config);

        let block1 = reader.next_block().unwrap().unwrap();
        assert_eq!(block1.len(), 64 * 1024);

        let block2 = reader.next_block().unwrap().unwrap();
        assert_eq!(block2.len(), 36 * 1024); // 100 - 64 = 36 KB

        assert!(reader.next_block().unwrap().is_none());
    }

    #[test]
    fn double_buffered_reader_disabled_pipelining() {
        let data = vec![0x34; 512 * 1024];
        let config = PipelineConfig::default()
            .with_block_size(64 * 1024)
            .with_enabled(false);

        let reader = DoubleBufferedReader::new(Cursor::new(data), config);

        // Should be in synchronous mode when disabled
        assert!(!reader.is_pipelined());
    }

    #[test]
    fn compute_checksums_pipelined_basic() {
        let data = vec![0x56; 256 * 1024];
        let config = PipelineConfig::default().with_block_size(64 * 1024);

        let checksums = compute_checksums_pipelined::<Md5, _>(
            Cursor::new(data.clone()),
            config,
            Some(256 * 1024),
        )
        .unwrap();

        assert_eq!(checksums.len(), 4);

        // Verify checksums match direct computation
        for (i, cs) in checksums.iter().enumerate() {
            let start = i * 64 * 1024;
            let end = (start + 64 * 1024).min(data.len());
            let block = &data[start..end];

            let expected_rolling = RollingDigest::from_bytes(block);
            let expected_strong = Md5::digest(block);

            assert_eq!(cs.rolling, expected_rolling);
            assert_eq!(cs.strong.as_ref(), expected_strong.as_ref());
            assert_eq!(cs.len, block.len());
        }
    }

    #[test]
    fn compute_checksums_pipelined_empty() {
        let data: Vec<u8> = vec![];
        let config = PipelineConfig::default();

        let checksums =
            compute_checksums_pipelined::<Md5, _>(Cursor::new(data), config, Some(0)).unwrap();

        assert!(checksums.is_empty());
    }

    #[test]
    fn pipelined_iterator_basic() {
        let data = vec![0x78; 128 * 1024];
        let config = PipelineConfig::default().with_block_size(32 * 1024);

        let mut iter: PipelinedChecksumIterator<Md5, _> =
            PipelinedChecksumIterator::new(Cursor::new(data.clone()), config);

        let mut count = 0;
        while let Some(cs) = iter.next().unwrap() {
            assert_eq!(cs.len, 32 * 1024);
            count += 1;
        }

        assert_eq!(count, 4);
    }

    #[test]
    fn pipeline_config_builder() {
        let config = PipelineConfig::new()
            .with_block_size(128 * 1024)
            .with_min_file_size(512 * 1024)
            .with_enabled(false);

        assert_eq!(config.block_size, 128 * 1024);
        assert_eq!(config.min_file_size, 512 * 1024);
        assert!(!config.enabled);
    }

    #[test]
    fn block_checksums_clone_debug() {
        let cs = BlockChecksums {
            rolling: RollingDigest::from_bytes(b"test"),
            strong: [0u8; 16],
            len: 4,
        };

        let cloned = cs.clone();
        assert_eq!(cloned.rolling, cs.rolling);
        assert_eq!(cloned.strong, cs.strong);
        assert_eq!(cloned.len, cs.len);

        let debug = format!("{cs:?}");
        assert!(debug.contains("BlockChecksums"));
    }

    #[test]
    fn multiple_reads_same_content() {
        // Verify that pipelined reading produces same results as sequential
        let data = vec![0x9A; 256 * 1024];
        let config = PipelineConfig::default().with_block_size(64 * 1024);

        // Pipelined read
        let pipelined =
            compute_checksums_pipelined::<Md5, _>(Cursor::new(data.clone()), config, None).unwrap();

        // Sequential read (disabled pipelining)
        let sync_config = config.with_enabled(false);
        let sequential =
            compute_checksums_pipelined::<Md5, _>(Cursor::new(data), sync_config, None).unwrap();

        assert_eq!(pipelined.len(), sequential.len());
        for (p, s) in pipelined.iter().zip(sequential.iter()) {
            assert_eq!(p.rolling, s.rolling);
            assert_eq!(p.strong.as_ref(), s.strong.as_ref());
            assert_eq!(p.len, s.len);
        }
    }

    #[test]
    fn handles_exact_block_boundary() {
        // File size exactly divisible by block size
        let data = vec![0xBC; 128 * 1024];
        let config = PipelineConfig::default().with_block_size(64 * 1024);

        let mut reader = DoubleBufferedReader::new(Cursor::new(data), config);

        let block1 = reader.next_block().unwrap().unwrap();
        assert_eq!(block1.len(), 64 * 1024);

        let block2 = reader.next_block().unwrap().unwrap();
        assert_eq!(block2.len(), 64 * 1024);

        assert!(reader.next_block().unwrap().is_none());
    }

    #[test]
    fn handles_very_small_blocks() {
        let data = vec![0xDE; 1000];
        let config = PipelineConfig::default()
            .with_block_size(100)
            .with_min_file_size(0); // Enable pipelining even for small files

        let checksums =
            compute_checksums_pipelined::<Md5, _>(Cursor::new(data), config, Some(1000)).unwrap();

        assert_eq!(checksums.len(), 10);
    }

    #[test]
    fn reader_thread_cleanup_on_drop() {
        let data = vec![0xF0; 1024 * 1024];
        let config = PipelineConfig::default().with_block_size(64 * 1024);

        {
            let mut reader =
                DoubleBufferedReader::with_size_hint(Cursor::new(data), config, Some(1024 * 1024));

            // Read just one block then drop
            let _ = reader.next_block().unwrap();
        }

        // If we get here without hanging, the thread cleanup worked
    }
}
