//! Portable file reader fallback for non-Unix platforms.
//!
//! Provides the same public API as `mmap_reader` but uses standard
//! buffered I/O instead of memory-mapped files.

#![allow(dead_code)]

use crate::traits::{FileReader, FileReaderFactory, StdFileReader};
use std::fs::File;
use std::io::{self, BufReader, Read};
use std::path::Path;

/// Threshold above which memory mapping would be preferred (informational only).
pub const MMAP_THRESHOLD: u64 = 64 * 1024;

/// A buffered file reader (fallback for platforms without mmap).
///
/// On non-Unix platforms, this uses standard buffered I/O instead of
/// memory mapping.
pub struct MmapReader {
    data: Vec<u8>,
    position: usize,
    size: u64,
}

impl std::fmt::Debug for MmapReader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MmapReader")
            .field("position", &self.position)
            .field("size", &self.size)
            .finish_non_exhaustive()
    }
}

impl MmapReader {
    /// Opens a file for reading, loading contents into memory.
    ///
    /// On non-Unix platforms, the file is read entirely into a buffer.
    pub fn open<P: AsRef<Path>>(path: P) -> io::Result<Self> {
        let file = File::open(path)?;
        let size = file.metadata()?.len();
        let mut reader = BufReader::new(file);
        let mut data = Vec::with_capacity(size as usize);
        reader.read_to_end(&mut data)?;

        Ok(Self {
            data,
            position: 0,
            size,
        })
    }

    /// Returns the file contents as a byte slice.
    #[must_use]
    pub fn as_slice(&self) -> &[u8] {
        &self.data
    }

    /// Returns a slice of the file starting at the given offset.
    #[must_use]
    pub fn slice_from(&self, offset: usize) -> &[u8] {
        &self.data[offset..]
    }

    /// Returns a slice of the file from `start` to `end`.
    #[must_use]
    pub fn slice_range(&self, start: usize, end: usize) -> &[u8] {
        &self.data[start..end]
    }

    /// No-op on non-Unix platforms (madvise is not available).
    pub fn advise_sequential(&self) -> io::Result<()> {
        Ok(())
    }

    /// No-op on non-Unix platforms.
    pub fn advise_random(&self) -> io::Result<()> {
        Ok(())
    }

    /// No-op on non-Unix platforms.
    pub fn advise_willneed(&self, _offset: usize, _len: usize) -> io::Result<()> {
        Ok(())
    }
}

impl Read for MmapReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let remaining = self.data.len() - self.position;
        let to_read = buf.len().min(remaining);

        if to_read == 0 {
            return Ok(0);
        }

        buf[..to_read].copy_from_slice(&self.data[self.position..self.position + to_read]);
        self.position += to_read;

        Ok(to_read)
    }
}

impl FileReader for MmapReader {
    fn size(&self) -> u64 {
        self.size
    }

    fn position(&self) -> u64 {
        self.position as u64
    }

    fn seek_to(&mut self, pos: u64) -> io::Result<()> {
        if pos > self.size {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "seek position beyond end of file",
            ));
        }
        self.position = pos as usize;
        Ok(())
    }

    fn read_all(&mut self) -> io::Result<Vec<u8>> {
        Ok(self.data[self.position..].to_vec())
    }
}

/// Factory that always uses standard buffered I/O on non-Unix platforms.
#[derive(Debug, Clone, Copy)]
pub struct AdaptiveReaderFactory {
    /// Threshold in bytes (informational only on non-Unix).
    pub threshold: u64,
}

impl Default for AdaptiveReaderFactory {
    fn default() -> Self {
        Self {
            threshold: MMAP_THRESHOLD,
        }
    }
}

impl AdaptiveReaderFactory {
    /// Creates a factory with a custom threshold.
    #[must_use]
    pub fn with_threshold(threshold: u64) -> Self {
        Self { threshold }
    }
}

/// Reader type (always uses buffered I/O on non-Unix platforms).
pub enum AdaptiveReader {
    /// Memory-mapped reader (actually buffered on non-Unix).
    Mmap(MmapReader),
    /// Standard buffered reader.
    Std(StdFileReader),
}

impl Read for AdaptiveReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            AdaptiveReader::Mmap(r) => r.read(buf),
            AdaptiveReader::Std(r) => r.read(buf),
        }
    }
}

impl FileReader for AdaptiveReader {
    fn size(&self) -> u64 {
        match self {
            AdaptiveReader::Mmap(r) => r.size(),
            AdaptiveReader::Std(r) => r.size(),
        }
    }

    fn position(&self) -> u64 {
        match self {
            AdaptiveReader::Mmap(r) => r.position(),
            AdaptiveReader::Std(r) => r.position(),
        }
    }

    fn seek_to(&mut self, pos: u64) -> io::Result<()> {
        match self {
            AdaptiveReader::Mmap(r) => r.seek_to(pos),
            AdaptiveReader::Std(r) => r.seek_to(pos),
        }
    }
}

impl FileReaderFactory for AdaptiveReaderFactory {
    type Reader = AdaptiveReader;

    fn open(&self, path: &Path) -> io::Result<Self::Reader> {
        Ok(AdaptiveReader::Std(StdFileReader::open(path)?))
    }
}
