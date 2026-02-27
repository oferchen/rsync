//! Async file writer with configurable buffering.

use std::path::{Path, PathBuf};

use tokio::fs::{File, OpenOptions};
use tokio::io::{AsyncSeekExt, AsyncWriteExt, BufWriter, SeekFrom};

use super::DEFAULT_BUFFER_SIZE;
use super::error::{AsyncIoError, IoResultExt};

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
        let file = File::create(path).await.with_path(path)?;

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
        let n = self.writer.write(buf).await.with_path(&self.path)?;
        self.bytes_written += n as u64;
        Ok(n)
    }

    /// Writes all bytes to the file.
    ///
    /// # Errors
    ///
    /// Returns an error if writing fails.
    pub async fn write_all(&mut self, buf: &[u8]) -> Result<(), AsyncIoError> {
        self.writer.write_all(buf).await.with_path(&self.path)?;
        self.bytes_written += buf.len() as u64;
        Ok(())
    }

    /// Flushes buffered data to the file.
    ///
    /// # Errors
    ///
    /// Returns an error if flushing fails.
    pub async fn flush(&mut self) -> Result<(), AsyncIoError> {
        self.writer.flush().await.with_path(&self.path)
    }

    /// Syncs data to disk.
    ///
    /// # Errors
    ///
    /// Returns an error if syncing fails.
    pub async fn sync_all(&mut self) -> Result<(), AsyncIoError> {
        self.flush().await?;
        self.writer.get_mut().sync_all().await.with_path(&self.path)
    }

    /// Seeks to a position in the file.
    ///
    /// # Errors
    ///
    /// Returns an error if seeking fails.
    pub async fn seek(&mut self, pos: SeekFrom) -> Result<u64, AsyncIoError> {
        self.flush().await?;
        self.writer.seek(pos).await.with_path(&self.path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

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
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "Hello, Rust!!");
    }
}
