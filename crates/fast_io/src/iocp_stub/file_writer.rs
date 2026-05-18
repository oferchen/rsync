//! Stub IOCP file writer.
//!
//! Mirrors the public surface of [`crate::iocp::file_writer`] so cross-platform
//! callers can name [`IocpWriter`] behind a runtime IOCP availability check
//! without `#[cfg]` branching. On this platform every constructor and method
//! returns [`io::ErrorKind::Unsupported`].

use std::io::{self, Seek, SeekFrom, Write};
use std::path::Path;

use crate::traits::FileWriter;

use super::IocpConfig;

/// Stub IOCP writer (not available on this platform).
///
/// Creating always fails with `Unsupported`.
pub struct IocpWriter {
    _private: (),
}

impl IocpWriter {
    /// Always returns an `Unsupported` error on this platform.
    pub fn create<P: AsRef<Path>>(_path: P, _config: &IocpConfig) -> io::Result<Self> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "IOCP is not available on this platform",
        ))
    }

    /// Creates a file with preallocated space (always fails on this platform).
    pub fn create_with_size<P: AsRef<Path>>(
        _path: P,
        _size: u64,
        _config: &IocpConfig,
    ) -> io::Result<Self> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "IOCP is not available on this platform",
        ))
    }
}

impl Write for IocpWriter {
    fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "IOCP is not available on this platform",
        ))
    }

    fn flush(&mut self) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "IOCP is not available on this platform",
        ))
    }
}

impl Seek for IocpWriter {
    fn seek(&mut self, _pos: SeekFrom) -> io::Result<u64> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "IOCP is not available on this platform",
        ))
    }
}

impl FileWriter for IocpWriter {
    fn bytes_written(&self) -> u64 {
        0
    }

    fn sync(&mut self) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "IOCP is not available on this platform",
        ))
    }

    fn preallocate(&mut self, _size: u64) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "IOCP is not available on this platform",
        ))
    }
}
