//! crates/engine/src/async_io.rs
//!
//! Async file I/O operations for the engine crate.
//!
//! This module provides tokio-based async alternatives to synchronous file
//! operations. It is only available when the `async` feature is enabled.
//!
//! # Features
//!
//! - Async file reading and writing with configurable buffer sizes
//! - Async file copying with progress callbacks
//! - Async checksum computation using spawn_blocking for CPU-intensive work
//! - Async sparse file writing support
//!
//! # Example
//!
//! ```ignore
//! use engine::async_io::{AsyncFileCopier, CopyProgress};
//!
//! let copier = AsyncFileCopier::new()
//!     .with_buffer_size(64 * 1024)
//!     .with_progress(|progress| {
//!         println!("Copied {} bytes", progress.bytes_copied);
//!     });
//!
//! copier.copy_file(source, destination).await?;
//! ```

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::fs::{self, File, OpenOptions};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt, BufReader, BufWriter, SeekFrom};
use tokio::sync::Semaphore;
use tokio::task;

/// Default buffer size for async file operations (64 KB).
pub const DEFAULT_BUFFER_SIZE: usize = 64 * 1024;

/// Maximum number of concurrent file operations when using `copy_files`.
pub const DEFAULT_MAX_CONCURRENT: usize = 4;

/// Progress information for async file copy operations.
#[derive(Debug, Clone)]
pub struct CopyProgress {
    /// Total bytes copied so far.
    pub bytes_copied: u64,
    /// Total size of the source file.
    pub total_bytes: u64,
    /// Elapsed time since copy started.
    pub elapsed: Duration,
    /// Source file path.
    pub source: PathBuf,
    /// Destination file path.
    pub destination: PathBuf,
}

impl CopyProgress {
    /// Returns the copy progress as a percentage (0.0 to 100.0).
    #[must_use]
    pub fn percentage(&self) -> f64 {
        if self.total_bytes == 0 {
            100.0
        } else {
            (self.bytes_copied as f64 / self.total_bytes as f64) * 100.0
        }
    }

    /// Returns the current transfer rate in bytes per second.
    #[must_use]
    pub fn bytes_per_second(&self) -> f64 {
        let secs = self.elapsed.as_secs_f64();
        if secs > 0.0 {
            self.bytes_copied as f64 / secs
        } else {
            0.0
        }
    }
}

/// Result of an async file copy operation.
#[derive(Debug, Clone)]
pub struct CopyResult {
    /// Total bytes copied.
    pub bytes_copied: u64,
    /// Total time elapsed.
    pub elapsed: Duration,
    /// Source file path.
    pub source: PathBuf,
    /// Destination file path.
    pub destination: PathBuf,
}

/// Error type for async file operations.
#[derive(Debug, thiserror::Error)]
pub enum AsyncIoError {
    /// I/O error during file operation.
    #[error("I/O error at {path}: {source}")]
    Io {
        /// The path where the error occurred.
        path: PathBuf,
        /// The underlying I/O error.
        #[source]
        source: io::Error,
    },

