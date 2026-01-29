//! Overlapped I/O for concurrent network read and disk write operations.
//!
//! This module implements a producer-consumer pattern to overlap network I/O
//! (reading delta data) with disk I/O (writing reconstructed files). While one
//! file is being written to disk, the next file's delta can be read from the network.
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────────────────┐
//! │                    Overlapped I/O Architecture                          │
//! ├─────────────────────────────────────────────────────────────────────────┤
//! │                                                                         │
//! │  Main Thread (Producer)              Writer Thread (Consumer)           │
//! │  ┌─────────────────────┐             ┌─────────────────────┐           │
//! │  │ Read delta from     │             │ Write file to disk  │           │
//! │  │ network             │             │                     │           │
//! │  │                     │             │ - Apply metadata    │           │
//! │  │ - NDX + iflags      │  ────────▶  │ - Atomic rename     │           │
//! │  │ - sum_head          │   Bounded   │ - Fsync if needed   │           │
//! │  │ - Delta tokens      │   Channel   │                     │           │
//! │  │ - Checksum          │             │                     │           │
//! │  └─────────────────────┘             └─────────────────────┘           │
//! │                                                                         │
//! │  Pipeline depth: N files can be buffered between read and write        │
//! │                                                                         │
//! └─────────────────────────────────────────────────────────────────────────┘
//! ```
//!
//! # Performance Benefits
//!
//! Without overlapping:
//! - File 1: [Read from network][Write to disk]
//! - File 2: [Read from network][Write to disk]
//!
//! With overlapping:
//! - File 1: [Read from network][Write to disk]
//! - File 2:                    [Read from network][Write to disk]
//!
//! For a transfer with many small files, this can significantly reduce total
//! transfer time by utilizing both network and disk bandwidth concurrently.
//!
//! # Memory Usage
//!
//! Memory is bounded by:
//! - Channel capacity: `overlap_depth` complete file buffers
//! - Each buffer contains the fully reconstructed file data
//! - Maximum memory ≈ overlap_depth × max_file_size
//!
//! To limit memory, the `max_buffer_size` option can skip overlapping for
//! large files.

use std::fs;
use std::io::{self, Write as IoWrite};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::thread::{self, JoinHandle};

use metadata::{MetadataOptions, apply_metadata_from_file_entry};
use protocol::flist::FileEntry;

use crate::temp_guard::TempFileGuard;

/// Configuration for overlapped I/O.
#[derive(Debug, Clone)]
pub struct OverlappedConfig {
    /// Number of files that can be buffered between network read and disk write.
    ///
    /// Higher values increase potential overlap but use more memory.
    /// Default: 2 (one being written, one being read)
    pub overlap_depth: usize,

    /// Maximum file size to buffer for overlapping.
    ///
    /// Files larger than this are written directly without buffering.
    /// Default: 64MB
    pub max_buffer_size: u64,

    /// Whether to fsync files after writing.
    pub do_fsync: bool,
}

impl Default for OverlappedConfig {
    fn default() -> Self {
        Self {
            overlap_depth: 2,
            max_buffer_size: 64 * 1024 * 1024, // 64MB
            do_fsync: false,
        }
    }
}

impl OverlappedConfig {
    /// Creates a new configuration with the specified overlap depth.
    #[must_use]
    pub fn with_overlap_depth(mut self, depth: usize) -> Self {
        self.overlap_depth = depth.max(1);
        self
    }

    /// Sets the maximum buffer size for overlapping.
    #[must_use]
    pub fn with_max_buffer_size(mut self, size: u64) -> Self {
        self.max_buffer_size = size;
        self
    }

    /// Sets whether to fsync files after writing.
    #[must_use]
    pub const fn with_fsync(mut self, do_fsync: bool) -> Self {
        self.do_fsync = do_fsync;
        self
    }

    /// Creates a synchronous configuration (no overlapping).
    #[must_use]
    pub fn synchronous() -> Self {
        Self {
            overlap_depth: 0,
            max_buffer_size: 0,
            do_fsync: false,
        }
    }
}

/// A completed file ready to be written to disk.
///
/// Contains all the data needed to atomically write the file.
#[derive(Debug)]
pub struct CompletedFile {
    /// Destination path for the file.
    pub file_path: PathBuf,
    /// Temporary path for atomic write.
    pub temp_path: PathBuf,
    /// Reconstructed file data.
    pub data: Vec<u8>,
    /// File entry from file list (for metadata).
    pub file_entry_index: usize,
    /// Bytes received for this file (for stats).
    pub bytes_received: u64,
}

