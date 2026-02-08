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
//! # Strategies
//!
//! This module provides three mapping strategies:
//!
//! - `BufferedMap`: Sliding window buffer for efficient sequential access
//! - `MmapStrategy`: Memory-mapped access for zero-copy large file access
//! - `AdaptiveMapStrategy`: Automatically selects optimal strategy based on file size
//!
//! # Upstream Reference
//!
//! See `fileio.c` in upstream rsync 3.4.1: `map_file()`, `map_ptr()`, `unmap_file()`.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;

use crate::constants::{MAX_MAP_SIZE, align_down};
use fast_io::FileReader;
#[cfg(unix)]
use fast_io::MmapReader;

/// Threshold above which memory mapping is preferred over buffered I/O.
/// Files larger than 1MB benefit from mmap's zero-copy access.
pub const MMAP_THRESHOLD: u64 = 1024 * 1024; // 1 MB

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

// ============================================================================
// BufferedMap - Sliding window buffer for sequential access
// ============================================================================

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

// ============================================================================
// MmapStrategy - Memory-mapped file access using fast_io::MmapReader
// ============================================================================

/// Memory-mapped file mapper for efficient large file access.
///
/// Uses `fast_io::MmapReader` to map the entire file into memory, providing
/// zero-copy access to file contents. This is most effective for:
/// - Large files (> 1 MB)
/// - Random access patterns
/// - Read-only access
///
/// # Safety
///
/// Memory-mapped files can cause undefined behavior if the underlying file
/// is modified while mapped. This implementation is safe as long as the
/// basis file is not modified during delta application.
#[cfg(unix)]
#[derive(Debug)]
pub struct MmapStrategy {
    /// The memory-mapped file reader.
    mmap: MmapReader,
}

impl MmapStrategy {
    /// Opens a file for memory-mapped access.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be opened or mapped.
    #[cfg(unix)]
    pub fn open<P: AsRef<Path>>(path: P) -> io::Result<Self> {
        Ok(Self {
            mmap: MmapReader::open(path)?,
        })
    }

    /// Returns the underlying memory map as a slice.
    #[must_use]
    #[cfg(unix)]
    pub fn as_slice(&self) -> &[u8] {
        self.mmap.as_slice()
    }
}

#[cfg(unix)]
impl MapStrategy for MmapStrategy {
    fn map_ptr(&mut self, offset: u64, len: usize) -> io::Result<&[u8]> {
        if len == 0 {
            return Ok(&[]);
        }

        let size = self.mmap.size();
        if offset.saturating_add(len as u64) > size {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "requested range extends past end of file",
            ));
        }

        let start = offset as usize;
        let end = start + len;
        Ok(&self.mmap.as_slice()[start..end])
    }

    #[inline]
    fn window_size(&self) -> usize {
        // Mmap has no window - entire file is mapped
        self.mmap.size() as usize
    }

    #[inline]
    fn file_size(&self) -> u64 {
        self.mmap.size()
    }
}

// ============================================================================
// AdaptiveMapStrategy - Automatically selects mmap or buffered based on size
// ============================================================================

/// Adaptive file mapper that selects the optimal strategy based on file size.
///
/// Uses memory mapping for files larger than `MMAP_THRESHOLD` (1 MB) and
/// buffered I/O for smaller files. This provides the best performance across
/// different file sizes:
///
/// - **Small files (< 1 MB)**: Buffered I/O avoids mmap setup overhead
/// - **Large files (>= 1 MB)**: Mmap provides zero-copy access
#[cfg(unix)]
#[derive(Debug)]
pub enum AdaptiveMapStrategy {
    /// Buffered I/O for small files.
    Buffered(BufferedMap),
    /// Memory-mapped I/O for large files.
    Mmap(MmapStrategy),
}

#[cfg(unix)]
impl AdaptiveMapStrategy {
    /// Opens a file with automatic strategy selection.
    ///
    /// Uses mmap for files >= `MMAP_THRESHOLD`, buffered I/O otherwise.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be opened.
    pub fn open<P: AsRef<Path>>(path: P) -> io::Result<Self> {
        Self::open_with_threshold(path, MMAP_THRESHOLD)
    }

    /// Opens a file with a custom threshold for strategy selection.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be opened.
    pub fn open_with_threshold<P: AsRef<Path>>(path: P, threshold: u64) -> io::Result<Self> {
        let metadata = std::fs::metadata(path.as_ref())?;
        let size = metadata.len();

        if size >= threshold {
            Ok(Self::Mmap(MmapStrategy::open(path)?))
        } else {
            Ok(Self::Buffered(BufferedMap::open(path)?))
        }
    }

    /// Returns true if using memory-mapped strategy.
    #[must_use]
    pub const fn is_mmap(&self) -> bool {
        matches!(self, Self::Mmap(_))
    }

    /// Returns true if using buffered strategy.
    #[must_use]
    pub const fn is_buffered(&self) -> bool {
        matches!(self, Self::Buffered(_))
    }
}

#[cfg(unix)]
impl MapStrategy for AdaptiveMapStrategy {
    fn map_ptr(&mut self, offset: u64, len: usize) -> io::Result<&[u8]> {
        match self {
            Self::Buffered(b) => b.map_ptr(offset, len),
            Self::Mmap(m) => m.map_ptr(offset, len),
        }
    }

    #[inline]
    fn window_size(&self) -> usize {
        match self {
            Self::Buffered(b) => b.window_size(),
            Self::Mmap(m) => m.window_size(),
        }
    }

    #[inline]
    fn file_size(&self) -> u64 {
        match self {
            Self::Buffered(b) => b.file_size(),
            Self::Mmap(m) => m.file_size(),
        }
    }
}

// ============================================================================
// MapFile - High-level wrapper for file mapping strategies
// ============================================================================

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

#[cfg(unix)]
impl MapFile<MmapStrategy> {
    /// Opens a file for mapped access using memory-mapped strategy.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be opened or mapped.
    pub fn open_mmap<P: AsRef<Path>>(path: P) -> io::Result<Self> {
        Ok(Self {
            strategy: MmapStrategy::open(path)?,
        })
    }
}

#[cfg(unix)]
impl MapFile<AdaptiveMapStrategy> {
    /// Opens a file with automatic strategy selection.
    ///
    /// Uses mmap for files >= 1MB, buffered I/O otherwise.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be opened.
    pub fn open_adaptive<P: AsRef<Path>>(path: P) -> io::Result<Self> {
        Ok(Self {
            strategy: AdaptiveMapStrategy::open(path)?,
        })
    }

    /// Opens a file with automatic strategy selection and custom threshold.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be opened.
    pub fn open_adaptive_with_threshold<P: AsRef<Path>>(
        path: P,
        threshold: u64,
    ) -> io::Result<Self> {
        Ok(Self {
            strategy: AdaptiveMapStrategy::open_with_threshold(path, threshold)?,
        })
    }

    /// Returns true if using memory-mapped strategy.
    #[must_use]
    pub fn is_mmap(&self) -> bool {
        self.strategy.is_mmap()
    }

    /// Returns true if using buffered strategy.
    #[must_use]
    pub fn is_buffered(&self) -> bool {
        self.strategy.is_buffered()
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

    // ========================================================================
    // BufferedMap tests (existing)
    // ========================================================================

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

    // ========================================================================
    // MmapStrategy tests
    // ========================================================================

    #[test]
    fn mmap_strategy_open_and_read() {
        let temp = create_test_file(1000);
        let mut strategy = MmapStrategy::open(temp.path()).unwrap();

        assert_eq!(strategy.file_size(), 1000);

        let data = strategy.map_ptr(0, 10).unwrap();
        assert_eq!(data, &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9]);
    }