    /// Error joining a spawned task.
    #[error("Task join error: {0}")]
    JoinError(#[from] task::JoinError),

    /// Operation was cancelled.
    #[error("Operation cancelled")]
    Cancelled,

    /// File not found.
    #[error("File not found: {0}")]
    NotFound(PathBuf),
}

impl AsyncIoError {
    /// Creates an I/O error with path context.
    pub fn io(path: impl Into<PathBuf>, source: io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }
}

/// Extension trait for mapping I/O results to [`AsyncIoError`] with path context.
///
/// This reduces boilerplate when converting `io::Result<T>` to `Result<T, AsyncIoError>`.
///
/// # Example
///
/// ```ignore
/// use engine::async_io::IoResultExt;
///
/// let content = tokio::fs::read(&path).await.with_path(&path)?;
/// ```
trait IoResultExt<T> {
    /// Maps an I/O error to [`AsyncIoError::Io`] with the given path.
    fn with_path(self, path: impl Into<PathBuf>) -> Result<T, AsyncIoError>;
}

impl<T> IoResultExt<T> for io::Result<T> {
    fn with_path(self, path: impl Into<PathBuf>) -> Result<T, AsyncIoError> {
        self.map_err(|e| AsyncIoError::io(path, e))
    }
}

/// Builder for async file copy operations.
#[derive(Debug, Clone)]
pub struct AsyncFileCopier {
    buffer_size: usize,
    preserve_permissions: bool,
    preserve_timestamps: bool,
    sparse_detection: bool,
    fsync: bool,
}

impl Default for AsyncFileCopier {
    fn default() -> Self {
        Self::new()
    }
}

impl AsyncFileCopier {
    /// Creates a new async file copier with default settings.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            buffer_size: DEFAULT_BUFFER_SIZE,
            preserve_permissions: true,
            preserve_timestamps: true,
            sparse_detection: false,
            fsync: false,
        }
    }

    /// Sets the buffer size for copy operations.
    #[must_use]
    pub fn with_buffer_size(mut self, size: usize) -> Self {
        self.buffer_size = size.max(4096);
        self
    }

    /// Enables or disables permission preservation.
    #[must_use]
    pub const fn preserve_permissions(mut self, preserve: bool) -> Self {
        self.preserve_permissions = preserve;
        self
    }

    /// Enables or disables timestamp preservation.
    #[must_use]
    pub const fn preserve_timestamps(mut self, preserve: bool) -> Self {
        self.preserve_timestamps = preserve;
        self
    }

    /// Enables or disables sparse file detection.
    #[must_use]
    pub const fn sparse_detection(mut self, enable: bool) -> Self {
        self.sparse_detection = enable;
        self
    }

    /// Enables or disables fsync after writes.
    #[must_use]
    pub const fn fsync(mut self, enable: bool) -> Self {
        self.fsync = enable;
        self
    }

    /// Copies a file asynchronously.
    ///
    /// # Errors
    ///
    /// Returns an error if the source file cannot be read or the destination
    /// cannot be written.
    pub async fn copy_file(
        &self,
        source: impl AsRef<Path>,
        destination: impl AsRef<Path>,
    ) -> Result<CopyResult, AsyncIoError> {
        self.copy_file_with_progress(source, destination, |_| {})
            .await
    }

    /// Copies a file asynchronously with progress reporting.
    ///
    /// The progress callback is called periodically during the copy operation.
    ///
    /// # Errors
    ///
    /// Returns an error if the source file cannot be read or the destination
    /// cannot be written.
    pub async fn copy_file_with_progress<F>(
        &self,
        source: impl AsRef<Path>,
        destination: impl AsRef<Path>,
        mut progress: F,
    ) -> Result<CopyResult, AsyncIoError>
    where
        F: FnMut(CopyProgress),
    {
        let source = source.as_ref();
        let destination = destination.as_ref();
        let start = Instant::now();

        // Get source metadata
        let metadata = fs::metadata(source)
            .await
            .with_path(source)?;

        let total_bytes = metadata.len();

        // Open source file
        let src_file = File::open(source)
            .await
            .with_path(source)?;

        let mut reader = BufReader::with_capacity(self.buffer_size, src_file);

        // Create destination file
        let dest_file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(destination)
            .await
            .with_path(destination)?;

        let mut writer = BufWriter::with_capacity(self.buffer_size, dest_file);

        // Copy with progress
        let mut buffer = vec![0u8; self.buffer_size];
        let mut bytes_copied: u64 = 0;
        let mut last_progress = Instant::now();

        loop {
            let n = reader
                .read(&mut buffer)
                .await
                .with_path(source)?;

            if n == 0 {
                break;
            }

            let chunk = &buffer[..n];

            // Sparse detection: skip all-zero chunks
            if self.sparse_detection && is_all_zeros(chunk) {
                writer
                    .seek(SeekFrom::Current(n as i64))
                    .await
                    .with_path(destination)?;
            } else {
                writer
                    .write_all(chunk)
                    .await
                    .with_path(destination)?;
            }

            bytes_copied += n as u64;

            // Report progress at most every 100ms
            if last_progress.elapsed() >= Duration::from_millis(100) {
                progress(CopyProgress {
                    bytes_copied,
                    total_bytes,
                    elapsed: start.elapsed(),
                    source: source.to_path_buf(),
                    destination: destination.to_path_buf(),
                });
                last_progress = Instant::now();
            }
        }

        // Flush and optionally sync
        writer
            .flush()
            .await
            .with_path(destination)?;

        if self.fsync {
            writer
                .get_mut()
                .sync_all()
                .await
                .with_path(destination)?;
        }

        // Preserve metadata
        if self.preserve_permissions || self.preserve_timestamps {
            // Use spawn_blocking for metadata operations
            let dest_path = destination.to_path_buf();
            let perms = self.preserve_permissions.then(|| metadata.permissions());
            let mtime = self.preserve_timestamps.then(|| metadata.modified().ok());

            task::spawn_blocking(move || {
                if let Some(perms) = perms {
                    let _ = std::fs::set_permissions(&dest_path, perms);
                }
                if let Some(Some(mtime)) = mtime {
                    let _ = filetime::set_file_mtime(
                        &dest_path,
                        filetime::FileTime::from_system_time(mtime),
                    );
                }
            })
            .await?;
        }

        // Final progress report
        progress(CopyProgress {
            bytes_copied,
            total_bytes,
            elapsed: start.elapsed(),
            source: source.to_path_buf(),
            destination: destination.to_path_buf(),
        });

        Ok(CopyResult {
            bytes_copied,
            elapsed: start.elapsed(),
            source: source.to_path_buf(),
            destination: destination.to_path_buf(),
        })
    }
}