/// Result from the writer thread for a single file.
#[derive(Debug)]
pub struct WriteResult {
    /// File entry index that was written.
    pub file_entry_index: usize,
    /// Bytes written for this file.
    pub bytes_written: u64,
    /// Error if write failed.
    pub error: Option<String>,
    /// Metadata error if metadata application failed.
    pub metadata_error: Option<(PathBuf, String)>,
}

/// Message sent to the writer thread.
enum WriterMessage {
    /// A completed file to write to disk.
    WriteFile(CompletedFile),
    /// Shutdown signal.
    Shutdown,
}

/// Handle to the writer thread for overlapped I/O.
pub struct OverlappedWriter {
    /// Sender to the writer thread.
    sender: SyncSender<WriterMessage>,
    /// Handle to the writer thread.
    handle: Option<JoinHandle<Vec<WriteResult>>>,
    /// Receiver for write results (writer thread sends results back).
    result_receiver: Receiver<WriteResult>,
    /// Configuration.
    config: OverlappedConfig,
    /// Number of files currently in flight.
    in_flight: usize,
}

impl OverlappedWriter {
    /// Creates a new overlapped writer.
    ///
    /// Spawns a background thread that writes files to disk.
    ///
    /// # Arguments
    ///
    /// * `config` - Configuration for overlapped I/O
    /// * `metadata_opts` - Metadata options for file attributes
    /// * `file_list` - Reference to the file list for metadata lookup
    pub fn new(
        config: OverlappedConfig,
        metadata_opts: MetadataOptions,
        file_list: Vec<FileEntry>,
    ) -> Self {
        let depth = config.overlap_depth.max(1);
        let (sender, receiver) = mpsc::sync_channel::<WriterMessage>(depth);
        let (result_sender, result_receiver) = mpsc::sync_channel::<WriteResult>(depth);
        let do_fsync = config.do_fsync;

        let handle = thread::spawn(move || {
            writer_thread_main(receiver, result_sender, metadata_opts, file_list, do_fsync)
        });

        Self {
            sender,
            handle: Some(handle),
            result_receiver,
            config,
            in_flight: 0,
        }
    }

    /// Returns true if overlapping is enabled.
    #[must_use]
    pub fn is_enabled(&self) -> bool {
        self.config.overlap_depth > 0
    }

    /// Returns true if a file should be buffered for overlapping.
    ///
    /// Large files are written directly to avoid excessive memory usage.
    #[must_use]
    pub fn should_buffer(&self, file_size: u64) -> bool {
        self.is_enabled() && file_size <= self.config.max_buffer_size
    }

    /// Queues a completed file for writing.
    ///
    /// This may block if the channel is full (backpressure).
    ///
    /// # Errors
    ///
    /// Returns an error if the writer thread has panicked.
    pub fn queue_write(&mut self, file: CompletedFile) -> io::Result<()> {
        self.sender
            .send(WriterMessage::WriteFile(file))
            .map_err(|_| {
                io::Error::new(io::ErrorKind::BrokenPipe, "writer thread has terminated")
            })?;
        self.in_flight += 1;
        Ok(())
    }

    /// Collects write results without blocking.
    ///
    /// Returns all available results from completed writes.
    pub fn try_collect_results(&mut self) -> Vec<WriteResult> {
        let mut results = Vec::new();
        while let Ok(result) = self.result_receiver.try_recv() {
            self.in_flight = self.in_flight.saturating_sub(1);
            results.push(result);
        }
        results
    }

    /// Waits for one write result.
    ///
    /// Blocks until at least one write completes.
    ///
    /// # Errors
    ///
    /// Returns an error if the writer thread has terminated.
    pub fn wait_for_result(&mut self) -> io::Result<WriteResult> {
        let result = self.result_receiver.recv().map_err(|_| {
            io::Error::new(io::ErrorKind::BrokenPipe, "writer thread has terminated")
        })?;
        self.in_flight = self.in_flight.saturating_sub(1);
        Ok(result)
    }

    /// Returns the number of files currently being processed.
    #[must_use]
    pub fn in_flight(&self) -> usize {
        self.in_flight
    }

    /// Returns true if the pipeline has available capacity.
    #[must_use]
    pub fn has_capacity(&self) -> bool {
        self.in_flight < self.config.overlap_depth
    }

