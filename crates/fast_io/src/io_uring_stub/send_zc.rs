//! Stub `IORING_OP_SEND_ZC` module mirroring [`crate::io_uring::send_zc`] on
//! non-Linux platforms or when the `io_uring` cargo feature is disabled.
//!
//! [`is_supported`] always returns `false` and [`try_send_zc`] always returns
//! `Unsupported`. This keeps cross-platform call sites (and the socket
//! writer's fallback path) compiling without `cfg`-gating each reference.

use std::io;
use std::os::unix::io::RawFd;

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
