//! Stub batched IOCP disk writer.
//!
//! Mirrors the public surface of [`crate::iocp::disk_batch`] so cross-platform
//! callers can name [`IocpDiskBatch`] behind a runtime [`is_iocp_available`]
//! check without `#[cfg]` branching. On this platform construction always
//! fails with [`io::ErrorKind::Unsupported`] and the bounce-copy counter is
//! permanently zero.
//!
//! [`is_iocp_available`]: super::is_iocp_available

use std::io::{self, Write};

use super::IocpConfig;

/// Stub batched IOCP disk writer (not available on this platform).
///
/// On non-Windows platforms, [`try_new`](Self::try_new) always returns `None`
/// and [`new`](Self::new) always returns `Unsupported`. Mirrors the public
/// surface of the Windows [`IocpDiskBatch`](crate::iocp::IocpDiskBatch) so
/// cross-platform code that names the type behind a runtime
/// [`is_iocp_available`](super::is_iocp_available) check still compiles.
#[derive(Debug)]
pub struct IocpDiskBatch {
    _private: (),
}

impl IocpDiskBatch {
    /// Always returns an `Unsupported` error on this platform.
    pub fn new(_config: &IocpConfig) -> io::Result<Self> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "IOCP batched disk writer is not available on this platform",
        ))
    }

    /// Always returns `None` on this platform.
    pub fn try_new(_config: &IocpConfig) -> Option<Self> {
        None
    }

    /// Begins a new file for writing (always fails on this platform).
    pub fn begin_file(&mut self, _file: std::fs::File) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "IOCP is not available on this platform",
        ))
    }

    /// Writes data to the current file (always fails on this platform).
    pub fn write_data(&mut self, _data: &[u8]) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "IOCP is not available on this platform",
        ))
    }

    /// Flushes buffered data (always fails on this platform).
    pub fn flush(&mut self) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "IOCP is not available on this platform",
        ))
    }

    /// Commits the current file (always fails on this platform).
    pub fn commit_file(&mut self, _do_fsync: bool) -> io::Result<(std::fs::File, u64)> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "IOCP is not available on this platform",
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

    /// Returns whether the internal buffer is page-aligned (always `false`
    /// on this platform because no batch is ever constructed).
    #[must_use]
    pub fn buffer_is_page_aligned(&self) -> bool {
        false
    }
}

/// Returns the cumulative count of bounce-buffer copies avoided by the
/// page-aligned IOCP write path. Always `0` on this platform because IOCP
/// is Windows-only.
#[must_use]
pub fn bounce_copies_avoided() -> u64 {
    0
}

/// Resets the process-wide bounce-copy counter (no-op on this platform).
#[doc(hidden)]
pub fn reset_bounce_copies_avoided_for_test() {}

impl Write for IocpDiskBatch {
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
