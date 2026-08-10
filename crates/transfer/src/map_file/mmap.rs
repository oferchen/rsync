//! Memory-mapped file strategy.
//!
//! Uses `fast_io::MmapReader` to map the entire file into memory, providing
//! zero-copy access to file contents. Most effective for large files (> 1 MB),
//! random access patterns, and read-only access.
//!
//! # Safety
//!
//! Memory-mapped files can cause undefined behavior if the underlying file
//! is modified while mapped. This implementation is safe as long as the
//! basis file is not modified during delta application.

use std::io;
use std::path::Path;

use fast_io::{FileReader, MmapReader};

use super::MapStrategy;

/// Memory-mapped file mapper for efficient large file access.
///
/// Maps the entire file into memory via `fast_io::MmapReader`, providing
/// zero-copy access without window sliding overhead.
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
    pub fn open<P: AsRef<Path>>(path: P) -> io::Result<Self> {
        Ok(Self {
            mmap: MmapReader::open(path)?,
        })
    }

    /// Returns the underlying memory map as a slice.
    #[must_use]
    pub fn as_slice(&self) -> &[u8] {
        self.mmap.as_slice()
    }
}

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
        self.mmap.size() as usize
    }

    #[inline]
    fn file_size(&self) -> u64 {
        self.mmap.size()
    }
}
