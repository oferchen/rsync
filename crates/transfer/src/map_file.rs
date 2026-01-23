//! Memory-mapped file abstraction for basis file access.
//!
//! This module implements the `map_file`/`map_ptr` pattern from upstream rsync,
//! providing efficient cached access to basis files during delta application.
//!
//! # Problem
//!
//! During delta application, block references require reading from the basis
//! file. Naive implementation opens the file for each block, causing:
//! - 2000+ open/close syscalls for a typical 16MB file
//! - No kernel page cache reuse across blocks
//! - 60-70% of transfer latency
//!
//! # Solution
//!
//! The `MapFile` struct maintains a sliding window buffer (256KB by default)
//! over the basis file. Sequential block accesses hit the cache, and the
//! window slides forward as needed.
//!
//! # Upstream Reference
//!
//! See `fileio.c` in upstream rsync 3.4.1: `map_file()`, `map_ptr()`, `unmap_file()`.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;

use crate::constants::{MAX_MAP_SIZE, align_down};

/// Strategy trait for file mapping implementations.
///
/// This allows swapping between buffered and memory-mapped implementations
/// without changing the delta application code.
pub trait MapStrategy: Send {
    /// Returns a slice of file data at the given offset.
    ///
    /// The returned slice is valid until the next call to `map_ptr`.
    /// The implementation may cache data to avoid repeated reads.
    ///
    /// # Errors
    ///
    /// Returns an error if the read fails or the requested range
    /// is beyond the file size.
    fn map_ptr(&mut self, offset: u64, len: usize) -> io::Result<&[u8]>;

    /// Returns the size of the mapping window.
    fn window_size(&self) -> usize;

    /// Returns the total file size.
    fn file_size(&self) -> u64;
}

/// Buffered file mapper with sliding window cache.
///
/// Maintains a buffer of up to `MAX_MAP_SIZE` bytes and slides the window
/// as needed to serve read requests efficiently.
#[derive(Debug)]
pub struct BufferedMap {
    /// The underlying file handle.
    file: File,
    /// Total file size in bytes.
    size: u64,
    /// Cached data buffer.
    buffer: Vec<u8>,
    /// Starting offset of cached data in the file.
    window_start: u64,
    /// Number of valid bytes in the buffer.
    window_len: usize,
    /// Maximum window size (typically MAX_MAP_SIZE).
    max_window: usize,
}

impl BufferedMap {
    /// Opens a file for buffered mapping.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be opened or its size determined.
    pub fn open<P: AsRef<Path>>(path: P) -> io::Result<Self> {
        Self::open_with_window(path, MAX_MAP_SIZE)
    }

    /// Opens a file with a custom window size.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be opened or its size determined.
    pub fn open_with_window<P: AsRef<Path>>(path: P, window_size: usize) -> io::Result<Self> {
        let file = File::open(path)?;
        let size = file.metadata()?.len();

        Ok(Self {
            file,
            size,
            buffer: Vec::with_capacity(window_size),
            window_start: 0,
            window_len: 0,
            max_window: window_size,
        })
    }

    /// Creates a BufferedMap from an already-open file.
    ///
    /// # Errors
    ///
    /// Returns an error if the file size cannot be determined.
    pub fn from_file(file: File) -> io::Result<Self> {
        Self::from_file_with_window(file, MAX_MAP_SIZE)
    }

    /// Creates a BufferedMap from an already-open file with custom window.
    ///
    /// # Errors
    ///
    /// Returns an error if the file size cannot be determined.
    pub fn from_file_with_window(file: File, window_size: usize) -> io::Result<Self> {
        let size = file.metadata()?.len();

        Ok(Self {
            file,
            size,
            buffer: Vec::with_capacity(window_size),
            window_start: 0,
            window_len: 0,
            max_window: window_size,
        })
    }

    /// Returns true if the requested range is within the current window.
    #[inline]
    fn is_in_window(&self, offset: u64, len: usize) -> bool {
        offset >= self.window_start
            && offset.saturating_add(len as u64) <= self.window_start + self.window_len as u64
    }

    /// Loads a new window starting at the aligned offset.
    fn load_window(&mut self, offset: u64, min_len: usize) -> io::Result<()> {
        // Align the read position down to ALIGN_BOUNDARY
        let aligned_start = align_down(offset);

        // Calculate how much to read (up to max_window, but not past EOF)
        let remaining = self.size.saturating_sub(aligned_start);
        let read_size = (self.max_window as u64).min(remaining) as usize;

        // Ensure we read at least min_len bytes from the requested offset
        let offset_in_window = (offset - aligned_start) as usize;
        let required_size = offset_in_window + min_len;
        if read_size < required_size {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "requested range extends past end of file",
            ));
        }

        // Seek and read
        self.file.seek(SeekFrom::Start(aligned_start))?;
        self.buffer.resize(read_size, 0);
        self.file.read_exact(&mut self.buffer[..read_size])?;

        self.window_start = aligned_start;
        self.window_len = read_size;

        Ok(())
    }
}

impl MapStrategy for BufferedMap {
    fn map_ptr(&mut self, offset: u64, len: usize) -> io::Result<&[u8]> {
        if len == 0 {
            return Ok(&[]);
        }

        // Check if requested range is beyond file size
        if offset.saturating_add(len as u64) > self.size {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "requested range extends past end of file",
            ));
        }

        // Load new window if needed
        if !self.is_in_window(offset, len) {
            self.load_window(offset, len)?;
        }

        // Return slice from buffer
        let start = (offset - self.window_start) as usize;
        let end = start + len;
        Ok(&self.buffer[start..end])
    }

    #[inline]
    fn window_size(&self) -> usize {
        self.max_window
    }

    #[inline]
    fn file_size(&self) -> u64 {
        self.size
    }
}

