//! Stub `cancel` module mirroring [`crate::io_uring::cancel`] on non-Linux
//! platforms or when the `io_uring` cargo feature is disabled.
//!
//! All entry points report `io::ErrorKind::Unsupported`; the
//! [`CancelOutcome`] enum is provided so cross-platform call sites can
//! match on it without `cfg`-gating.

pub use crate::io_uring_common::{
    ASYNC_CANCEL_FD_MIN_KERNEL, ASYNC_CANCEL_MIN_KERNEL, IORING_OP_ASYNC_CANCEL,
};
use std::io;

/// Stub mirror of [`crate::io_uring::cancel::CancelOutcome`]. Provided
/// so cross-platform callers can pattern-match without conditional
/// compilation; no stub entry point ever returns a value of this type
/// because all entry points fail with `Unsupported`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelOutcome {
    /// Mirror of the Linux variant.
    Cancelled,
    /// Mirror of the Linux variant.
    NotFound,
    /// Mirror of the Linux variant.
    AlreadyComplete,
}

/// Opaque ring placeholder on non-Linux platforms. The Linux entry
/// points take `&mut io_uring::IoUring`; the stub uses this unit
/// struct so the signatures stay close without exposing the
/// `io_uring` crate on platforms that do not link it.
#[derive(Debug, Default)]
pub struct StubIoUring {
    _private: (),
}

/// Always returns `Unsupported` on this platform.
pub fn cancel_by_user_data(_ring: &mut StubIoUring, _user_data: u64) -> io::Result<CancelOutcome> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "IORING_OP_ASYNC_CANCEL is not available on this platform",
    ))
}

/// Always returns `Unsupported` on this platform.
pub fn cancel_all_by_fd(_ring: &mut StubIoUring, _fd: std::os::raw::c_int) -> io::Result<usize> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "IORING_OP_ASYNC_CANCEL is not available on this platform",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stub_constants_match_linux_uapi() {
        assert_eq!(IORING_OP_ASYNC_CANCEL, 14);
        assert_eq!(ASYNC_CANCEL_MIN_KERNEL, (5, 5));
        assert_eq!(ASYNC_CANCEL_FD_MIN_KERNEL, (5, 19));
    }

    #[test]
    fn stub_cancel_by_user_data_returns_unsupported() {
        let mut ring = StubIoUring::default();
        let err = cancel_by_user_data(&mut ring, 0xDEAD).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::Unsupported);
    }

    #[test]
    fn stub_cancel_all_by_fd_returns_unsupported() {
        let mut ring = StubIoUring::default();
        let err = cancel_all_by_fd(&mut ring, 3).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::Unsupported);
    }
}