/// Batch async file copier for multiple files.
#[derive(Debug)]
pub struct AsyncBatchCopier {
    copier: AsyncFileCopier,
    max_concurrent: usize,
}

impl Default for AsyncBatchCopier {
    fn default() -> Self {
        Self::new()
    }
}

impl AsyncBatchCopier {
    /// Creates a new batch copier with default settings.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            copier: AsyncFileCopier::new(),
            max_concurrent: DEFAULT_MAX_CONCURRENT,
        }
    }

    /// Sets the maximum number of concurrent copy operations.
    #[must_use]
    pub fn max_concurrent(mut self, max: usize) -> Self {
        self.max_concurrent = max.max(1);
        self
    }

    /// Sets the underlying file copier configuration.
    #[must_use]
    pub const fn with_copier(mut self, copier: AsyncFileCopier) -> Self {
        self.copier = copier;
        self
    }

    /// Copies multiple files concurrently.
    ///
    /// Returns a vector of results for each file pair.
    ///
    /// # Arguments
    ///
    /// * `files` - Iterator of (source, destination) path pairs
    ///
    /// # Errors
    ///
    /// Individual file errors are returned in the result vector.
    pub async fn copy_files<I, P, Q>(&self, files: I) -> Vec<Result<CopyResult, AsyncIoError>>
    where
        I: IntoIterator<Item = (P, Q)>,
        P: AsRef<Path> + Send + 'static,
        Q: AsRef<Path> + Send + 'static,
    {
        let semaphore = Arc::new(Semaphore::new(self.max_concurrent));
        let copier = Arc::new(self.copier.clone());

        let tasks: Vec<_> = files
            .into_iter()
            .map(|(src, dst)| {
                let permit = semaphore.clone();
                let copier = copier.clone();
                let src = src.as_ref().to_path_buf();
                let dst = dst.as_ref().to_path_buf();

                tokio::spawn(async move {
                    // The semaphore is created locally and only dropped after all tasks complete,
                    // so `acquire()` can only fail if the semaphore is closed, which cannot happen.
                    let _permit = permit
                        .acquire()
                        .await
                        .expect("semaphore closed unexpectedly");
                    copier.copy_file(&src, &dst).await
                })
            })
            .collect();

        let mut results = Vec::with_capacity(tasks.len());
        for task in tasks {
            match task.await {
                Ok(result) => results.push(result),
                Err(e) => results.push(Err(AsyncIoError::JoinError(e))),
            }
        }

        results
    }
}

/// Async file reader with configurable buffering.
#[derive(Debug)]
pub struct AsyncFileReader {
    reader: BufReader<File>,
    path: PathBuf,
    position: u64,
    size: u64,
}

