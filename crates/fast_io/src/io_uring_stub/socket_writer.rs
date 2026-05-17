//! Stub io_uring socket writer mirroring
//! [`crate::io_uring::socket_writer::IoUringSocketWriter`]. Not available on
//! this platform.

use crate::io_uring_common::IoUringConfig;
use std::io::{self, Write};
use std::os::unix::io::RawFd;

/// Stub io_uring socket writer (not available on this platform).
pub struct IoUringSocketWriter {
    _private: (),
}

impl IoUringSocketWriter {
    /// Always returns an `Unsupported` error on this platform.
    pub fn from_raw_fd(_fd: RawFd, _config: &IoUringConfig) -> io::Result<Self> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "io_uring is not available on this platform",
        ))
    }
}

impl Write for IoUringSocketWriter {
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
