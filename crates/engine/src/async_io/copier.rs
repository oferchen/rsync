//! Single-file async copy with progress reporting and sparse detection.

use std::path::Path;
use std::time::{Duration, Instant};

use tokio::fs::{self, File, OpenOptions};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt, BufReader, BufWriter, SeekFrom};
use tokio::task;

use super::DEFAULT_BUFFER_SIZE;
use super::error::{AsyncIoError, IoResultExt};
use super::progress::{CopyProgress, CopyResult};
use crate::local_copy::SparseDetector;

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

    /// Returns the configured buffer size.
    #[must_use]
    pub const fn buffer_size(&self) -> usize {
        self.buffer_size
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

        let metadata = fs::metadata(source).await.with_path(source)?;
        let total_bytes = metadata.len();

        let src_file = File::open(source).await.with_path(source)?;
        let mut reader = BufReader::with_capacity(self.buffer_size, src_file);

        let dest_file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(destination)
            .await
            .with_path(destination)?;

        let mut writer = BufWriter::with_capacity(self.buffer_size, dest_file);

        let mut buffer = vec![0u8; self.buffer_size];
        let mut bytes_copied: u64 = 0;
        let mut last_progress = Instant::now();

        loop {
            let n = reader.read(&mut buffer).await.with_path(source)?;

            if n == 0 {
                break;
            }

            let chunk = &buffer[..n];

            if self.sparse_detection && SparseDetector::is_all_zeros(chunk) {
                writer
                    .seek(SeekFrom::Current(n as i64))
                    .await
                    .with_path(destination)?;
            } else {
                writer.write_all(chunk).await.with_path(destination)?;
            }

            bytes_copied += n as u64;

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

        writer.flush().await.with_path(destination)?;

        if self.fsync {
            writer.get_mut().sync_all().await.with_path(destination)?;
        }

        if self.preserve_permissions || self.preserve_timestamps {
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

        let data = vec![b'x'; 1024 * 100];
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
    async fn test_sparse_detection() {
        let temp = TempDir::new().unwrap();
        let src = temp.path().join("sparse_src.bin");
        let dst = temp.path().join("sparse_dst.bin");

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

    #[test]
    fn test_async_file_copier_default() {
        let copier = AsyncFileCopier::default();
        assert_eq!(copier.buffer_size(), DEFAULT_BUFFER_SIZE);
    }

    #[test]
    fn test_async_file_copier_builder_chain() {
        let copier = AsyncFileCopier::new()
            .with_buffer_size(8192)
            .preserve_permissions(false)
            .preserve_timestamps(false)
            .sparse_detection(true)
            .fsync(true);
        assert_eq!(copier.buffer_size(), 8192);
        assert!(!copier.preserve_permissions);
        assert!(!copier.preserve_timestamps);
        assert!(copier.sparse_detection);
        assert!(copier.fsync);
    }

    #[test]
    fn test_async_file_copier_buffer_size_min() {
        let copier = AsyncFileCopier::new().with_buffer_size(1);
        assert_eq!(copier.buffer_size(), 4096);
    }

    #[test]
    fn test_async_file_copier_clone() {
        let copier = AsyncFileCopier::new().with_buffer_size(8192);
        let cloned = copier.clone();
        assert_eq!(copier.buffer_size(), cloned.buffer_size());
    }

    #[test]
    fn test_async_file_copier_debug() {
        let copier = AsyncFileCopier::new();
        let debug = format!("{copier:?}");
        assert!(debug.contains("AsyncFileCopier"));
    }
}
