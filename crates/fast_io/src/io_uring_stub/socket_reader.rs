//! Stub io_uring socket reader mirroring
//! [`crate::io_uring::socket_reader::IoUringSocketReader`]. Not available on
//! this platform.

use crate::io_uring_common::IoUringConfig;
use std::io::{self, Read};
use std::os::unix::io::RawFd;

/// Stub io_uring socket reader (not available on this platform).
pub struct IoUringSocketReader {
    _private: (),
}

impl IoUringSocketReader {
    /// Always returns an `Unsupported` error on this platform.
    pub fn from_raw_fd(_fd: RawFd, _config: &IoUringConfig) -> io::Result<Self> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "io_uring is not available on this platform",
        ))
    }
}

impl Read for IoUringSocketReader {
    fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "io_uring is not available on this platform",
        ))
    }
}
