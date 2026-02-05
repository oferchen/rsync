//! Core traits for file I/O abstraction.
//!
//! These traits allow swapping implementations (standard, mmap, io_uring)
//! without changing application code.

use std::io::{self, Read, Write};
use std::path::Path;

/// A reader that can read file contents efficiently.
///
/// Implementations may use standard I/O, memory mapping, or io_uring.
pub trait FileReader: Read {
    /// Returns the total size of the file in bytes.
    fn size(&self) -> u64;

    /// Returns the current read position.
    fn position(&self) -> u64;

    /// Seeks to a position in the file.
    fn seek_to(&mut self, pos: u64) -> io::Result<()>;

    /// Returns the remaining bytes to read.
    fn remaining(&self) -> u64 {
        self.size().saturating_sub(self.position())
    }

    /// Reads the entire file into a vector.
    fn read_all(&mut self) -> io::Result<Vec<u8>> {
        let mut buf = Vec::with_capacity(self.remaining() as usize);
        self.read_to_end(&mut buf)?;
        Ok(buf)
    }
}

/// A writer that can write file contents efficiently.
///
/// Implementations may use standard I/O, pre-allocated files, or io_uring.
pub trait FileWriter: Write {
    /// Returns the number of bytes written so far.
    fn bytes_written(&self) -> u64;

    /// Syncs the file to disk.
    fn sync(&mut self) -> io::Result<()>;

    /// Pre-allocates space for the file (advisory).
    fn preallocate(&mut self, _size: u64) -> io::Result<()> {
        Ok(()) // Default: no-op
    }
}

/// Factory for creating file readers.
pub trait FileReaderFactory: Send + Sync {
    /// The reader type produced by this factory.
    type Reader: FileReader + Send;

    /// Opens a file for reading.
    fn open(&self, path: &Path) -> io::Result<Self::Reader>;
}

/// Factory for creating file writers.
pub trait FileWriterFactory: Send + Sync {
    /// The writer type produced by this factory.
    type Writer: FileWriter + Send;

    /// Creates a file for writing.
    fn create(&self, path: &Path) -> io::Result<Self::Writer>;

    /// Creates a file with pre-allocated space.
    fn create_with_size(&self, path: &Path, size: u64) -> io::Result<Self::Writer>;
}

// ─────────────────────────────────────────────────────────────────────────────
// Standard I/O implementations
// ─────────────────────────────────────────────────────────────────────────────

use std::fs::File;
use std::io::{BufReader, BufWriter, Seek, SeekFrom};

/// Standard file reader using buffered I/O.
pub struct StdFileReader {
    inner: BufReader<File>,
    size: u64,
    position: u64,
}

impl StdFileReader {
    /// Opens a file for reading.
    pub fn open(path: &Path) -> io::Result<Self> {
        let file = File::open(path)?;
        let size = file.metadata()?.len();
        Ok(Self {
            inner: BufReader::new(file),
            size,
            position: 0,
        })
    }
}

impl Read for StdFileReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.position += n as u64;
        Ok(n)
    }
}

impl FileReader for StdFileReader {
    fn size(&self) -> u64 {
        self.size
    }

    fn position(&self) -> u64 {
        self.position
    }

    fn seek_to(&mut self, pos: u64) -> io::Result<()> {
        self.inner.seek(SeekFrom::Start(pos))?;
        self.position = pos;
        Ok(())
    }
}

/// Standard file writer using buffered I/O.
pub struct StdFileWriter {
    inner: BufWriter<File>,
    bytes_written: u64,
}

impl StdFileWriter {
    /// Creates a file for writing.
    pub fn create(path: &Path) -> io::Result<Self> {
        let file = File::create(path)?;
        Ok(Self {
            inner: BufWriter::new(file),
            bytes_written: 0,
        })
    }

    /// Creates a file with pre-allocated space.
    pub fn create_with_size(path: &Path, size: u64) -> io::Result<Self> {
        let file = File::create(path)?;
        file.set_len(size)?;
        Ok(Self {
            inner: BufWriter::new(file),
            bytes_written: 0,
        })
    }
}

impl Write for StdFileWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.bytes_written += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

impl FileWriter for StdFileWriter {
    fn bytes_written(&self) -> u64 {
        self.bytes_written
    }

    fn sync(&mut self) -> io::Result<()> {
        self.inner.flush()?;
        self.inner.get_ref().sync_all()
    }

    fn preallocate(&mut self, size: u64) -> io::Result<()> {
        self.inner.get_ref().set_len(size)
    }
}

/// Factory for standard file readers.
#[derive(Debug, Clone, Copy, Default)]
pub struct StdReaderFactory;

impl FileReaderFactory for StdReaderFactory {
    type Reader = StdFileReader;

    fn open(&self, path: &Path) -> io::Result<Self::Reader> {
        StdFileReader::open(path)
    }
}

/// Factory for standard file writers.
#[derive(Debug, Clone, Copy, Default)]
pub struct StdWriterFactory;

impl FileWriterFactory for StdWriterFactory {
    type Writer = StdFileWriter;

    fn create(&self, path: &Path) -> io::Result<Self::Writer> {
        StdFileWriter::create(path)
    }