impl AsyncFileReader {
    /// Opens a file for async reading.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be opened.
    pub async fn open(path: impl AsRef<Path>) -> Result<Self, AsyncIoError> {
        Self::open_with_buffer_size(path, DEFAULT_BUFFER_SIZE).await
    }

    /// Opens a file for async reading with a custom buffer size.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be opened.
    pub async fn open_with_buffer_size(
        path: impl AsRef<Path>,
        buffer_size: usize,
    ) -> Result<Self, AsyncIoError> {
        let path = path.as_ref();
        let file = File::open(path)
            .await
            .with_path(path)?;

        let metadata = file
            .metadata()
            .await
            .with_path(path)?;

        Ok(Self {
            reader: BufReader::with_capacity(buffer_size, file),
            path: path.to_path_buf(),
            position: 0,
            size: metadata.len(),
        })
    }

    /// Returns the file path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Returns the current read position.
    #[must_use]
    pub const fn position(&self) -> u64 {
        self.position
    }

    /// Returns the total file size.
    #[must_use]
    pub const fn size(&self) -> u64 {
        self.size
    }

    /// Returns the number of remaining bytes.
    #[must_use]
    pub const fn remaining(&self) -> u64 {
        self.size.saturating_sub(self.position)
    }

    /// Reads up to `buf.len()` bytes into the buffer.
    ///
    /// # Errors
    ///
    /// Returns an error if reading fails.
    pub async fn read(&mut self, buf: &mut [u8]) -> Result<usize, AsyncIoError> {
        let n = self
            .reader
            .read(buf)
            .await
            .with_path(&self.path)?;
        self.position += n as u64;
        Ok(n)
    }

    /// Seeks to a position in the file.
    ///
    /// # Errors
    ///
    /// Returns an error if seeking fails.
    pub async fn seek(&mut self, pos: SeekFrom) -> Result<u64, AsyncIoError> {
        let new_pos = self
            .reader
            .seek(pos)
            .await
            .with_path(&self.path)?;
        self.position = new_pos;
        Ok(new_pos)
    }

    /// Reads the entire file into a vector.
    ///
    /// # Errors
    ///
    /// Returns an error if reading fails.
    pub async fn read_to_end(&mut self) -> Result<Vec<u8>, AsyncIoError> {
        let mut buf = Vec::with_capacity(self.remaining() as usize);
        self.reader
            .read_to_end(&mut buf)
            .await
            .with_path(&self.path)?;
        self.position = self.size;
        Ok(buf)
    }
}

/// Async file writer with configurable buffering.
#[derive(Debug)]
pub struct AsyncFileWriter {
    writer: BufWriter<File>,
    path: PathBuf,
    bytes_written: u64,
}

impl AsyncFileWriter {
    /// Creates a new file for async writing.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be created.
    pub async fn create(path: impl AsRef<Path>) -> Result<Self, AsyncIoError> {
        Self::create_with_buffer_size(path, DEFAULT_BUFFER_SIZE).await
    }

    /// Creates a new file for async writing with a custom buffer size.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be created.
    pub async fn create_with_buffer_size(
        path: impl AsRef<Path>,
        buffer_size: usize,
    ) -> Result<Self, AsyncIoError> {
        let path = path.as_ref();
        let file = File::create(path)
            .await
            .with_path(path)?;

        Ok(Self {
            writer: BufWriter::with_capacity(buffer_size, file),
            path: path.to_path_buf(),
            bytes_written: 0,
        })
    }

    /// Opens an existing file for async writing (append mode).
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be opened.
    pub async fn open_append(path: impl AsRef<Path>) -> Result<Self, AsyncIoError> {
        let path = path.as_ref();
        let file = OpenOptions::new()
            .write(true)
            .append(true)
            .open(path)
            .await
            .with_path(path)?;

        Ok(Self {
            writer: BufWriter::with_capacity(DEFAULT_BUFFER_SIZE, file),
            path: path.to_path_buf(),
            bytes_written: 0,
        })
    }

    /// Returns the file path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Returns the total bytes written.
    #[must_use]
    pub const fn bytes_written(&self) -> u64 {
        self.bytes_written
    }

