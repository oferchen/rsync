//! Stub io_uring reader mirroring [`crate::io_uring::file_reader::IoUringReader`].
//! Opening always fails with `Unsupported`.

use crate::io_uring_common::IoUringConfig;
use crate::traits::FileReader;
use std::io::{self, Read};
use std::path::Path;

/// Stub io_uring reader (not available on this platform).
///
/// Opening always fails with `Unsupported`.
pub struct IoUringReader {
    _private: (),
}

impl IoUringReader {
    /// Always returns an `Unsupported` error on this platform.
    pub fn open<P: AsRef<Path>>(_path: P, _config: &IoUringConfig) -> io::Result<Self> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "io_uring is not available on this platform",
        ))
    }

    /// Reads data at the specified offset (always fails on this platform).
    pub fn read_at(&mut self, _offset: u64, _buf: &mut [u8]) -> io::Result<usize> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "io_uring is not available on this platform",
        ))
    }

    /// Reads the entire file into a vector (always fails on this platform).
    pub fn read_all_batched(&mut self) -> io::Result<Vec<u8>> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "io_uring is not available on this platform",
        ))
    }
}

impl Read for IoUringReader {
    fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "io_uring is not available on this platform",
        ))
    }
}

impl FileReader for IoUringReader {
    fn size(&self) -> u64 {
        0
    }

    fn position(&self) -> u64 {
        0
    }

    fn seek_to(&mut self, _pos: u64) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "io_uring is not available on this platform",
        ))
    }

    fn read_all(&mut self) -> io::Result<Vec<u8>> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "io_uring is not available on this platform",
        ))
    }
}