/// High-level file mapper that wraps a strategy.
///
/// This provides a convenient API for delta application code.
#[derive(Debug)]
pub struct MapFile<S: MapStrategy = BufferedMap> {
    strategy: S,
}

impl MapFile<BufferedMap> {
    /// Opens a file for mapped access using buffered strategy.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be opened.
    pub fn open<P: AsRef<Path>>(path: P) -> io::Result<Self> {
        Ok(Self {
            strategy: BufferedMap::open(path)?,
        })
    }

    /// Creates a MapFile from an already-open file.
    ///
    /// # Errors
    ///
    /// Returns an error if the file size cannot be determined.
    pub fn from_file(file: File) -> io::Result<Self> {
        Ok(Self {
            strategy: BufferedMap::from_file(file)?,
        })
    }
}

impl<S: MapStrategy> MapFile<S> {
    /// Creates a MapFile with a custom strategy.
    pub fn with_strategy(strategy: S) -> Self {
        Self { strategy }
    }

    /// Returns a slice of file data at the given offset.
    ///
    /// # Errors
    ///
    /// Returns an error if the read fails.
    #[inline]
    pub fn map_ptr(&mut self, offset: u64, len: usize) -> io::Result<&[u8]> {
        self.strategy.map_ptr(offset, len)
    }

    /// Returns the total file size.
    #[inline]
    pub fn file_size(&self) -> u64 {
        self.strategy.file_size()
    }

    /// Returns the window size of the underlying strategy.
    #[inline]
    pub fn window_size(&self) -> usize {
        self.strategy.window_size()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn create_test_file(size: usize) -> NamedTempFile {
        let mut file = NamedTempFile::new().unwrap();
        let data: Vec<u8> = (0..size).map(|i| (i % 256) as u8).collect();
        file.write_all(&data).unwrap();
        file.flush().unwrap();
        file
    }

    #[test]
    fn open_file() {
        let temp = create_test_file(1000);
        let map = MapFile::open(temp.path()).unwrap();
        assert_eq!(map.file_size(), 1000);
    }

    #[test]
    fn map_ptr_returns_correct_data() {
        let temp = create_test_file(1000);
        let mut map = MapFile::open(temp.path()).unwrap();

        let data = map.map_ptr(0, 10).unwrap();
        assert_eq!(data, &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9]);
    }

    #[test]
    fn map_ptr_mid_file() {
        let temp = create_test_file(1000);
        let mut map = MapFile::open(temp.path()).unwrap();

        let data = map.map_ptr(500, 10).unwrap();
        let expected: Vec<u8> = (500..510).map(|i| (i % 256) as u8).collect();
        assert_eq!(data, &expected[..]);
    }

    #[test]
    fn map_ptr_sequential_reads_use_cache() {
        let temp = create_test_file(10000);
        let mut map = MapFile::open(temp.path()).unwrap();

        // First read loads window
        let _ = map.map_ptr(0, 100).unwrap();

        // Subsequent reads should hit cache
        for offset in (100..5000).step_by(100) {
            let data = map.map_ptr(offset as u64, 100).unwrap();
            let expected: Vec<u8> = (offset..offset + 100).map(|i| (i % 256) as u8).collect();
            assert_eq!(data, &expected[..]);
        }
    }

    #[test]
    fn map_ptr_zero_length() {
        let temp = create_test_file(1000);
        let mut map = MapFile::open(temp.path()).unwrap();

        let data = map.map_ptr(500, 0).unwrap();
        assert!(data.is_empty());
    }

    #[test]
    fn map_ptr_past_eof_fails() {
        let temp = create_test_file(1000);
        let mut map = MapFile::open(temp.path()).unwrap();

        let result = map.map_ptr(900, 200);
        assert!(result.is_err());
    }

    #[test]
    fn map_ptr_window_slides_forward() {
        let temp = create_test_file(MAX_MAP_SIZE * 3);
        let mut map = MapFile::open(temp.path()).unwrap();

        // Read from start
        let data1 = map.map_ptr(0, 100).unwrap();
        assert_eq!(data1[0], 0);

        // Read from way past window - should load new window
        let offset = (MAX_MAP_SIZE * 2) as u64;
        let data2 = map.map_ptr(offset, 100).unwrap();
        let expected_start = (offset % 256) as u8;
        assert_eq!(data2[0], expected_start);
    }

    #[test]
    fn buffered_map_with_custom_window() {
        let temp = create_test_file(10000);
        let map = BufferedMap::open_with_window(temp.path(), 1024).unwrap();
        assert_eq!(map.window_size(), 1024);
    }

    #[test]
    fn from_file_works() {
        let temp = create_test_file(1000);
        let file = File::open(temp.path()).unwrap();
        let mut map = MapFile::from_file(file).unwrap();

        let data = map.map_ptr(0, 10).unwrap();
        assert_eq!(data, &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9]);
    }

    #[test]
    fn alignment_respected() {
        let temp = create_test_file(5000);
        let mut map = BufferedMap::open_with_window(temp.path(), 4096).unwrap();

        // Request data at non-aligned offset
        let _ = map.map_ptr(1500, 100).unwrap();

        // Window should start at aligned boundary (1024)
        assert_eq!(map.window_start, align_down(1500));
        assert_eq!(map.window_start, 1024);
    }
}