    #[test]
    fn mmap_strategy_mid_file_read() {
        let temp = create_test_file(1000);
        let mut strategy = MmapStrategy::open(temp.path()).unwrap();

        let data = strategy.map_ptr(500, 10).unwrap();
        let expected: Vec<u8> = (500..510).map(|i| (i % 256) as u8).collect();
        assert_eq!(data, &expected[..]);
    }

    #[test]
    fn mmap_strategy_zero_length_read() {
        let temp = create_test_file(1000);
        let mut strategy = MmapStrategy::open(temp.path()).unwrap();

        let data = strategy.map_ptr(500, 0).unwrap();
        assert!(data.is_empty());
    }

    #[test]
    fn mmap_strategy_past_eof_fails() {
        let temp = create_test_file(1000);
        let mut strategy = MmapStrategy::open(temp.path()).unwrap();

        let result = strategy.map_ptr(900, 200);
        assert!(result.is_err());
    }

    #[test]
    fn mmap_strategy_window_size_is_file_size() {
        let temp = create_test_file(5000);
        let strategy = MmapStrategy::open(temp.path()).unwrap();

        // Mmap has no window - entire file is mapped
        assert_eq!(strategy.window_size(), 5000);
    }

    #[test]
    fn mmap_strategy_as_slice() {
        let temp = create_test_file(100);
        let strategy = MmapStrategy::open(temp.path()).unwrap();

        let slice = strategy.as_slice();
        assert_eq!(slice.len(), 100);
        assert_eq!(slice[0], 0);
        assert_eq!(slice[99], 99);
    }