    /// Writes bytes to the file.
    ///
    /// # Errors
    ///
    /// Returns an error if writing fails.
    pub async fn write(&mut self, buf: &[u8]) -> Result<usize, AsyncIoError> {
        let n = self
            .writer
            .write(buf)
            .await
            .with_path(&self.path)?;
        self.bytes_written += n as u64;
        Ok(n)
    }

    /// Writes all bytes to the file.
    ///
    /// # Errors
    ///
    /// Returns an error if writing fails.
    pub async fn write_all(&mut self, buf: &[u8]) -> Result<(), AsyncIoError> {
        self.writer
            .write_all(buf)
            .await
            .with_path(&self.path)?;
        self.bytes_written += buf.len() as u64;
        Ok(())
    }

    /// Flushes buffered data to the file.
    ///
    /// # Errors
    ///
    /// Returns an error if flushing fails.
    pub async fn flush(&mut self) -> Result<(), AsyncIoError> {
        self.writer
            .flush()
            .await
            .with_path(&self.path)
    }

    /// Syncs data to disk.
    ///
    /// # Errors
    ///
    /// Returns an error if syncing fails.
    pub async fn sync_all(&mut self) -> Result<(), AsyncIoError> {
        self.flush().await?;
        self.writer
            .get_mut()
            .sync_all()
            .await
            .with_path(&self.path)
    }

    /// Seeks to a position in the file.
    ///
    /// # Errors
    ///
    /// Returns an error if seeking fails.
    pub async fn seek(&mut self, pos: SeekFrom) -> Result<u64, AsyncIoError> {
        self.flush().await?;
        self.writer
            .seek(pos)
            .await
            .with_path(&self.path)
    }
}

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
            let n = file
                .read(&mut buffer)
                .with_path(&path)?;
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