    fn create_with_size(&self, path: &Path, size: u64) -> io::Result<Self::Writer> {
        StdFileWriter::create_with_size(path, size)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::tempdir;

    #[test]
    fn std_reader_tracks_position() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.txt");
        std::fs::write(&path, b"hello world").unwrap();

        let mut reader = StdFileReader::open(&path).unwrap();
        assert_eq!(reader.size(), 11);
        assert_eq!(reader.position(), 0);

        let mut buf = [0u8; 5];
        reader.read_exact(&mut buf).unwrap();
        assert_eq!(reader.position(), 5);
        assert_eq!(&buf, b"hello");
    }

    #[test]
    fn std_writer_tracks_bytes() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.txt");

        let mut writer = StdFileWriter::create(&path).unwrap();
        writer.write_all(b"hello").unwrap();
        assert_eq!(writer.bytes_written(), 5);

        writer.write_all(b" world").unwrap();
        assert_eq!(writer.bytes_written(), 11);
    }

    #[test]
    fn std_writer_flush_writes_buffered_data() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("flush_test.txt");

        let mut writer = StdFileWriter::create(&path).unwrap();

        // Write data that stays in the buffer
        writer.write_all(b"buffered data").unwrap();
        assert_eq!(writer.bytes_written(), 13);

        // Before flush, file might not contain all data (due to buffering)
        // After flush, all data must be written
        writer.flush().unwrap();

        // Verify all data is present after explicit flush
        drop(writer);
        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(content, "buffered data");
    }

    #[test]
    fn std_writer_auto_flush_on_buffer_full() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("auto_flush_test.txt");

        let mut writer = StdFileWriter::create(&path).unwrap();

        // BufWriter has a default capacity of 8KB
        // Write more than 8KB to trigger automatic flush
        let large_data = vec![b'x'; 16 * 1024]; // 16KB
        writer.write_all(&large_data).unwrap();

        assert_eq!(writer.bytes_written(), 16 * 1024);

        // Even without explicit flush, the buffer should have been auto-flushed
        // when it filled up. Let's verify by checking after a sync.
        writer.sync().unwrap();

        drop(writer);
        let content = std::fs::read(&path).unwrap();
        assert_eq!(content.len(), 16 * 1024);
        assert_eq!(content, large_data);
    }

    #[test]
    fn std_writer_flush_on_drop() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("drop_flush_test.txt");

        {
            let mut writer = StdFileWriter::create(&path).unwrap();
            writer.write_all(b"data written before drop").unwrap();
            // Drop happens here - should flush buffered data
        }

        // Verify data was written even without explicit flush
        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(content, "data written before drop");
    }

    #[test]
    fn std_writer_multiple_flush_operations() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("multiple_flush_test.txt");

        let mut writer = StdFileWriter::create(&path).unwrap();

        // Write and flush multiple times
        writer.write_all(b"first").unwrap();
        writer.flush().unwrap();

        writer.write_all(b" second").unwrap();
        writer.flush().unwrap();

        writer.write_all(b" third").unwrap();
        writer.flush().unwrap();

        assert_eq!(writer.bytes_written(), 18);

        drop(writer);
        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(content, "first second third");
    }

    #[test]
    fn std_writer_flush_empty_buffer() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("empty_flush_test.txt");

        let mut writer = StdFileWriter::create(&path).unwrap();

        // Flush with no data written should succeed
        writer.flush().unwrap();
        assert_eq!(writer.bytes_written(), 0);

        // Write something and verify
        writer.write_all(b"data").unwrap();
        writer.flush().unwrap();

        drop(writer);
        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(content, "data");
    }

    #[test]
    fn std_writer_sync_includes_flush() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("sync_test.txt");

        let mut writer = StdFileWriter::create(&path).unwrap();
        writer.write_all(b"sync test data").unwrap();

        // sync() should flush and sync to disk
        writer.sync().unwrap();

        // Data should be persisted
        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(content, "sync test data");
    }

    #[test]
    fn std_writer_flush_after_partial_writes() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("partial_writes_test.txt");

        let mut writer = StdFileWriter::create(&path).unwrap();

        // Write in small chunks
        for i in 0..10 {
            writer.write_all(format!("{i}").as_bytes()).unwrap();
        }

        assert_eq!(writer.bytes_written(), 10);
        writer.flush().unwrap();

        drop(writer);
        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(content, "0123456789");
    }

    #[test]
    fn std_writer_error_handling_invalid_path() {
        // Try to create a writer in a non-existent directory
        let result = StdFileWriter::create(Path::new("/nonexistent/dir/file.txt"));
        assert!(result.is_err());
    }

    #[test]
    fn std_writer_flush_consistency() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("consistency_test.txt");

        let mut writer = StdFileWriter::create(&path).unwrap();

        // Write, flush, write more, flush again
        writer.write_all(b"first batch").unwrap();
        writer.flush().unwrap();

        // Verify intermediate state
        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(content, "first batch");

        writer.write_all(b" second batch").unwrap();
        writer.flush().unwrap();

        // Verify final state
        drop(writer);
        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(content, "first batch second batch");
    }

    #[test]
    fn std_writer_bytes_written_accurate_across_flushes() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("bytes_tracking_test.txt");

        let mut writer = StdFileWriter::create(&path).unwrap();

        writer.write_all(b"12345").unwrap();
        assert_eq!(writer.bytes_written(), 5);
        writer.flush().unwrap();
        assert_eq!(writer.bytes_written(), 5);

        writer.write_all(b"67890").unwrap();
        assert_eq!(writer.bytes_written(), 10);
        writer.flush().unwrap();
        assert_eq!(writer.bytes_written(), 10);

        drop(writer);
        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(content, "1234567890");
    }
}
