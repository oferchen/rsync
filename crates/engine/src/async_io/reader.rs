//! Async file reader with configurable buffering.

use std::path::{Path, PathBuf};

use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncSeekExt, BufReader, SeekFrom};

use super::DEFAULT_BUFFER_SIZE;
use super::error::{AsyncIoError, IoResultExt};

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
        let file = File::open(path).await.with_path(path)?;

        let metadata = file.metadata().await.with_path(path)?;

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
        let n = self.reader.read(buf).await.with_path(&self.path)?;
        self.position += n as u64;
        Ok(n)
    }

    /// Seeks to a position in the file.
    ///
    /// # Errors
    ///
    /// Returns an error if seeking fails.
    pub async fn seek(&mut self, pos: SeekFrom) -> Result<u64, AsyncIoError> {
        let new_pos = self.reader.seek(pos).await.with_path(&self.path)?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

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
}
