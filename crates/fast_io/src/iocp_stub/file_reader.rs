//! Stub IOCP file reader.
//!
//! Mirrors the public surface of [`crate::iocp::file_reader`] so cross-platform
//! callers can name [`IocpReader`] behind a runtime IOCP availability check
//! without `#[cfg]` branching. On this platform every constructor and method
//! returns [`io::ErrorKind::Unsupported`].

use std::io::{self, Read};
use std::path::Path;

use crate::traits::FileReader;

use super::IocpConfig;

/// Stub IOCP reader (not available on this platform).
///
/// Opening always fails with `Unsupported`.
pub struct IocpReader {
    _private: (),
}

impl IocpReader {
    /// Always returns an `Unsupported` error on this platform.
    pub fn open<P: AsRef<Path>>(_path: P, _config: &IocpConfig) -> io::Result<Self> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "IOCP is not available on this platform",
        ))
    }

    /// Reads data at the specified offset.
    pub fn read_at(&mut self, _offset: u64, _buf: &mut [u8]) -> io::Result<usize> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "IOCP is not available on this platform",
        ))
    }

    /// Reads the entire file into a vector.
    pub fn read_all_batched(&mut self) -> io::Result<Vec<u8>> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "IOCP is not available on this platform",
        ))
    }
}

impl Read for IocpReader {
    fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "IOCP is not available on this platform",
        ))
    }
}

impl FileReader for IocpReader {
    fn size(&self) -> u64 {
        0
    }

    fn position(&self) -> u64 {
        0
    }

    fn seek_to(&mut self, _pos: u64) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "IOCP is not available on this platform",
        ))
    }

    fn read_all(&mut self) -> io::Result<Vec<u8>> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "IOCP is not available on this platform",
        ))
    }
}
