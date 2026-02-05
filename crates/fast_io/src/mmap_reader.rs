//! Memory-mapped file reader for efficient large file access.
//!
//! Uses the `memmap2` crate to map files directly into memory, avoiding
//! read syscalls and enabling zero-copy access to file contents.
//!
//! # When to Use
//!
//! Memory mapping is most effective for:
//! - Large files (> 1 MB)
//! - Random access patterns
//! - Read-only access
//! - When multiple processes read the same file
//!
//! For small files or sequential access, standard buffered I/O may be faster
//! due to reduced setup overhead.
//!
//! # Safety
//!
//! Memory-mapped files can cause undefined behavior if the underlying file
//! is modified while mapped. This implementation uses read-only mappings
//! and is safe as long as the file is not modified externally.

use crate::traits::{FileReader, FileReaderFactory};
use memmap2::{Mmap, MmapOptions};
use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

/// Threshold above which memory mapping is preferred over buffered I/O.
pub const MMAP_THRESHOLD: u64 = 1024 * 1024; // 1 MB

/// A memory-mapped file reader.
///
/// Provides efficient read access to file contents by mapping the file
/// directly into the process address space.
///
/// # Example
///
/// ```ignore
/// use fast_io::MmapReader;
///
/// let reader = MmapReader::open("large_file.bin")?;
/// println!("File size: {} bytes", reader.size());
///
/// // Access bytes directly
/// let first_1k = &reader.as_slice()[..1024];
/// ```
pub struct MmapReader {
    mmap: Mmap,
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
    /// Opens a file for memory-mapped reading.
    ///
    /// # Safety
    ///
    /// The file must not be modified while the `MmapReader` exists.
    /// Modifying the file can cause undefined behavior.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be opened or mapped.
    pub fn open<P: AsRef<Path>>(path: P) -> io::Result<Self> {
        let file = File::open(path)?;
        let size = file.metadata()?.len();

        // SAFETY: We assume the file won't be modified while mapped.
        // This is a common assumption for rsync-style file transfer
        // where source files are treated as immutable during transfer.
        let mmap = unsafe { MmapOptions::new().map(&file)? };

        Ok(Self {
            mmap,
            position: 0,
            size,
        })
    }

    /// Returns the file contents as a byte slice.
    ///
    /// This is a zero-copy operation - no data is copied from the kernel.
    #[must_use]
    pub fn as_slice(&self) -> &[u8] {
        &self.mmap
    }

    /// Returns a slice of the file starting at the given offset.
    ///
    /// # Panics
    ///
    /// Panics if `offset` is greater than the file size.
    #[must_use]
    pub fn slice_from(&self, offset: usize) -> &[u8] {
        &self.mmap[offset..]
    }

    /// Returns a slice of the file from `start` to `end`.
    ///
    /// # Panics
    ///
    /// Panics if the range is out of bounds.
    #[must_use]
    pub fn slice_range(&self, start: usize, end: usize) -> &[u8] {
        &self.mmap[start..end]
    }

    /// Advises the kernel about access patterns.
    ///
    /// This is a hint to the kernel for prefetching.
    #[cfg(unix)]
    pub fn advise_sequential(&self) -> io::Result<()> {
        self.mmap.advise(memmap2::Advice::Sequential)?;
        Ok(())
    }

    /// Advises the kernel that the file will be accessed randomly.
    #[cfg(unix)]
    pub fn advise_random(&self) -> io::Result<()> {
        self.mmap.advise(memmap2::Advice::Random)?;
        Ok(())
    }

    /// Advises the kernel to prefetch the specified range.
    #[cfg(unix)]
    pub fn advise_willneed(&self, offset: usize, len: usize) -> io::Result<()> {
        self.mmap
            .advise_range(memmap2::Advice::WillNeed, offset, len)?;
        Ok(())
    }
}

impl Read for MmapReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let remaining = self.mmap.len() - self.position;
        let to_read = buf.len().min(remaining);

        if to_read == 0 {
            return Ok(0);
        }

        buf[..to_read].copy_from_slice(&self.mmap[self.position..self.position + to_read]);
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
        // For mmap, we can just clone the slice
        Ok(self.mmap[self.position..].to_vec())
    }
}

/// Factory that chooses between mmap and standard I/O based on file size.
///
/// Uses memory mapping for files larger than `MMAP_THRESHOLD`, and
/// standard buffered I/O for smaller files.
#[derive(Debug, Clone, Copy)]
pub struct AdaptiveReaderFactory {
    /// Threshold in bytes above which to use mmap.
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

/// Reader that can be either mmap or standard I/O.
pub enum AdaptiveReader {
    /// Memory-mapped reader for large files.
    Mmap(MmapReader),
    /// Standard buffered reader for small files.
    Std(crate::traits::StdFileReader),
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
        let metadata = std::fs::metadata(path)?;
        let size = metadata.len();

        if size >= self.threshold {
            Ok(AdaptiveReader::Mmap(MmapReader::open(path)?))
        } else {
            Ok(AdaptiveReader::Std(crate::traits::StdFileReader::open(
                path,
            )?))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::tempdir;

    #[test]
    fn mmap_reader_basic() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.txt");
        std::fs::write(&path, b"hello world").unwrap();

        let reader = MmapReader::open(&path).unwrap();
        assert_eq!(reader.size(), 11);
        assert_eq!(reader.as_slice(), b"hello world");
    }

    #[test]
    fn mmap_reader_read_trait() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.txt");
        std::fs::write(&path, b"hello world").unwrap();

        let mut reader = MmapReader::open(&path).unwrap();
        let mut buf = [0u8; 5];

        assert_eq!(reader.read(&mut buf).unwrap(), 5);
        assert_eq!(&buf, b"hello");
        assert_eq!(reader.position(), 5);

        assert_eq!(reader.read(&mut buf).unwrap(), 5);
        assert_eq!(&buf, b" worl");
    }

    #[test]
    fn mmap_reader_seek() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.txt");
        std::fs::write(&path, b"hello world").unwrap();

        let mut reader = MmapReader::open(&path).unwrap();
        reader.seek_to(6).unwrap();

        let mut buf = [0u8; 5];
        reader.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, b"world");
    }

    #[test]
    fn adaptive_factory_chooses_correctly() {
        let dir = tempdir().unwrap();

        // Small file -> Std
        let small_path = dir.path().join("small.txt");
        std::fs::write(&small_path, b"small").unwrap();

        let factory = AdaptiveReaderFactory::with_threshold(100);
        let reader = factory.open(&small_path).unwrap();
        assert!(matches!(reader, AdaptiveReader::Std(_)));

        // Large file -> Mmap
        let large_path = dir.path().join("large.txt");
        {
            let mut f = File::create(&large_path).unwrap();
            f.write_all(&[0u8; 200]).unwrap();
        }

        let reader = factory.open(&large_path).unwrap();
        assert!(matches!(reader, AdaptiveReader::Mmap(_)));
    }
}
