//! Stub shared-ring module mirroring [`crate::io_uring::shared_ring`] on
//! non-Linux platforms or when the `io_uring` cargo feature is disabled.
//!
//! Every constructor returns `None` / `Unsupported`, so callers fall back to
//! the per-channel ring path or to standard buffered I/O.

pub use crate::io_uring_common::{OpTag, SharedCompletion, SharedRingConfig};
use std::io;
use std::os::raw::c_int;

/// Stub `SharedRing`. All constructors return `Unsupported` / `None`.
pub struct SharedRing {
    _private: (),
}

impl SharedRing {
    /// Always returns `None` on this platform.
    #[must_use]
    pub fn try_new(
        _reader_fd: c_int,
        _writer_fd: c_int,
        _config: &SharedRingConfig,
    ) -> Option<Self> {
        None
    }

    /// Always returns `Unsupported` on this platform.
    pub fn new(
        _reader_fd: c_int,
        _writer_fd: c_int,
        _config: &SharedRingConfig,
    ) -> io::Result<Self> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "io_uring shared ring is not available on this platform",
        ))
    }

    /// Always returns `false` on this platform.
    #[must_use]
    pub fn poll_add_supported(&self) -> bool {
        false
    }

    /// Always returns `false` on this platform.
    #[must_use]
    pub fn has_registered_buffers(&self) -> bool {
        false
    }

    /// Always returns `-1` on this platform.
    #[must_use]
    pub fn reader_slot(&self) -> i32 {
        -1
    }

    /// Always returns `-1` on this platform.
    #[must_use]
    pub fn writer_slot(&self) -> i32 {
        -1
    }

    /// Always returns `Unsupported` on this platform.
    pub fn submit_read(&mut self, _op_id: u64, _offset: u64, _buf: &mut [u8]) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "io_uring shared ring is not available on this platform",
        ))
    }

    /// Always returns `Unsupported` on this platform.
    pub fn submit_poll_write(&mut self, _op_id: u64) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "io_uring shared ring is not available on this platform",
        ))
    }

    /// Always returns `Unsupported` on this platform.
    pub fn submit_send(&mut self, _op_id: u64, _data: &[u8]) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "io_uring shared ring is not available on this platform",
        ))
    }

    /// Always returns `Unsupported` on this platform.
    pub fn submit_and_wait(&mut self, _wait_for: usize) -> io::Result<usize> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "io_uring shared ring is not available on this platform",
        ))
    }

    /// Always returns an empty vector on this platform.
    pub fn reap(&mut self) -> io::Result<Vec<SharedCompletion>> {
        Ok(Vec::new())
    }
}
