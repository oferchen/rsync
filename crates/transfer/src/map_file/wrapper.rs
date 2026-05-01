//! High-level `MapFile` wrapper over mapping strategies.
//!
//! Provides a convenient generic API for delta application code,
//! parameterized by the underlying `MapStrategy` implementation.

use std::fs::File;
use std::io;
use std::path::Path;

use super::MapStrategy;
use super::buffered::BufferedMap;
#[cfg(unix)]
use super::{adaptive::AdaptiveMapStrategy, mmap::MmapStrategy};

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

    /// Creates a `MapFile` from an already-open file.
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

    /// Opens a file forcing the buffered (non-mmap) variant under the
    /// `AdaptiveMapStrategy` enum.
    ///
    /// Used when the basis file must not be mmap-backed - notably when
    /// the destination writer is io_uring-backed, where mmap'd pages reaching
    /// an SQE can stall the SQPOLL kernel thread on cold-page faults or
    /// raise `SIGBUS` inside the kernel on concurrent truncation. See
    /// `docs/design/basis-file-io-policy.md`.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be opened.
    pub fn open_adaptive_buffered<P: AsRef<Path>>(path: P) -> io::Result<Self> {
        Ok(Self {
            strategy: AdaptiveMapStrategy::open_buffered(path)?,
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
    /// Creates a `MapFile` with a custom strategy.
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
