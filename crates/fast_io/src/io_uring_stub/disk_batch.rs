//! Stub batched io_uring disk writer mirroring
//! [`crate::io_uring::disk_batch::IoUringDiskBatch`]. Not available on this
//! platform.

use crate::io_uring_common::IoUringConfig;
use std::io::{self, Write};

/// Stub batched io_uring disk writer (not available on this platform).
#[derive(Debug)]
pub struct IoUringDiskBatch {
    _private: (),
}

impl IoUringDiskBatch {
    /// Always returns an `Unsupported` error on this platform.
    pub fn new(_config: &IoUringConfig) -> io::Result<Self> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "io_uring batched disk writer is not available on this platform",
        ))
    }

    /// Always returns `None` on this platform.
    #[must_use]
    pub fn try_new(_config: &IoUringConfig) -> Option<Self> {
        None
    }

    /// Begins a new file for writing (always fails on this platform).
    pub fn begin_file(&mut self, _file: std::fs::File) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "io_uring is not available on this platform",
        ))
    }

    /// Writes data to the current file (always fails on this platform).
    pub fn write_data(&mut self, _data: &[u8]) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "io_uring is not available on this platform",
        ))
    }

    /// Flushes buffered data (always fails on this platform).
    pub fn flush(&mut self) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "io_uring is not available on this platform",
        ))
    }

    /// Commits the current file (always fails on this platform).
    pub fn commit_file(&mut self, _do_fsync: bool) -> io::Result<(std::fs::File, u64)> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "io_uring is not available on this platform",
        ))
    }

    /// Returns bytes written (always 0 on this platform).
    #[must_use]
    pub fn bytes_written(&self) -> u64 {
        0
    }

    /// Returns bytes written including pending buffer (always 0 on this platform).
    #[must_use]
    pub fn bytes_written_with_pending(&self) -> u64 {
        0
    }
}

impl Write for IoUringDiskBatch {
    fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "io_uring is not available on this platform",
        ))
    }

    fn flush(&mut self) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "io_uring is not available on this platform",
        ))
    }
}
