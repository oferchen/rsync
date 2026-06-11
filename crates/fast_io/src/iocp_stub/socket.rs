//! Cross-platform stub for the Windows-only `iocp::socket` module (issue #1928).
//!
//! Mirrors the public surface of [`crate::iocp::socket`] on Windows so code
//! that names `IocpSocketReader` / `IocpSocketWriter` behind a runtime check
//! against [`super::is_iocp_available`] still compiles on Linux and macOS.
//! All constructors and methods return [`std::io::ErrorKind::Unsupported`].

use std::io;
use std::sync::Arc;

use super::CompletionPump;

/// Shared completion-pump reference - matches the Windows alias so
/// downstream APIs keep their signatures unchanged across platforms.
pub type SharedPump = Arc<CompletionPump>;

/// Stub IOCP socket reader. Construction always fails with
/// [`std::io::ErrorKind::Unsupported`].
pub struct IocpSocketReader {
    _private: (),
}

impl IocpSocketReader {
    /// Returns a stub instance for type compatibility - never used because
    /// the pump cannot be constructed on this platform. Consumers behind
    /// a runtime IOCP check never reach here.
    #[must_use]
    pub fn from_raw_socket(_socket: u64, _pump: SharedPump) -> Self {
        Self { _private: () }
    }

    /// Returns `Unsupported` on this platform.
    pub fn associate(_socket: u64, _pump: SharedPump) -> io::Result<Self> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "IOCP socket reader is not available on this platform",
        ))
    }

    /// Returns `Unsupported` on this platform.
    pub fn recv_async(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "IOCP recv_async is not available on this platform",
        ))
    }

    /// Override the per-socket completion key reported by the pump.
    #[must_use]
    pub fn with_completion_key(self, _key: usize) -> Self {
        self
    }

    /// Returns the completion key (always `0` on this platform).
    #[must_use]
    pub fn completion_key(&self) -> usize {
        0
    }
}

/// Stub IOCP socket writer. Construction always fails with
/// [`std::io::ErrorKind::Unsupported`].
pub struct IocpSocketWriter {
    _private: (),
}

impl IocpSocketWriter {
    /// Returns a stub instance for type compatibility.
    #[must_use]
    pub fn from_raw_socket(_socket: u64, _pump: SharedPump) -> Self {
        Self { _private: () }
    }

    /// Returns `Unsupported` on this platform.
    pub fn associate(_socket: u64, _pump: SharedPump) -> io::Result<Self> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "IOCP socket writer is not available on this platform",
        ))
    }

    /// Returns `Unsupported` on this platform.
    pub fn send_async(&mut self, _buf: &[u8]) -> io::Result<usize> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "IOCP send_async is not available on this platform",
        ))
    }

    /// Override the per-socket completion key reported by the pump.
    #[must_use]
    pub fn with_completion_key(self, _key: usize) -> Self {
        self
    }

    /// Returns the completion key (always `0` on this platform).
    #[must_use]
    pub fn completion_key(&self) -> usize {
        0
    }
}