    /// Shuts down the writer thread and waits for completion.
    ///
    /// Returns all remaining write results.
    ///
    /// # Errors
    ///
    /// Returns an error if the writer thread panicked.
    pub fn shutdown(mut self) -> io::Result<Vec<WriteResult>> {
        // Send shutdown signal
        let _ = self.sender.send(WriterMessage::Shutdown);

        // Wait for thread to finish
        let handle = self.handle.take();
        if let Some(h) = handle {
            match h.join() {
                Ok(results) => Ok(results),
                Err(_) => Err(io::Error::new(
                    io::ErrorKind::Other,
                    "writer thread panicked",
                )),
            }
        } else {
            Ok(Vec::new())
        }
    }
}

impl Drop for OverlappedWriter {
    fn drop(&mut self) {
        // Send shutdown signal
        let _ = self.sender.send(WriterMessage::Shutdown);

        // Wait for thread to finish
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// Main loop for the writer thread.
///
/// Processes completed files and writes them to disk.
fn writer_thread_main(
    receiver: Receiver<WriterMessage>,
    result_sender: SyncSender<WriteResult>,
    metadata_opts: MetadataOptions,
    file_list: Vec<FileEntry>,
    do_fsync: bool,
) -> Vec<WriteResult> {
    let mut results = Vec::new();

    loop {
        match receiver.recv() {
            Ok(WriterMessage::WriteFile(file)) => {
                let result = write_file_to_disk(&file, &metadata_opts, &file_list, do_fsync);
                // Send result back to main thread
                if result_sender.send(result.clone()).is_err() {
                    // Main thread has dropped receiver, store result locally
                    results.push(result);
                }
            }
            Ok(WriterMessage::Shutdown) | Err(_) => {
                // Channel closed or shutdown requested
                break;
            }
        }
    }

    // Drain any remaining messages
    while let Ok(msg) = receiver.try_recv() {
        if let WriterMessage::WriteFile(file) = msg {
            let result = write_file_to_disk(&file, &metadata_opts, &file_list, do_fsync);
            results.push(result);
        }
    }

    results
}

/// Writes a completed file to disk with atomic rename.
fn write_file_to_disk(
    file: &CompletedFile,
    metadata_opts: &MetadataOptions,
    file_list: &[FileEntry],
    do_fsync: bool,
) -> WriteResult {
    let mut result = WriteResult {
        file_entry_index: file.file_entry_index,
        bytes_written: 0,
        error: None,
        metadata_error: None,
    };

    // Create temp file guard for cleanup on failure
    let mut temp_guard = TempFileGuard::new(file.temp_path.clone());

    // Write data to temp file
    match fs::File::create(&file.temp_path) {
        Ok(mut output) => {
            if let Err(e) = output.write_all(&file.data) {
                result.error = Some(format!("failed to write data: {e}"));
                return result;
            }

            // Fsync if requested
            if do_fsync {
                if let Err(e) = output.sync_all() {
                    result.error = Some(format!("fsync failed: {e}"));
                    return result;
                }
            }

            // Drop file handle before rename
            drop(output);

            // Atomic rename
            if let Err(e) = fs::rename(&file.temp_path, &file.file_path) {
                result.error = Some(format!("rename failed: {e}"));
                return result;
            }

            // Success - keep temp file (now renamed)
            temp_guard.keep();
            result.bytes_written = file.data.len() as u64;

            // Apply metadata
            if let Some(file_entry) = file_list.get(file.file_entry_index) {
                if let Err(meta_err) =
                    apply_metadata_from_file_entry(&file.file_path, file_entry, metadata_opts)
                {
                    result.metadata_error = Some((file.file_path.clone(), meta_err.to_string()));
                }
            }
        }
        Err(e) => {
            result.error = Some(format!("failed to create temp file: {e}"));
        }
    }

    result
}

/// Statistics from overlapped I/O operations.
#[derive(Debug, Clone, Default)]
pub struct OverlappedStats {
    /// Number of files written via overlapped I/O.
    pub files_overlapped: u64,
    /// Number of files written directly (too large for overlapping).
    pub files_direct: u64,
    /// Total bytes written via overlapped I/O.
    pub bytes_overlapped: u64,
    /// Total bytes written directly.
    pub bytes_direct: u64,
}

impl OverlappedStats {
    /// Returns the total number of files processed.
    #[must_use]
    pub const fn total_files(&self) -> u64 {
        self.files_overlapped + self.files_direct
    }

    /// Returns the total bytes written.
    #[must_use]
    pub const fn total_bytes(&self) -> u64 {
        self.bytes_overlapped + self.bytes_direct
    }

    /// Returns the overlap ratio (0.0 to 1.0).
    #[must_use]
    pub fn overlap_ratio(&self) -> f64 {
        let total = self.total_files();
        if total == 0 {
            return 0.0;
        }
        self.files_overlapped as f64 / total as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::flist::FileEntryBuilder;
    use std::fs;
    use tempfile::TempDir;

    fn make_file_entry(name: &str, size: u64) -> FileEntry {
        FileEntryBuilder::new(name.into())
            .with_size(size)
            .with_mode(0o100644) // Regular file
            .build()
    }

    #[test]
    fn test_overlapped_config_defaults() {
        let config = OverlappedConfig::default();
        assert_eq!(config.overlap_depth, 2);
        assert_eq!(config.max_buffer_size, 64 * 1024 * 1024);
        assert!(!config.do_fsync);
    }

    #[test]
    fn test_overlapped_config_synchronous() {
        let config = OverlappedConfig::synchronous();
        assert_eq!(config.overlap_depth, 0);
        assert_eq!(config.max_buffer_size, 0);
    }

    #[test]
    fn test_overlapped_writer_basic() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("test.txt");
        let temp_path = temp_dir.path().join("test.txt.tmp");

        let config = OverlappedConfig::default();
        let metadata_opts = MetadataOptions::new();
        let file_list = vec![make_file_entry("test.txt", 100)];

        let mut writer = OverlappedWriter::new(config, metadata_opts, file_list);

        let completed = CompletedFile {
            file_path: file_path.clone(),
            temp_path,
            data: b"Hello, World!".to_vec(),
            file_entry_index: 0,
            bytes_received: 13,
        };

        writer.queue_write(completed).unwrap();

        // Wait for result
        let result = writer.wait_for_result().unwrap();
        assert!(result.error.is_none());
        assert_eq!(result.bytes_written, 13);

        // Verify file contents
        let contents = fs::read_to_string(&file_path).unwrap();
        assert_eq!(contents, "Hello, World!");

        // Shutdown
        let remaining = writer.shutdown().unwrap();
        assert!(remaining.is_empty());
    }

    #[test]
    fn test_overlapped_writer_multiple_files() {
        let temp_dir = TempDir::new().unwrap();

        let config = OverlappedConfig::default().with_overlap_depth(4);
        let metadata_opts = MetadataOptions::new();
        let file_list: Vec<FileEntry> = (0..5)
            .map(|i| make_file_entry(&format!("file{i}.txt"), 100))
            .collect();

        let mut writer = OverlappedWriter::new(config, metadata_opts, file_list);

        // Queue multiple files
        for i in 0..5 {
            let file_path = temp_dir.path().join(format!("file{i}.txt"));
            let temp_path = temp_dir.path().join(format!("file{i}.txt.tmp"));
            let data = format!("Content of file {i}");

            let completed = CompletedFile {
                file_path,
                temp_path,
                data: data.into_bytes(),
                file_entry_index: i,
                bytes_received: 20,
            };

            writer.queue_write(completed).unwrap();
        }

        // Collect all results
        let mut total_bytes = 0u64;
        for _ in 0..5 {
            let result = writer.wait_for_result().unwrap();
            assert!(result.error.is_none(), "Error: {:?}", result.error);
            total_bytes += result.bytes_written;
        }

        assert_eq!(total_bytes, 5 * 18); // "Content of file N" = 18 bytes

        // Shutdown
        writer.shutdown().unwrap();
    }

    #[test]
    fn test_should_buffer() {
        let config = OverlappedConfig::default()
            .with_overlap_depth(2)
            .with_max_buffer_size(1024);
        let metadata_opts = MetadataOptions::new();
        let file_list = vec![];

        let writer = OverlappedWriter::new(config, metadata_opts, file_list);

        assert!(writer.should_buffer(100));
        assert!(writer.should_buffer(1024));
        assert!(!writer.should_buffer(1025));
        assert!(!writer.should_buffer(1_000_000));
    }

    #[test]
    fn test_overlapped_stats() {
        let mut stats = OverlappedStats::default();
        stats.files_overlapped = 8;
        stats.files_direct = 2;
        stats.bytes_overlapped = 8000;
        stats.bytes_direct = 20000;

        assert_eq!(stats.total_files(), 10);
        assert_eq!(stats.total_bytes(), 28000);
        assert!((stats.overlap_ratio() - 0.8).abs() < 0.001);
    }

    #[test]
    fn test_overlapped_writer_disabled() {
        let config = OverlappedConfig::synchronous();
        let metadata_opts = MetadataOptions::new();
        let file_list = vec![];

        let writer = OverlappedWriter::new(config, metadata_opts, file_list);
        assert!(!writer.is_enabled());
        assert!(!writer.should_buffer(100));
    }
}