    #[test]
    fn map_file_open_mmap() {
        let temp = create_test_file(1000);
        let mut map = MapFile::open_mmap(temp.path()).unwrap();

        assert_eq!(map.file_size(), 1000);

        let data = map.map_ptr(0, 10).unwrap();
        assert_eq!(data, &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9]);
    }

    // ========================================================================
    // AdaptiveMapStrategy tests
    // ========================================================================

    #[test]
    fn adaptive_strategy_uses_buffered_for_small_files() {
        let temp = create_test_file(100);
        let strategy = AdaptiveMapStrategy::open_with_threshold(temp.path(), 1000).unwrap();

        assert!(strategy.is_buffered());
        assert!(!strategy.is_mmap());
    }

    #[test]
    fn adaptive_strategy_uses_mmap_for_large_files() {
        let temp = create_test_file(2000);
        let strategy = AdaptiveMapStrategy::open_with_threshold(temp.path(), 1000).unwrap();

        assert!(strategy.is_mmap());
        assert!(!strategy.is_buffered());
    }

    #[test]
    fn adaptive_strategy_threshold_boundary() {
        // File exactly at threshold should use mmap
        let temp = create_test_file(1000);
        let strategy = AdaptiveMapStrategy::open_with_threshold(temp.path(), 1000).unwrap();

        assert!(strategy.is_mmap());
    }

    #[test]
    fn adaptive_strategy_reads_correctly_buffered() {
        let temp = create_test_file(100);
        let mut strategy = AdaptiveMapStrategy::open_with_threshold(temp.path(), 1000).unwrap();

        let data = strategy.map_ptr(50, 10).unwrap();
        let expected: Vec<u8> = (50..60).collect();
        assert_eq!(data, &expected[..]);
    }

    #[test]
    fn adaptive_strategy_reads_correctly_mmap() {
        let temp = create_test_file(2000);
        let mut strategy = AdaptiveMapStrategy::open_with_threshold(temp.path(), 1000).unwrap();

        let data = strategy.map_ptr(500, 10).unwrap();
        let expected: Vec<u8> = (500..510).map(|i| (i % 256) as u8).collect();
        assert_eq!(data, &expected[..]);
    }

    #[test]
    fn adaptive_strategy_file_size() {
        let temp = create_test_file(5000);
        let strategy = AdaptiveMapStrategy::open(temp.path()).unwrap();

        assert_eq!(strategy.file_size(), 5000);
    }

    #[test]
    fn map_file_open_adaptive() {
        let temp = create_test_file(100);
        let map = MapFile::open_adaptive(temp.path()).unwrap();

        assert_eq!(map.file_size(), 100);
        // Small file uses buffered
        assert!(map.is_buffered());
    }

    #[test]
    fn map_file_open_adaptive_with_threshold() {
        // Small file with low threshold -> mmap
        let temp = create_test_file(100);
        let map = MapFile::open_adaptive_with_threshold(temp.path(), 50).unwrap();

        assert!(map.is_mmap());
    }

    #[test]
    fn map_file_adaptive_data_access() {
        let temp = create_test_file(1000);
        let mut map = MapFile::open_adaptive(temp.path()).unwrap();

        let data = map.map_ptr(0, 10).unwrap();
        assert_eq!(data, &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9]);
    }

    #[test]
    fn adaptive_default_threshold() {
        // File below 1MB threshold should use buffered
        let temp = create_test_file(500_000);
        let strategy = AdaptiveMapStrategy::open(temp.path()).unwrap();

        assert!(strategy.is_buffered());
    }

    // =========================================================================
    // Empty File Tests
    // =========================================================================

    #[test]
    fn empty_file_open_buffered() {
        let temp = create_test_file(0);
        let map = MapFile::open(temp.path()).unwrap();
        assert_eq!(map.file_size(), 0);
        assert_eq!(map.window_size(), MAX_MAP_SIZE);
    }

    #[test]
    fn empty_file_open_mmap() {
        let temp = create_test_file(0);
        let map = MapFile::open_mmap(temp.path()).unwrap();
        assert_eq!(map.file_size(), 0);
        assert_eq!(map.window_size(), 0);
    }

    #[test]
    fn empty_file_open_adaptive() {
        let temp = create_test_file(0);
        let map = MapFile::open_adaptive(temp.path()).unwrap();
        assert_eq!(map.file_size(), 0);
        assert!(map.is_buffered()); // Empty files use buffered (below threshold)
    }

    #[test]
    fn empty_file_map_ptr_zero_length_succeeds() {
        let temp = create_test_file(0);
        let mut map = MapFile::open(temp.path()).unwrap();

        // Zero-length read at offset 0 should succeed
        let data = map.map_ptr(0, 0).unwrap();
        assert!(data.is_empty());
    }

    #[test]
    fn empty_file_map_ptr_nonzero_length_fails() {
        let temp = create_test_file(0);
        let mut map = MapFile::open(temp.path()).unwrap();

        // Non-zero length read should fail
        let result = map.map_ptr(0, 1);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn empty_file_mmap_zero_length_succeeds() {
        let temp = create_test_file(0);
        let mut strategy = MmapStrategy::open(temp.path()).unwrap();

        let data = strategy.map_ptr(0, 0).unwrap();
        assert!(data.is_empty());
    }

    #[test]
    fn empty_file_mmap_nonzero_length_fails() {
        let temp = create_test_file(0);
        let mut strategy = MmapStrategy::open(temp.path()).unwrap();

        let result = strategy.map_ptr(0, 1);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
    }

    // =========================================================================
    // Large File Tests
    // =========================================================================

    #[test]
    fn large_file_exceeds_single_window_buffered() {
        // Create file larger than MAX_MAP_SIZE (256 KB)
        let size = MAX_MAP_SIZE * 2 + 1000;
        let temp = create_test_file(size);
        let mut map = MapFile::open(temp.path()).unwrap();

        assert_eq!(map.file_size(), size as u64);

        // Read from start
        let data1 = map.map_ptr(0, 100).unwrap();
        assert_eq!(data1[0], 0);

        // Read from middle (beyond first window)
        let mid_offset = (MAX_MAP_SIZE + 1000) as u64;
        let data2 = map.map_ptr(mid_offset, 100).unwrap();
        assert_eq!(data2[0], (mid_offset % 256) as u8);

        // Read from end
        let end_offset = (size - 100) as u64;
        let data3 = map.map_ptr(end_offset, 100).unwrap();
        assert_eq!(data3[0], (end_offset % 256) as u8);
    }

    #[test]
    fn large_file_mmap_strategy() {
        // Create file larger than MMAP_THRESHOLD (1 MB)
        let size = MMAP_THRESHOLD as usize + 1024;
        let temp = create_test_file(size);
        let mut map = MapFile::open_mmap(temp.path()).unwrap();

        assert_eq!(map.file_size(), size as u64);

        // Mmap should be able to access any offset without window issues
        let data1 = map.map_ptr(0, 100).unwrap();
        assert_eq!(data1[0], 0);

        let mid_offset = (size / 2) as u64;
        let data2 = map.map_ptr(mid_offset, 100).unwrap();
        assert_eq!(data2[0], (mid_offset % 256) as u8);

        let end_offset = (size - 100) as u64;
        let data3 = map.map_ptr(end_offset, 100).unwrap();
        assert_eq!(data3[0], (end_offset % 256) as u8);
    }

    #[test]
    fn large_file_adaptive_uses_mmap() {
        // Create file larger than MMAP_THRESHOLD
        let size = MMAP_THRESHOLD as usize + 1024;
        let temp = create_test_file(size);
        let map = MapFile::open_adaptive(temp.path()).unwrap();

        assert!(map.is_mmap());
        assert_eq!(map.file_size(), size as u64);
    }

    #[test]
    fn large_file_sequential_access_across_windows() {
        let size = MAX_MAP_SIZE * 4;
        let temp = create_test_file(size);
        let mut map = MapFile::open(temp.path()).unwrap();

        // Sequential reads that span multiple windows
        let chunk_size = 1000;
        for offset in (0..size).step_by(MAX_MAP_SIZE / 2) {
            if offset + chunk_size > size {
                break;
            }
            let data = map.map_ptr(offset as u64, chunk_size).unwrap();
            let expected: Vec<u8> = (offset..offset + chunk_size)
                .map(|i| (i % 256) as u8)
                .collect();
            assert_eq!(data, &expected[..], "Mismatch at offset {offset}");
        }
    }

    #[test]
    fn large_file_random_access_pattern() {
        let size = MAX_MAP_SIZE * 3;
        let temp = create_test_file(size);
        let mut map = MapFile::open(temp.path()).unwrap();

        // Random access pattern that forces window reloads
        let offsets = [
            0,
            MAX_MAP_SIZE * 2,
            1000,
            MAX_MAP_SIZE + 500,
            MAX_MAP_SIZE * 2 + 1000,
            500,
        ];

        for &offset in &offsets {
            if offset + 100 > size {
                continue;
            }
            let data = map.map_ptr(offset as u64, 100).unwrap();
            assert_eq!(data[0], (offset % 256) as u8, "Mismatch at offset {offset}");
        }
    }

    // =========================================================================
    // Window Sliding Tests
    // =========================================================================

    #[test]
    fn window_slides_forward_correctly() {
        let size = MAX_MAP_SIZE * 2;
        let temp = create_test_file(size);
        let mut map = BufferedMap::open(temp.path()).unwrap();

        // Initial read at start
        let _ = map.map_ptr(0, 100).unwrap();
        assert_eq!(map.window_start, 0);

        // Read that requires window slide
        let far_offset = (MAX_MAP_SIZE + 1000) as u64;
        let _ = map.map_ptr(far_offset, 100).unwrap();

        // Window should have moved
        assert!(map.window_start > 0);
        assert!(map.window_start <= far_offset);
    }

    #[test]
    fn window_can_slide_backward() {
        let size = MAX_MAP_SIZE * 2;
        let temp = create_test_file(size);
        let mut map = BufferedMap::open(temp.path()).unwrap();

        // First read far into file
        let far_offset = (MAX_MAP_SIZE + 1000) as u64;
        let _ = map.map_ptr(far_offset, 100).unwrap();
        let first_window_start = map.window_start;

        // Now read from start - should slide backward
        let _ = map.map_ptr(0, 100).unwrap();
        assert!(map.window_start < first_window_start);
        assert_eq!(map.window_start, 0);
    }

    #[test]
    fn window_respects_alignment_boundary() {
        let temp = create_test_file(10000);
        let mut map = BufferedMap::open_with_window(temp.path(), 2048).unwrap();

        // Test various non-aligned offsets that force window reloads
        // Use offsets far enough apart to ensure window slides
        let test_offsets = [100, 3000, 5500, 8000];

        for &offset in &test_offsets {
            let _ = map.map_ptr(offset, 100).unwrap();
            // Window start should always be aligned to ALIGN_BOUNDARY
            assert_eq!(
                map.window_start % crate::constants::ALIGN_BOUNDARY as u64,
                0,
                "Window start not on alignment boundary (start={}, boundary={}, offset={})",
                map.window_start,
                crate::constants::ALIGN_BOUNDARY,
                offset
            );
            // Window should contain the requested offset
            assert!(
                offset >= map.window_start && offset < map.window_start + map.window_len as u64,
                "Offset {} not in window [{}, {})",
                offset,
                map.window_start,
                map.window_start + map.window_len as u64
            );
        }
    }

    // =========================================================================
    // Cache Behavior Tests
    // =========================================================================

    #[test]
    fn cache_hit_within_window() {
        let temp = create_test_file(10000);
        let mut map = BufferedMap::open(temp.path()).unwrap();

        // Load window
        let _ = map.map_ptr(0, 100).unwrap();
        let initial_window_start = map.window_start;
        let initial_window_len = map.window_len;

        // Multiple reads within same window should not change window
        for offset in [100, 200, 500, 1000] {
            let _ = map.map_ptr(offset as u64, 100).unwrap();
            assert_eq!(map.window_start, initial_window_start);
            assert_eq!(map.window_len, initial_window_len);
        }
    }

    #[test]
    fn cache_miss_outside_window() {
        let size = MAX_MAP_SIZE * 2;
        let temp = create_test_file(size);
        let mut map = BufferedMap::open(temp.path()).unwrap();

        // Load initial window
        let _ = map.map_ptr(0, 100).unwrap();

        // Access outside window should cause reload
        let far_offset = (MAX_MAP_SIZE + 1000) as u64;
        let _ = map.map_ptr(far_offset, 100).unwrap();

        // Window should have moved
        assert!(map.window_start > 0);
    }

    // =========================================================================
    // Custom Window Size Tests
    // =========================================================================

    #[test]
    fn small_custom_window_size() {
        let temp = create_test_file(10000);
        // Window must be large enough to hold aligned reads
        // With 1024 alignment, need at least 2048 to read at offset 1000
        let mut map = BufferedMap::open_with_window(temp.path(), 2048).unwrap();
        assert_eq!(map.window_size(), 2048);

        // Should still work with small window
        let data = map.map_ptr(0, 100).unwrap();
        assert_eq!(data[0], 0);

        // Access within small window should work
        let data = map.map_ptr(1000, 100).unwrap();
        assert_eq!(data[0], (1000 % 256) as u8);
    }

    #[test]
    fn large_custom_window_size() {
        // Create file larger than the large window
        let window_size = MAX_MAP_SIZE * 2;
        let file_size = window_size + 10000;
        let temp = create_test_file(file_size);
        let mut map = BufferedMap::open_with_window(temp.path(), window_size).unwrap();
        assert_eq!(map.window_size(), window_size);

        // Should be able to read across what would normally be multiple windows
        // Check first position
        let data1 = map.map_ptr(0, 100).unwrap();
        assert_eq!(data1[0], 0);

        // Check position within the large window (releases first borrow)
        let offset = MAX_MAP_SIZE + 1000;
        let data2 = map.map_ptr(offset as u64, 100).unwrap();
        assert_eq!(data2[0], (offset % 256) as u8);
    }

    #[test]
    fn from_file_with_custom_window() {
        let temp = create_test_file(10000);
        let file = File::open(temp.path()).unwrap();
        let map = BufferedMap::from_file_with_window(file, 2048).unwrap();
        assert_eq!(map.window_size(), 2048);
        assert_eq!(map.file_size(), 10000);
    }

    // =========================================================================
    // Error Handling Tests
    // =========================================================================

    #[test]
    fn file_not_found_error_buffered() {
        let result = MapFile::open("/nonexistent/path/to/file.txt");
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn file_not_found_error_mmap() {
        let result = MapFile::<MmapStrategy>::open_mmap("/nonexistent/path/to/file.txt");
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn file_not_found_error_adaptive() {
        let result = MapFile::<AdaptiveMapStrategy>::open_adaptive("/nonexistent/path/to/file.txt");
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn buffered_map_file_not_found() {
        let result = BufferedMap::open("/nonexistent/path/to/file.txt");
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn mmap_strategy_file_not_found() {
        let result = MmapStrategy::open("/nonexistent/path/to/file.txt");
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn adaptive_strategy_file_not_found() {
        let result = AdaptiveMapStrategy::open("/nonexistent/path/to/file.txt");
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::NotFound);
    }

    #[test]
    #[cfg(unix)]
    fn permission_denied_error_buffered() {
        use std::os::unix::fs::PermissionsExt;
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let path = dir.path().join("no_read.txt");
        std::fs::write(&path, b"test data").unwrap();

        // Remove read permissions
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o000);
        std::fs::set_permissions(&path, perms).unwrap();

        let result = MapFile::open(&path);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::PermissionDenied);

        // Restore permissions for cleanup
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o644);
        std::fs::set_permissions(&path, perms).unwrap();
    }

    #[test]
    #[cfg(unix)]
    fn permission_denied_error_mmap() {
        use std::os::unix::fs::PermissionsExt;
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let path = dir.path().join("no_read.txt");
        std::fs::write(&path, b"test data").unwrap();

        // Remove read permissions
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o000);
        std::fs::set_permissions(&path, perms).unwrap();

        let result = MmapStrategy::open(&path);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::PermissionDenied);

        // Restore permissions for cleanup
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o644);
        std::fs::set_permissions(&path, perms).unwrap();
    }

    #[test]
    fn map_ptr_offset_at_eof() {
        let temp = create_test_file(1000);
        let mut map = MapFile::open(temp.path()).unwrap();

        // Zero-length read at EOF should succeed
        let data = map.map_ptr(1000, 0).unwrap();
        assert!(data.is_empty());
    }

    #[test]
    fn map_ptr_offset_past_eof() {
        let temp = create_test_file(1000);
        let mut map = MapFile::open(temp.path()).unwrap();

        // Read starting past EOF should fail
        let result = map.map_ptr(1001, 10);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn map_ptr_read_extends_past_eof() {
        let temp = create_test_file(1000);
        let mut map = MapFile::open(temp.path()).unwrap();

        // Read that extends past EOF should fail
        let result = map.map_ptr(950, 100);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn mmap_map_ptr_offset_past_eof() {
        let temp = create_test_file(1000);
        let mut map = MapFile::open_mmap(temp.path()).unwrap();

        let result = map.map_ptr(1001, 10);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
    }

    // =========================================================================
    // Edge Case Tests
    // =========================================================================

    #[test]
    fn single_byte_file_buffered() {
        let mut file = NamedTempFile::new().unwrap();
        file.write_all(&[42]).unwrap();
        file.flush().unwrap();

        let mut map = MapFile::open(file.path()).unwrap();
        assert_eq!(map.file_size(), 1);

        let data = map.map_ptr(0, 1).unwrap();
        assert_eq!(data, &[42]);
    }

    #[test]
    fn single_byte_file_mmap() {
        let mut file = NamedTempFile::new().unwrap();
        file.write_all(&[42]).unwrap();
        file.flush().unwrap();

        let mut map = MapFile::open_mmap(file.path()).unwrap();
        assert_eq!(map.file_size(), 1);

        let data = map.map_ptr(0, 1).unwrap();
        assert_eq!(data, &[42]);
    }

    #[test]
    fn read_exactly_file_size() {
        let temp = create_test_file(1000);
        let mut map = MapFile::open(temp.path()).unwrap();

        // Read entire file
        let data = map.map_ptr(0, 1000).unwrap();
        assert_eq!(data.len(), 1000);

        let expected: Vec<u8> = (0..1000).map(|i| (i % 256) as u8).collect();
        assert_eq!(data, &expected[..]);
    }

    #[test]
    fn read_last_byte() {
        let temp = create_test_file(1000);
        let mut map = MapFile::open(temp.path()).unwrap();

        let data = map.map_ptr(999, 1).unwrap();
        assert_eq!(data, &[(999 % 256) as u8]);
    }

    #[test]
    fn many_sequential_small_reads() {
        let temp = create_test_file(10000);
        let mut map = MapFile::open(temp.path()).unwrap();

        // Many small sequential reads (simulating rsync block access)
        for offset in (0..9900).step_by(10) {
            let data = map.map_ptr(offset as u64, 10).unwrap();
            let expected: Vec<u8> = (offset..offset + 10).map(|i| (i % 256) as u8).collect();
            assert_eq!(data, &expected[..]);
        }
    }

    #[test]
    fn overlapping_reads() {
        let temp = create_test_file(1000);
        let mut map = MapFile::open(temp.path()).unwrap();

        // Overlapping reads should return correct data
        let data1 = map.map_ptr(0, 100).unwrap().to_vec();
        let data2 = map.map_ptr(50, 100).unwrap().to_vec();
        let data3 = map.map_ptr(25, 50).unwrap().to_vec();

        // Verify overlap regions match
        assert_eq!(&data1[50..100], &data2[0..50]);
        assert_eq!(&data1[25..75], &data3[..]);
    }

    // =========================================================================
    // Binary Data Tests
    // =========================================================================

    #[test]
    fn binary_data_with_null_bytes() {
        let mut file = NamedTempFile::new().unwrap();
        let data = vec![0u8, 1, 2, 0, 0, 3, 0, 4, 0, 0, 0, 5];
        file.write_all(&data).unwrap();
        file.flush().unwrap();

        let mut map = MapFile::open(file.path()).unwrap();
        let read_data = map.map_ptr(0, data.len()).unwrap();
        assert_eq!(read_data, &data[..]);
    }

    #[test]
    fn all_byte_values() {
        let mut file = NamedTempFile::new().unwrap();
        let data: Vec<u8> = (0..=255).collect();
        file.write_all(&data).unwrap();
        file.flush().unwrap();

        let mut map = MapFile::open(file.path()).unwrap();
        let read_data = map.map_ptr(0, 256).unwrap();
        assert_eq!(read_data, &data[..]);
    }

    #[test]
    fn binary_data_mmap() {
        let mut file = NamedTempFile::new().unwrap();
        let data: Vec<u8> = (0..=255).collect();
        file.write_all(&data).unwrap();
        file.flush().unwrap();

        let mut map = MapFile::open_mmap(file.path()).unwrap();
        let read_data = map.map_ptr(0, 256).unwrap();
        assert_eq!(read_data, &data[..]);
    }

    // =========================================================================
    // MapStrategy Trait Tests
    // =========================================================================

    #[test]
    fn map_strategy_file_size_buffered() {
        let temp = create_test_file(5000);
        let map = BufferedMap::open(temp.path()).unwrap();
        assert_eq!(map.file_size(), 5000);
    }

    #[test]
    fn map_strategy_window_size_buffered() {
        let temp = create_test_file(1000);
        let map = BufferedMap::open(temp.path()).unwrap();
        assert_eq!(map.window_size(), MAX_MAP_SIZE);
    }

    // =========================================================================
    // MapFile with Custom Strategy Tests
    // =========================================================================

    #[test]
    fn map_file_with_strategy_buffered() {
        let temp = create_test_file(1000);
        let strategy = BufferedMap::open(temp.path()).unwrap();
        let mut map = MapFile::with_strategy(strategy);

        assert_eq!(map.file_size(), 1000);
        let data = map.map_ptr(0, 10).unwrap();
        assert_eq!(data, &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9]);
    }

    #[test]
    fn map_file_with_strategy_mmap() {
        let temp = create_test_file(1000);
        let strategy = MmapStrategy::open(temp.path()).unwrap();
        let mut map = MapFile::with_strategy(strategy);

        assert_eq!(map.file_size(), 1000);
        let data = map.map_ptr(0, 10).unwrap();
        assert_eq!(data, &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9]);
    }

    // =========================================================================
    // Stress Tests
    // =========================================================================

    #[test]
    fn stress_alternating_start_end_reads_buffered() {
        let size = MAX_MAP_SIZE * 2;
        let temp = create_test_file(size);
        let mut map = MapFile::open(temp.path()).unwrap();

        // Alternately read from start and end (worst case for caching)
        for _ in 0..10 {
            let data_start = map.map_ptr(0, 100).unwrap();
            assert_eq!(data_start[0], 0);

            let end_offset = (size - 100) as u64;
            let data_end = map.map_ptr(end_offset, 100).unwrap();
            assert_eq!(data_end[0], (end_offset % 256) as u8);
        }
    }

    #[test]
    fn stress_alternating_start_end_reads_mmap() {
        let size = MAX_MAP_SIZE * 2;
        let temp = create_test_file(size);
        let mut map = MapFile::open_mmap(temp.path()).unwrap();

        // Mmap should handle alternating access efficiently (no window sliding)
        for _ in 0..10 {
            let data_start = map.map_ptr(0, 100).unwrap();
            assert_eq!(data_start[0], 0);

            let end_offset = (size - 100) as u64;
            let data_end = map.map_ptr(end_offset, 100).unwrap();
            assert_eq!(data_end[0], (end_offset % 256) as u8);
        }
    }

    #[test]
    fn stress_many_window_reloads() {
        let size = MAX_MAP_SIZE * 4;
        let temp = create_test_file(size);
        // Window must be large enough for aligned reads + requested size
        // With 1024 alignment, need at least 2048 to safely read 100 bytes anywhere
        let mut map = BufferedMap::open_with_window(temp.path(), 2048).unwrap();

        // Force many window reloads with small window
        for i in 0..100 {
            let offset = ((i * 500) % (size - 100)) as u64;
            let data = map.map_ptr(offset, 100).unwrap();
            assert_eq!(data[0], (offset % 256) as u8);
        }
    }

    // =========================================================================
    // Threshold Boundary Tests
    // =========================================================================

    #[test]
    fn threshold_boundary_one_below() {
        let threshold = 1000u64;
        let temp = create_test_file((threshold - 1) as usize);
        let strategy = AdaptiveMapStrategy::open_with_threshold(temp.path(), threshold).unwrap();

        assert!(strategy.is_buffered());
    }

    #[test]
    fn threshold_boundary_exactly_at() {
        let threshold = 1000u64;
        let temp = create_test_file(threshold as usize);
        let strategy = AdaptiveMapStrategy::open_with_threshold(temp.path(), threshold).unwrap();

        assert!(strategy.is_mmap());
    }

    #[test]
    fn threshold_boundary_one_above() {
        let threshold = 1000u64;
        let temp = create_test_file((threshold + 1) as usize);
        let strategy = AdaptiveMapStrategy::open_with_threshold(temp.path(), threshold).unwrap();

        assert!(strategy.is_mmap());
    }

    #[test]
    fn default_threshold_value() {
        assert_eq!(MMAP_THRESHOLD, 1024 * 1024);
    }

    // =========================================================================
    // Large File Mmap Tests (Multi-MB files, but < 10MB)
    // =========================================================================

    #[test]
    fn large_file_2mb_mmap_sequential_access() {
        // 2MB file - large enough to trigger mmap, small enough for CI
        let size = 2 * 1024 * 1024;
        let temp = create_test_file(size);
        let mut map = MapFile::open_mmap(temp.path()).unwrap();

        assert_eq!(map.file_size(), size as u64);
        assert_eq!(map.window_size(), size); // Entire file is window

        // Sequential reads across the entire file
        let chunk_size = 4096;
        for offset in (0..size).step_by(chunk_size * 10) {
            if offset + chunk_size > size {
                break;
            }
            let data = map.map_ptr(offset as u64, chunk_size).unwrap();
            assert_eq!(data.len(), chunk_size);
            assert_eq!(data[0], (offset % 256) as u8);
        }
    }

    #[test]
    fn large_file_5mb_mmap_random_access() {
        // 5MB file with random access pattern
        let size = 5 * 1024 * 1024;
        let temp = create_test_file(size);
        let mut map = MapFile::open_mmap(temp.path()).unwrap();

        // Random access at various points
        let test_offsets = [0, 1024, size / 4, size / 2, 3 * size / 4, size - 1024];

        for &offset in &test_offsets {
            let data = map.map_ptr(offset as u64, 1024).unwrap();
            assert_eq!(data[0], (offset % 256) as u8);
        }
    }

    #[test]
    fn large_file_8mb_adaptive_uses_mmap() {
        // 8MB file - should definitely use mmap with default threshold
        let size = 8 * 1024 * 1024;
        let temp = create_test_file(size);
        let map = MapFile::open_adaptive(temp.path()).unwrap();

        assert!(map.is_mmap());
        assert_eq!(map.file_size(), size as u64);
    }

    #[test]
    fn large_file_buffered_vs_mmap_correctness() {
        // Verify buffered and mmap return identical data for same file
        let size = 3 * 1024 * 1024; // 3MB
        let temp = create_test_file(size);

        let mut buffered = MapFile::open(temp.path()).unwrap();
        let mut mmap = MapFile::open_mmap(temp.path()).unwrap();

        // Test at multiple offsets
        for offset in (0..size).step_by(512 * 1024) {
            if offset + 1024 > size {
                break;
            }
            let buf_data = buffered.map_ptr(offset as u64, 1024).unwrap().to_vec();
            let mmap_data = mmap.map_ptr(offset as u64, 1024).unwrap();

            assert_eq!(buf_data, mmap_data, "Data mismatch at offset {offset}");
        }
    }

    #[test]
    fn large_file_mmap_read_last_page() {
        // Test reading the last page of a large file
        let size = 4 * 1024 * 1024; // 4MB
        let temp = create_test_file(size);
        let mut map = MapFile::open_mmap(temp.path()).unwrap();

        // Read last 4KB
        let last_page_offset = size - 4096;
        let data = map.map_ptr(last_page_offset as u64, 4096).unwrap();
        assert_eq!(data.len(), 4096);
        assert_eq!(data[0], (last_page_offset % 256) as u8);
    }

    #[test]
    fn large_file_mmap_strided_access() {
        // Strided access pattern (common in block-based algorithms)
        let size = 2 * 1024 * 1024; // 2MB
        let temp = create_test_file(size);
        let mut map = MapFile::open_mmap(temp.path()).unwrap();

        let block_size = 8192;
        let stride = 64 * 1024; // 64KB stride

        for i in 0..((size / stride) - 1) {
            let offset = i * stride;
            if offset + block_size > size {
                break;
            }
            let data = map.map_ptr(offset as u64, block_size).unwrap();
            assert_eq!(data.len(), block_size);
            assert_eq!(data[0], (offset % 256) as u8);
        }
    }

    // =========================================================================
    // Concurrent Access Tests
    // =========================================================================

    #[test]
    fn mmap_send_trait() {
        // Verify MmapStrategy implements Send (required for thread safety)
        fn assert_send<T: Send>() {}
        assert_send::<MmapStrategy>();
    }

    #[test]
    fn buffered_send_trait() {
        // Verify BufferedMap implements Send
        fn assert_send<T: Send>() {}
        assert_send::<BufferedMap>();
    }

    #[test]
    fn adaptive_send_trait() {
        // Verify AdaptiveMapStrategy implements Send
        fn assert_send<T: Send>() {}
        assert_send::<AdaptiveMapStrategy>();
    }

    #[test]
    fn multiple_readers_same_file_mmap() {
        // Multiple independent readers can read the same file
        let size = 1024 * 1024; // 1MB
        let temp = create_test_file(size);

        let mut reader1 = MapFile::open_mmap(temp.path()).unwrap();
        let mut reader2 = MapFile::open_mmap(temp.path()).unwrap();

        // Both readers should access same data independently
        let data1 = reader1.map_ptr(0, 1024).unwrap().to_vec();
        let data2 = reader2.map_ptr(0, 1024).unwrap();
        assert_eq!(data1, data2);

        // Readers can be at different positions
        let offset1 = 0;
        let offset2 = size / 2;
        let d1 = reader1.map_ptr(offset1, 1024).unwrap();
        let d2 = reader2.map_ptr(offset2 as u64, 1024).unwrap();
        assert_eq!(d1[0], (offset1 % 256) as u8);
        assert_eq!(d2[0], (offset2 % 256) as u8);
    }

    #[test]
    fn multiple_readers_same_file_buffered() {
        // Multiple buffered readers can read the same file
        let size = 1024 * 1024; // 1MB
        let temp = create_test_file(size);

        let mut reader1 = MapFile::open(temp.path()).unwrap();
        let mut reader2 = MapFile::open(temp.path()).unwrap();

        // Each has independent window
        let data1 = reader1.map_ptr(0, 1024).unwrap().to_vec();
        let data2 = reader2.map_ptr(0, 1024).unwrap();
        assert_eq!(data1, data2);

        // Different positions work independently
        let d1 = reader1.map_ptr(0, 1024).unwrap().to_vec();
        let d2 = reader2.map_ptr((size / 2) as u64, 1024).unwrap();
        assert_eq!(d1[0], 0);
        assert_eq!(d2[0], ((size / 2) % 256) as u8);
    }

    // =========================================================================
    // Memory Safety Tests
    // =========================================================================

    #[test]
    fn mmap_borrowed_slice_lifetime() {
        // Ensure borrowed slices are properly bound to reader lifetime
        let temp = create_test_file(1000);
        let mut map = MapFile::open_mmap(temp.path()).unwrap();

        let data1 = map.map_ptr(0, 100).unwrap().to_vec();
        // map_ptr returns &[u8] borrowed from map, so we can call it again
        let data2 = map.map_ptr(100, 100).unwrap().to_vec();

        assert_eq!(data1[0], 0);
        assert_eq!(data2[0], 100);
    }

    #[test]
    fn buffered_borrowed_slice_lifetime() {
        // Buffered slices also follow proper lifetime rules
        let temp = create_test_file(1000);
        let mut map = MapFile::open(temp.path()).unwrap();

        let data1 = map.map_ptr(0, 100).unwrap().to_vec();
        let data2 = map.map_ptr(100, 100).unwrap().to_vec();

        assert_eq!(data1[0], 0);
        assert_eq!(data2[0], 100);
    }

    #[test]
    fn mmap_slice_bounds_checking() {
        // Verify bounds checking prevents out-of-bounds access
        let temp = create_test_file(1000);
        let mut map = MapFile::open_mmap(temp.path()).unwrap();

        // Valid access
        assert!(map.map_ptr(0, 1000).is_ok());
        assert!(map.map_ptr(999, 1).is_ok());

        // Invalid access
        assert!(map.map_ptr(1000, 1).is_err());
        assert!(map.map_ptr(500, 501).is_err());
        assert!(map.map_ptr(u64::MAX, 1).is_err());
    }

    #[test]
    fn buffered_slice_bounds_checking() {
        let temp = create_test_file(1000);
        let mut map = MapFile::open(temp.path()).unwrap();

        // Valid access
        assert!(map.map_ptr(0, 1000).is_ok());
        assert!(map.map_ptr(999, 1).is_ok());

        // Invalid access
        assert!(map.map_ptr(1000, 1).is_err());
        assert!(map.map_ptr(500, 501).is_err());
    }

    // =========================================================================
    // Performance Characteristic Tests
    // =========================================================================

    #[test]
    fn mmap_no_window_sliding_overhead() {
        // Mmap should not have window sliding overhead
        let size = 2 * 1024 * 1024; // 2MB
        let temp = create_test_file(size);
        let mut map = MapFile::open_mmap(temp.path()).unwrap();

        // Access pattern that would cause many window slides with buffered I/O
        for _ in 0..10 {
            let _ = map.map_ptr(0, 100).unwrap();
            let _ = map.map_ptr((size - 100) as u64, 100).unwrap();
            let _ = map.map_ptr((size / 2) as u64, 100).unwrap();
        }
        // Test passes if no panics/errors - mmap handles this efficiently
    }

    #[test]
    fn buffered_sequential_access_efficiency() {
        // Buffered should handle sequential access efficiently
        let size = MAX_MAP_SIZE * 2;
        let temp = create_test_file(size);
        let mut map = MapFile::open(temp.path()).unwrap();

        // Sequential reads should mostly hit cache
        for offset in (0..size).step_by(1024) {
            if offset + 100 > size {
                break;
            }
            let data = map.map_ptr(offset as u64, 100).unwrap();
            assert_eq!(data[0], (offset % 256) as u8);
        }
    }

    #[test]
    fn adaptive_switches_at_threshold() {
        // Verify adaptive switching behavior at exact threshold
        let below = MMAP_THRESHOLD - 1;
        let at = MMAP_THRESHOLD;
        let above = MMAP_THRESHOLD + 1;

        let temp_below = create_test_file(below as usize);
        let temp_at = create_test_file(at as usize);
        let temp_above = create_test_file(above as usize);

        let map_below = MapFile::open_adaptive(temp_below.path()).unwrap();
        let map_at = MapFile::open_adaptive(temp_at.path()).unwrap();
        let map_above = MapFile::open_adaptive(temp_above.path()).unwrap();

        assert!(map_below.is_buffered());
        assert!(map_at.is_mmap());
        assert!(map_above.is_mmap());
    }

    // =========================================================================
    // Integration Tests with Different File Patterns
    // =========================================================================

    #[test]
    fn sparse_access_pattern_mmap() {
        // Sparse access pattern (reading small chunks spread across file)
        let size = 4 * 1024 * 1024; // 4MB
        let temp = create_test_file(size);
        let mut map = MapFile::open_mmap(temp.path()).unwrap();

        let offsets = [
            0,
            64 * 1024,
            512 * 1024,
            1024 * 1024,
            2 * 1024 * 1024,
            3 * 1024 * 1024,
            size - 1024,
        ];

        for &offset in &offsets {
            let data = map.map_ptr(offset as u64, 512).unwrap();
            assert_eq!(data[0], (offset % 256) as u8);
        }
    }

    #[test]
    fn reverse_sequential_access_buffered() {
        // Reading file backwards
        let size = MAX_MAP_SIZE * 2;
        let temp = create_test_file(size);
        let mut map = MapFile::open(temp.path()).unwrap();

        let step = 4096;
        for offset in (step..size).step_by(step).rev() {
            if offset < 100 {
                break;
            }
            let data = map.map_ptr((offset - 100) as u64, 100).unwrap();
            assert_eq!(data[0], ((offset - 100) % 256) as u8);
        }
    }

    #[test]
    fn zigzag_access_pattern() {
        // Zigzag between start and progressively further offsets
        let size = 2 * 1024 * 1024; // 2MB
        let temp = create_test_file(size);
        let mut map = MapFile::open_mmap(temp.path()).unwrap();

        for i in 1..10 {
            let near_offset = 0;
            let far_offset = (i * 200 * 1024).min(size - 100);

            let data1 = map.map_ptr(near_offset, 100).unwrap().to_vec();
            let data2 = map.map_ptr(far_offset as u64, 100).unwrap();

            assert_eq!(data1[0], 0);
            assert_eq!(data2[0], (far_offset % 256) as u8);
        }
    }

    // =========================================================================
    // Edge Cases with Large Files
    // =========================================================================

    #[test]
    fn large_file_exact_page_boundaries() {
        // Test access at 4KB page boundaries
        let size = 4 * 1024 * 1024; // 4MB
        let temp = create_test_file(size);
        let mut map = MapFile::open_mmap(temp.path()).unwrap();

        let page_size = 4096;
        for page in 0..(size / page_size) {
            let offset = page * page_size;
            if offset + 100 > size {
                break;
            }
            let data = map.map_ptr(offset as u64, 100).unwrap();
            assert_eq!(data[0], (offset % 256) as u8);
        }
    }

    #[test]
    fn large_file_unaligned_access_mmap() {
        // Test unaligned offsets with mmap
        let size = 2 * 1024 * 1024; // 2MB
        let temp = create_test_file(size);
        let mut map = MapFile::open_mmap(temp.path()).unwrap();

        // Various unaligned offsets
        let offsets = [1, 3, 7, 13, 127, 1023, 4095, 8191];
        for &offset in &offsets {
            let data = map.map_ptr(offset, 100).unwrap();
            assert_eq!(data[0], (offset % 256) as u8);
        }
    }

    #[test]
    fn large_file_single_byte_reads_across_file() {
        // Reading single bytes across a large file
        let size = 1024 * 1024; // 1MB
        let temp = create_test_file(size);
        let mut map = MapFile::open_mmap(temp.path()).unwrap();

        // Sample single bytes at regular intervals
        for offset in (0..size).step_by(16 * 1024) {
            let data = map.map_ptr(offset as u64, 1).unwrap();
            assert_eq!(data[0], (offset % 256) as u8);
        }
    }

    #[test]
    fn large_file_maximum_single_read_mmap() {
        // Read entire large file in one operation
        let size = 2 * 1024 * 1024; // 2MB
        let temp = create_test_file(size);
        let mut map = MapFile::open_mmap(temp.path()).unwrap();

        let data = map.map_ptr(0, size).unwrap();
        assert_eq!(data.len(), size);
        assert_eq!(data[0], 0);
        assert_eq!(data[size - 1], ((size - 1) % 256) as u8);
    }

    // =========================================================================
    // Strategy Conversion Tests
    // =========================================================================

    #[test]
    fn map_file_strategy_type_safety() {
        // Verify that different strategy types are distinct
        let temp = create_test_file(1000);

        let _buffered: MapFile<BufferedMap> = MapFile::open(temp.path()).unwrap();
        let _mmap: MapFile<MmapStrategy> = MapFile::open_mmap(temp.path()).unwrap();
        let _adaptive: MapFile<AdaptiveMapStrategy> = MapFile::open_adaptive(temp.path()).unwrap();

        // Type system ensures we can't mix them up
    }

    #[test]
    fn custom_strategy_with_map_file() {
        // Test MapFile::with_strategy for custom strategy instances
        let temp = create_test_file(1000);

        let strategy = BufferedMap::open_with_window(temp.path(), 512).unwrap();
        let mut map = MapFile::with_strategy(strategy);

        assert_eq!(map.window_size(), 512);
        let data = map.map_ptr(0, 100).unwrap();
        assert_eq!(data[0], 0);
    }

    // =========================================================================
    // Error Recovery Tests
    // =========================================================================

    #[test]
    fn mmap_after_error_recovery() {
        let temp = create_test_file(1000);
        let mut map = MapFile::open_mmap(temp.path()).unwrap();

        // Cause an error
        let err = map.map_ptr(2000, 100);
        assert!(err.is_err());

        // Should still work for valid requests
        let data = map.map_ptr(0, 100).unwrap();
        assert_eq!(data[0], 0);
    }

    #[test]
    fn buffered_after_error_recovery() {
        let temp = create_test_file(1000);
        let mut map = MapFile::open(temp.path()).unwrap();

        // Cause an error
        let err = map.map_ptr(2000, 100);
        assert!(err.is_err());

        // Should still work for valid requests
        let data = map.map_ptr(0, 100).unwrap();
        assert_eq!(data[0], 0);
    }

    #[test]
    fn adaptive_after_error_recovery() {
        let temp = create_test_file(1000);
        let mut map = MapFile::open_adaptive(temp.path()).unwrap();

        // Cause an error
        let err = map.map_ptr(2000, 100);
        assert!(err.is_err());

        // Should still work for valid requests
        let data = map.map_ptr(0, 100).unwrap();
        assert_eq!(data[0], 0);
    }

    // =========================================================================
    // Adaptive Map Strategy Selection Tests
    // =========================================================================

    #[test]
    fn adaptive_strategy_selection_small_file_uses_buffered() {
        // Files below MMAP_THRESHOLD should use buffered strategy
        let small_size = 512 * 1024; // 512 KB - well below 1MB threshold
        let temp = create_test_file(small_size);
        let strategy = AdaptiveMapStrategy::open(temp.path()).unwrap();

        assert!(strategy.is_buffered());
        assert!(!strategy.is_mmap());
        assert_eq!(strategy.file_size(), small_size as u64);
    }

    #[test]
    fn adaptive_strategy_selection_large_file_uses_mmap() {
        // Files at or above MMAP_THRESHOLD should use mmap strategy
        let large_size = 2 * 1024 * 1024; // 2 MB - above 1MB threshold
        let temp = create_test_file(large_size);
        let strategy = AdaptiveMapStrategy::open(temp.path()).unwrap();

        assert!(strategy.is_mmap());
        assert!(!strategy.is_buffered());
        assert_eq!(strategy.file_size(), large_size as u64);
    }

    #[test]
    fn adaptive_strategy_selection_boundary_below_threshold() {
        // File 1 byte below threshold should use buffered
        let size = (MMAP_THRESHOLD - 1) as usize;
        let temp = create_test_file(size);
        let strategy = AdaptiveMapStrategy::open(temp.path()).unwrap();

        assert!(
            strategy.is_buffered(),
            "File at {size} bytes (1 below threshold) should use buffered"
        );
    }

    #[test]
    fn adaptive_strategy_selection_boundary_exactly_at_threshold() {
        // File exactly at threshold should use mmap
        let size = MMAP_THRESHOLD as usize;
        let temp = create_test_file(size);
        let strategy = AdaptiveMapStrategy::open(temp.path()).unwrap();

        assert!(
            strategy.is_mmap(),
            "File at {size} bytes (exactly at threshold) should use mmap"
        );
    }

    #[test]
    fn adaptive_strategy_selection_boundary_above_threshold() {
        // File 1 byte above threshold should use mmap
        let size = (MMAP_THRESHOLD + 1) as usize;
        let temp = create_test_file(size);
        let strategy = AdaptiveMapStrategy::open(temp.path()).unwrap();

        assert!(
            strategy.is_mmap(),
            "File at {size} bytes (1 above threshold) should use mmap"
        );
    }

    #[test]
    fn adaptive_strategy_selection_empty_file_uses_buffered() {
        // Empty files (0 bytes) should use buffered strategy
        let temp = create_test_file(0);
        let strategy = AdaptiveMapStrategy::open(temp.path()).unwrap();

        assert!(strategy.is_buffered());
        assert_eq!(strategy.file_size(), 0);
    }

    #[test]
    fn adaptive_strategy_selection_tiny_file_uses_buffered() {
        // Very small files (1 byte) should use buffered strategy
        let temp = create_test_file(1);
        let strategy = AdaptiveMapStrategy::open(temp.path()).unwrap();

        assert!(strategy.is_buffered());
        assert_eq!(strategy.file_size(), 1);
    }

    #[test]
    fn adaptive_strategy_selection_custom_threshold_zero() {
        // With threshold 0, all files should use mmap
        let temp = create_test_file(100);
        let strategy = AdaptiveMapStrategy::open_with_threshold(temp.path(), 0).unwrap();

        assert!(strategy.is_mmap());
    }

    #[test]
    fn adaptive_strategy_selection_custom_threshold_max() {
        // With very large threshold, all reasonable files use buffered
        let temp = create_test_file(1000);
        let strategy = AdaptiveMapStrategy::open_with_threshold(temp.path(), u64::MAX).unwrap();

        assert!(strategy.is_buffered());
    }

    #[test]
    fn adaptive_strategy_selection_window_size_differs() {
        // Verify window_size() behavior differs between strategies
        let small_temp = create_test_file(1000);
        let large_temp = create_test_file((MMAP_THRESHOLD + 1024) as usize);

        let small_strategy = AdaptiveMapStrategy::open(small_temp.path()).unwrap();
        let large_strategy = AdaptiveMapStrategy::open(large_temp.path()).unwrap();

        // Buffered has MAX_MAP_SIZE window
        assert_eq!(small_strategy.window_size(), MAX_MAP_SIZE);

        // Mmap window is the entire file
        assert_eq!(
            large_strategy.window_size(),
            (MMAP_THRESHOLD + 1024) as usize
        );
    }

    #[test]
    fn adaptive_strategy_selection_data_consistency() {
        // Verify that both strategies return identical data for the same file
        let size = 10000;
        let temp = create_test_file(size);

        // Force buffered (high threshold)
        let mut buffered = AdaptiveMapStrategy::open_with_threshold(temp.path(), u64::MAX).unwrap();
        // Force mmap (low threshold)
        let mut mmap = AdaptiveMapStrategy::open_with_threshold(temp.path(), 0).unwrap();

        assert!(buffered.is_buffered());
        assert!(mmap.is_mmap());

        // Read same data from both and compare
        for offset in (0..size - 100).step_by(500) {
            let buf_data = buffered.map_ptr(offset as u64, 100).unwrap().to_vec();
            let mmap_data = mmap.map_ptr(offset as u64, 100).unwrap();

            assert_eq!(
                buf_data, mmap_data,
                "Data mismatch at offset {offset} between buffered and mmap strategies"
            );
        }
    }

    #[test]
    fn adaptive_strategy_selection_map_file_convenience_methods() {
        // Test MapFile convenience methods for adaptive strategy
        let small_temp = create_test_file(1000);
        let large_temp = create_test_file((MMAP_THRESHOLD + 1024) as usize);

        let small_map = MapFile::open_adaptive(small_temp.path()).unwrap();
        let large_map = MapFile::open_adaptive(large_temp.path()).unwrap();

        // Verify is_mmap() and is_buffered() work through MapFile
        assert!(small_map.is_buffered());
        assert!(!small_map.is_mmap());

        assert!(large_map.is_mmap());
        assert!(!large_map.is_buffered());
    }

    #[test]
    fn adaptive_strategy_selection_map_file_with_custom_threshold() {
        // Test MapFile::open_adaptive_with_threshold
        let temp = create_test_file(500);

        // With threshold 100, should use mmap
        let map_low = MapFile::open_adaptive_with_threshold(temp.path(), 100).unwrap();
        assert!(map_low.is_mmap());

        // With threshold 1000, should use buffered
        let map_high = MapFile::open_adaptive_with_threshold(temp.path(), 1000).unwrap();
        assert!(map_high.is_buffered());
    }

    #[test]
    fn adaptive_strategy_selection_multiple_threshold_boundaries() {
        // Test multiple boundary values to ensure consistent behavior
        let boundaries = [
            (100, 99, true, false),  // threshold=100, size=99 -> buffered
            (100, 100, false, true), // threshold=100, size=100 -> mmap
            (100, 101, false, true), // threshold=100, size=101 -> mmap
            (1, 0, true, false),     // threshold=1, size=0 -> buffered
            (1, 1, false, true),     // threshold=1, size=1 -> mmap
        ];

        for (threshold, size, expect_buffered, expect_mmap) in boundaries {
            let temp = create_test_file(size);
            let strategy =
                AdaptiveMapStrategy::open_with_threshold(temp.path(), threshold as u64).unwrap();

            assert_eq!(
                strategy.is_buffered(),
                expect_buffered,
                "threshold={threshold}, size={size}: expected is_buffered={expect_buffered}"
            );
            assert_eq!(
                strategy.is_mmap(),
                expect_mmap,
                "threshold={threshold}, size={size}: expected is_mmap={expect_mmap}"
            );
        }
    }

    #[test]
    fn adaptive_strategy_selection_read_after_strategy_check() {
        // Ensure strategy selection doesn't affect subsequent reads
        let temp = create_test_file(1000);

        let mut strategy = AdaptiveMapStrategy::open_with_threshold(temp.path(), 500).unwrap();
        assert!(strategy.is_mmap());

        // Read should still work correctly
        let data = strategy.map_ptr(0, 100).unwrap();
        assert_eq!(data.len(), 100);
        for (i, &byte) in data.iter().enumerate() {
            assert_eq!(byte, i as u8);
        }
    }

    #[test]
    fn adaptive_strategy_selection_file_size_preserved() {
        // Ensure file_size() works correctly for both strategies
        let sizes = [
            0,
            1,
            100,
            1000,
            MMAP_THRESHOLD as usize - 1,
            MMAP_THRESHOLD as usize,
            MMAP_THRESHOLD as usize + 1,
        ];

        for size in sizes {
            let temp = create_test_file(size);
            let strategy = AdaptiveMapStrategy::open(temp.path()).unwrap();

            assert_eq!(
                strategy.file_size(),
                size as u64,
                "File size mismatch for size={size}"
            );
        }
    }

    // =========================================================================
    // MmapStrategy::as_slice() Tests
    // =========================================================================

    #[test]
    fn mmap_as_slice_full_file() {
        let size = 1024;
        let temp = create_test_file(size);
        let strategy = MmapStrategy::open(temp.path()).unwrap();

        let slice = strategy.as_slice();
        assert_eq!(slice.len(), size);

        for (i, &byte) in slice.iter().enumerate() {
            assert_eq!(byte, (i % 256) as u8);
        }
    }

    #[test]
    fn mmap_as_slice_vs_map_ptr() {
        let size = 1000;
        let temp = create_test_file(size);
        let mut strategy = MmapStrategy::open(temp.path()).unwrap();

        let via_map_ptr = strategy.map_ptr(0, size).unwrap().to_vec();
        let slice = strategy.as_slice();

        assert_eq!(slice, &via_map_ptr[..]);
    }

    #[test]
    fn mmap_as_slice_empty_file() {
        let temp = create_test_file(0);
        let strategy = MmapStrategy::open(temp.path()).unwrap();

        let slice = strategy.as_slice();
        assert!(slice.is_empty());
    }
}
