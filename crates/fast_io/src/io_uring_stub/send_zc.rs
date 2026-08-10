//! Stub `IORING_OP_SEND_ZC` module mirroring [`crate::io_uring::send_zc`] on
//! non-Linux platforms or when the `io_uring` cargo feature is disabled.
//!
//! `is_supported` always returns `false` and `try_send_zc` always returns
//! `Unsupported`. This keeps cross-platform call sites (and the socket
//! writer's fallback path) compiling without `cfg`-gating each reference.
//!
//! The `ZeroCopySender` type is gated on the `iouring-send-zc` feature
//! and mirrors the Linux constructor surface so cross-platform code can
//! reference the type behind the feature flag; every method returns
//! `io::ErrorKind::Unsupported`.

use std::io;
#[cfg(unix)]
use std::os::unix::io::RawFd;
#[cfg(not(unix))]
type RawFd = std::os::raw::c_int;

/// Always returns `false` on this platform.
#[must_use]
pub fn is_supported() -> bool {
    false
}

/// Always returns `Unsupported` on this platform.
pub fn try_send_zc(_ring: &mut (), _fd: RawFd, _buf: &[u8], _user_data: u64) -> io::Result<usize> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "IORING_OP_SEND_ZC is not available on this platform",
    ))
}

/// Stub dispatch threshold; matches the Linux value so cross-platform
/// callers see a single constant. Unused on this platform because every
/// [`ZeroCopySender`] method returns [`io::ErrorKind::Unsupported`].
#[cfg(feature = "iouring-send-zc")]
pub const SEND_ZC_DISPATCH_MIN_BYTES: usize = 4 * 1024;

/// Stub mirror of [`crate::io_uring::send_zc::ZeroCopySender`].
///
/// Every constructor and method returns [`io::ErrorKind::Unsupported`] so
/// cross-platform call sites compile against the same type but never
/// route real traffic through the stub.
#[cfg(feature = "iouring-send-zc")]
pub struct ZeroCopySender {
    _fd: RawFd,
}

#[cfg(feature = "iouring-send-zc")]
impl ZeroCopySender {
    /// Always returns [`io::ErrorKind::Unsupported`] on this platform.
    pub fn new(_fd: RawFd) -> io::Result<Self> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "IORING_OP_SEND_ZC is not available on this platform",
        ))
    }

    /// Always returns [`io::ErrorKind::Unsupported`] on this platform.
    pub fn send_zc(&mut self, _buf: &[u8]) -> io::Result<usize> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "IORING_OP_SEND_ZC is not available on this platform",
        ))
    }

    /// Always returns `false` on this platform.
    #[must_use]
    pub fn registered_buffers_active(&self) -> bool {
        false
    }

    /// Returns the configured slot size constant.
    #[must_use]
    pub fn slot_bytes(&self) -> usize {
        SEND_ZC_DISPATCH_MIN_BYTES
    }

    /// Returns the wrapped raw file descriptor.
    #[must_use]
    pub fn raw_fd(&self) -> RawFd {
        self._fd
    }
}
