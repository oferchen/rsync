//! Adaptive file mapping strategy.
//!
//! Automatically selects between buffered I/O and memory mapping based on
//! file size, providing optimal performance across different file sizes.

use std::io;
use std::path::Path;

use super::buffered::BufferedMap;
use super::mmap::MmapStrategy;
use super::{MMAP_THRESHOLD, MapStrategy};

/// Adaptive file mapper that selects the optimal strategy based on file size.
///
/// Uses memory mapping for files larger than `MMAP_THRESHOLD` (1 MB) and
/// buffered I/O for smaller files:
///
/// - **Small files (< 1 MB)**: Buffered I/O avoids mmap setup overhead
/// - **Large files (>= 1 MB)**: Mmap provides zero-copy access
#[derive(Debug)]
pub enum AdaptiveMapStrategy {
    /// Buffered I/O for small files.
    Buffered(BufferedMap),
    /// Memory-mapped I/O for large files.
    Mmap(MmapStrategy),
}

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

    /// Opens a file forcing the buffered (non-mmap) variant regardless of size.
    ///
    /// Used when the basis file pointer must never reach an io_uring submission
    /// (cold-page faults can stall the SQPOLL kernel thread, and concurrent
    /// truncation raises `SIGBUS` inside the kernel SQE service path).
    /// See `docs/design/basis-file-io-policy.md` and audit
    /// `docs/audits/mmap-iouring-co-usage.md` finding F1.
    ///
    /// Mirrors upstream rsync's deliberate avoidance of `mmap(2)` for basis
    /// files (`fileio.c:214-217`).
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be opened.
    pub fn open_buffered<P: AsRef<Path>>(path: P) -> io::Result<Self> {
        Ok(Self::Buffered(BufferedMap::open(path)?))
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