/// Checks if a buffer contains only zeros.
#[inline]
fn is_all_zeros(buf: &[u8]) -> bool {
    // Use chunks for efficient comparison without unsafe
    buf.chunks(16).all(|chunk| chunk.iter().all(|&b| b == 0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_async_file_copy() {
        let temp = TempDir::new().unwrap();
        let src = temp.path().join("source.txt");
        let dst = temp.path().join("dest.txt");

        std::fs::write(&src, b"Hello, async world!").unwrap();

        let copier = AsyncFileCopier::new();
        let result = copier.copy_file(&src, &dst).await.unwrap();

        assert_eq!(result.bytes_copied, 19);
        assert_eq!(
            std::fs::read_to_string(&dst).unwrap(),
            "Hello, async world!"
        );
    }

    #[tokio::test]
    async fn test_async_file_copy_with_progress() {
        let temp = TempDir::new().unwrap();
        let src = temp.path().join("source.txt");
        let dst = temp.path().join("dest.txt");

        let data = vec![b'x'; 1024 * 100]; // 100 KB
        std::fs::write(&src, &data).unwrap();

        let mut progress_calls = 0u32;
        let copier = AsyncFileCopier::new().with_buffer_size(8192);

        let result = copier
            .copy_file_with_progress(&src, &dst, |_progress| {
                progress_calls += 1;
            })
            .await
            .unwrap();

        assert_eq!(result.bytes_copied, 100 * 1024);
        assert!(progress_calls > 0);
    }

    #[tokio::test]
    async fn test_async_file_reader() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("test.txt");
        std::fs::write(&path, b"Test content").unwrap();

        let mut reader = AsyncFileReader::open(&path).await.unwrap();

        assert_eq!(reader.size(), 12);
        assert_eq!(reader.position(), 0);

        let data = reader.read_to_end().await.unwrap();
        assert_eq!(data, b"Test content");
        assert_eq!(reader.position(), 12);
    }

    #[tokio::test]
    async fn test_async_file_writer() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("output.txt");

        let mut writer = AsyncFileWriter::create(&path).await.unwrap();
        writer.write_all(b"Hello, ").await.unwrap();
        writer.write_all(b"World!").await.unwrap();
        writer.flush().await.unwrap();

        assert_eq!(writer.bytes_written(), 13);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "Hello, World!");
    }

    #[tokio::test]
    async fn test_sparse_detection() {
        let temp = TempDir::new().unwrap();
        let src = temp.path().join("sparse_src.bin");
        let dst = temp.path().join("sparse_dst.bin");

        // Create file with zeros
        let mut data = vec![0u8; 64 * 1024];
        data[0..4].copy_from_slice(b"HEAD");
        data[64 * 1024 - 4..].copy_from_slice(b"TAIL");
        std::fs::write(&src, &data).unwrap();

        let copier = AsyncFileCopier::new().sparse_detection(true);
        copier.copy_file(&src, &dst).await.unwrap();

        let result = std::fs::read(&dst).unwrap();
        assert_eq!(&result[0..4], b"HEAD");
        assert_eq!(&result[64 * 1024 - 4..], b"TAIL");
    }

    #[tokio::test]
    async fn test_batch_copy() {
        let temp = TempDir::new().unwrap();

        // Create source files
        let files: Vec<_> = (0..5)
            .map(|i| {
                let src = temp.path().join(format!("src_{i}.txt"));
                let dst = temp.path().join(format!("dst_{i}.txt"));
                std::fs::write(&src, format!("Content {i}")).unwrap();
                (src, dst)
            })
            .collect();

        let batch_copier = AsyncBatchCopier::new().max_concurrent(2);
        let results = batch_copier.copy_files(files.clone()).await;

        assert_eq!(results.len(), 5);
        for (i, result) in results.iter().enumerate() {
            assert!(result.is_ok(), "File {i} should copy successfully");
        }

        // Verify all files were copied
        for i in 0..5 {
            let content =
                std::fs::read_to_string(temp.path().join(format!("dst_{i}.txt"))).unwrap();
            assert_eq!(content, format!("Content {i}"));
        }
    }

    #[test]
    fn test_is_all_zeros() {
        assert!(is_all_zeros(&[0; 100]));
        assert!(!is_all_zeros(&[0, 0, 1, 0]));
        assert!(is_all_zeros(&[]));
        assert!(!is_all_zeros(&[1]));
    }

    #[test]
    fn test_copy_progress_percentage() {
        let progress = CopyProgress {
            bytes_copied: 50,
            total_bytes: 100,
            elapsed: Duration::from_secs(1),
            source: PathBuf::from("src"),
            destination: PathBuf::from("dst"),
        };

        assert!((progress.percentage() - 50.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_copy_progress_bytes_per_second() {
        let progress = CopyProgress {
            bytes_copied: 1000,
            total_bytes: 2000,
            elapsed: Duration::from_secs(2),
            source: PathBuf::from("src"),
            destination: PathBuf::from("dst"),
        };

        assert!((progress.bytes_per_second() - 500.0).abs() < f64::EPSILON);
    }

    // Additional CopyProgress tests
    #[test]
    fn test_copy_progress_percentage_zero_total() {
        let progress = CopyProgress {
            bytes_copied: 0,
            total_bytes: 0,
            elapsed: Duration::from_secs(1),
            source: PathBuf::from("src"),
            destination: PathBuf::from("dst"),
        };

        assert!((progress.percentage() - 100.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_copy_progress_bytes_per_second_zero_elapsed() {
        let progress = CopyProgress {
            bytes_copied: 1000,
            total_bytes: 2000,
            elapsed: Duration::from_secs(0),
            source: PathBuf::from("src"),
            destination: PathBuf::from("dst"),
        };

        assert!((progress.bytes_per_second() - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_copy_progress_clone() {
        let progress = CopyProgress {
            bytes_copied: 100,
            total_bytes: 200,
            elapsed: Duration::from_secs(1),
            source: PathBuf::from("src"),
            destination: PathBuf::from("dst"),
        };
        let cloned = progress.clone();
        assert_eq!(cloned.bytes_copied, progress.bytes_copied);
        assert_eq!(cloned.total_bytes, progress.total_bytes);
    }

    #[test]
    fn test_copy_progress_debug() {
        let progress = CopyProgress {
            bytes_copied: 100,
            total_bytes: 200,
            elapsed: Duration::from_secs(1),
            source: PathBuf::from("src"),
            destination: PathBuf::from("dst"),
        };
        let debug = format!("{progress:?}");
        assert!(debug.contains("CopyProgress"));
    }

    // CopyResult tests
    #[test]
    fn test_copy_result_clone() {
        let result = CopyResult {
            bytes_copied: 100,
            elapsed: Duration::from_secs(1),
            source: PathBuf::from("src"),
            destination: PathBuf::from("dst"),
        };
        let cloned = result.clone();
        assert_eq!(cloned.bytes_copied, result.bytes_copied);
    }

    #[test]
    fn test_copy_result_debug() {
        let result = CopyResult {
            bytes_copied: 100,
            elapsed: Duration::from_secs(1),
            source: PathBuf::from("src"),
            destination: PathBuf::from("dst"),
        };
        let debug = format!("{result:?}");
        assert!(debug.contains("CopyResult"));
    }

    // AsyncIoError tests
    #[test]
    fn test_async_io_error_io() {
        let error = AsyncIoError::io(
            "/path/to/file",
            io::Error::new(io::ErrorKind::NotFound, "not found"),
        );
        let display = format!("{error}");
        assert!(display.contains("/path/to/file"));
    }

    #[test]
    fn test_async_io_error_cancelled() {
        let error = AsyncIoError::Cancelled;
        let display = format!("{error}");
        assert!(display.contains("cancelled"));
    }

    #[test]
    fn test_async_io_error_not_found() {
        let error = AsyncIoError::NotFound(PathBuf::from("/missing/file"));
        let display = format!("{error}");
        assert!(display.contains("/missing/file"));
    }

    // AsyncFileCopier builder tests
    #[test]
    fn test_async_file_copier_default() {
        let copier = AsyncFileCopier::default();
        assert_eq!(copier.buffer_size, DEFAULT_BUFFER_SIZE);
    }

    #[test]
    fn test_async_file_copier_builder_chain() {
        let copier = AsyncFileCopier::new()
            .with_buffer_size(8192)
            .preserve_permissions(false)
            .preserve_timestamps(false)
            .sparse_detection(true)
            .fsync(true);
        assert_eq!(copier.buffer_size, 8192);
        assert!(!copier.preserve_permissions);
        assert!(!copier.preserve_timestamps);
        assert!(copier.sparse_detection);
        assert!(copier.fsync);
    }

    #[test]
    fn test_async_file_copier_buffer_size_min() {
        let copier = AsyncFileCopier::new().with_buffer_size(1);
        // Buffer size should be at least 4096
        assert_eq!(copier.buffer_size, 4096);
    }

    #[test]
    fn test_async_file_copier_clone() {
        let copier = AsyncFileCopier::new().with_buffer_size(8192);
        let cloned = copier.clone();
        assert_eq!(copier.buffer_size, cloned.buffer_size);
    }

    #[test]
    fn test_async_file_copier_debug() {
        let copier = AsyncFileCopier::new();
        let debug = format!("{copier:?}");
        assert!(debug.contains("AsyncFileCopier"));
    }

    // AsyncBatchCopier tests
    #[test]
    fn test_async_batch_copier_default() {
        let copier = AsyncBatchCopier::default();
        assert_eq!(copier.max_concurrent, DEFAULT_MAX_CONCURRENT);
    }

    #[test]
    fn test_async_batch_copier_max_concurrent() {
        let copier = AsyncBatchCopier::new().max_concurrent(8);
        assert_eq!(copier.max_concurrent, 8);
    }

    #[test]
    fn test_async_batch_copier_max_concurrent_min() {
        let copier = AsyncBatchCopier::new().max_concurrent(0);
        // Should be at least 1
        assert_eq!(copier.max_concurrent, 1);
    }

    #[test]
    fn test_async_batch_copier_with_copier() {
        let file_copier = AsyncFileCopier::new().with_buffer_size(16384);
        let batch = AsyncBatchCopier::new().with_copier(file_copier);
        assert_eq!(batch.copier.buffer_size, 16384);
    }

    #[test]
    fn test_async_batch_copier_debug() {
        let copier = AsyncBatchCopier::new();
        let debug = format!("{copier:?}");
        assert!(debug.contains("AsyncBatchCopier"));
    }

    // AsyncFileReader tests
    #[tokio::test]
    async fn test_async_file_reader_remaining() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("test.txt");
        std::fs::write(&path, b"Hello, World!").unwrap();

        let reader = AsyncFileReader::open(&path).await.unwrap();
        assert_eq!(reader.remaining(), 13);
        assert_eq!(reader.path(), path.as_path());
    }

    #[tokio::test]
    async fn test_async_file_reader_with_buffer_size() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("test.txt");
        std::fs::write(&path, b"Hello").unwrap();

        let reader = AsyncFileReader::open_with_buffer_size(&path, 1024)
            .await
            .unwrap();
        assert_eq!(reader.size(), 5);
    }

    #[tokio::test]
    async fn test_async_file_reader_seek() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("test.txt");
        std::fs::write(&path, b"Hello, World!").unwrap();

        let mut reader = AsyncFileReader::open(&path).await.unwrap();
        let pos = reader.seek(SeekFrom::Start(7)).await.unwrap();
        assert_eq!(pos, 7);
        assert_eq!(reader.position(), 7);
    }

    #[tokio::test]
    async fn test_async_file_reader_read() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("test.txt");
        std::fs::write(&path, b"Hello").unwrap();

        let mut reader = AsyncFileReader::open(&path).await.unwrap();
        let mut buf = [0u8; 3];
        let n = reader.read(&mut buf).await.unwrap();
        assert_eq!(n, 3);
        assert_eq!(&buf, b"Hel");
        assert_eq!(reader.position(), 3);
    }

    // AsyncFileWriter tests
    #[tokio::test]
    async fn test_async_file_writer_path() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("output.txt");

        let writer = AsyncFileWriter::create(&path).await.unwrap();
        assert_eq!(writer.path(), path.as_path());
        assert_eq!(writer.bytes_written(), 0);
    }

    #[tokio::test]
    async fn test_async_file_writer_with_buffer_size() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("output.txt");

        let mut writer = AsyncFileWriter::create_with_buffer_size(&path, 8192)
            .await
            .unwrap();
        writer.write_all(b"test").await.unwrap();
        writer.flush().await.unwrap();
        assert_eq!(writer.bytes_written(), 4);
    }

    #[tokio::test]
    async fn test_async_file_writer_write() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("output.txt");

        let mut writer = AsyncFileWriter::create(&path).await.unwrap();
        let n = writer.write(b"Hello").await.unwrap();
        assert_eq!(n, 5);
        writer.flush().await.unwrap();
        assert_eq!(writer.bytes_written(), 5);
    }

    #[tokio::test]
    async fn test_async_file_writer_sync_all() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("output.txt");

        let mut writer = AsyncFileWriter::create(&path).await.unwrap();
        writer.write_all(b"Hello").await.unwrap();
        writer.sync_all().await.unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "Hello");
    }

    #[tokio::test]
    async fn test_async_file_writer_seek() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("output.txt");

        let mut writer = AsyncFileWriter::create(&path).await.unwrap();
        writer.write_all(b"Hello, World!").await.unwrap();
        let pos = writer.seek(SeekFrom::Start(7)).await.unwrap();
        assert_eq!(pos, 7);
        writer.write_all(b"Rust!!").await.unwrap();
        writer.flush().await.unwrap();
        // "World!" is 6 chars, "Rust!!" is 6 chars, so file length stays the same
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "Hello, Rust!!");
    }

    // ChecksumAlgorithm tests
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

    // is_all_zeros additional tests
    #[test]
    fn test_is_all_zeros_large_buffer() {
        assert!(is_all_zeros(&vec![0u8; 4096]));
    }

    #[test]
    fn test_is_all_zeros_last_byte_nonzero() {
        let mut buf = vec![0u8; 100];
        buf[99] = 1;
        assert!(!is_all_zeros(&buf));
    }

    #[test]
    fn test_is_all_zeros_first_byte_nonzero() {
        let mut buf = vec![0u8; 100];
        buf[0] = 1;
        assert!(!is_all_zeros(&buf));
    }

    #[test]
    fn test_is_all_zeros_middle_byte_nonzero() {
        let mut buf = vec![0u8; 100];
        buf[50] = 255;
        assert!(!is_all_zeros(&buf));
    }
}
