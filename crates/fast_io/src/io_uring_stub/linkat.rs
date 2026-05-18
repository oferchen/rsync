//! Stub `linkat` module mirroring [`crate::io_uring::linkat`] on non-Linux
//! platforms or when the `io_uring` cargo feature is disabled.

pub use crate::io_uring_common::{IORING_OP_LINKAT, LINKAT_MIN_KERNEL};
use std::ffi::CStr;
use std::io;

/// Borrowed arguments for an `IORING_OP_LINKAT` submission. On the stub
/// the struct exists only so cross-platform call sites compile; no SQE
/// is ever built.
#[derive(Debug)]
pub struct LinkAtArgs<'a> {
    /// Directory file descriptor that resolves `old_path`.
    pub old_dirfd: i32,
    /// Source path of the existing inode being hardlinked.
    pub old_path: &'a CStr,
    /// Directory file descriptor that resolves `new_path`.
    pub new_dirfd: i32,
    /// Destination path of the new hardlink.
    pub new_path: &'a CStr,
    /// Flags passed to the kernel.
    pub flags: i32,
}

/// Always returns `false` on this platform.
#[must_use]
pub fn linkat_supported() -> bool {
    false
}

/// Always returns `Unsupported` on this platform.
pub fn build_linkat_sqe(_args: LinkAtArgs<'_>) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "IORING_OP_LINKAT is not available on this platform",
    ))
}

/// Stub mirror of the Linux `build_linkat_sqe_unchecked`. No-op.
pub fn build_linkat_sqe_unchecked(_args: LinkAtArgs<'_>) {}

/// Always returns `Unsupported` on this platform.
pub fn submit_linkat_blocking(_args: LinkAtArgs<'_>) -> io::Result<i32> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "IORING_OP_LINKAT is not available on this platform",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stub_reports_unsupported() {
        assert!(!linkat_supported());
    }

    #[test]
    fn stub_constants_match_linux_uapi() {
        assert_eq!(IORING_OP_LINKAT, 39);
        assert_eq!(LINKAT_MIN_KERNEL, (5, 15));
    }

    #[test]
    fn stub_build_linkat_sqe_returns_unsupported() {
        let old = c"/tmp/old";
        let new = c"/tmp/new";
        let err = build_linkat_sqe(LinkAtArgs {
            old_dirfd: 0,
            old_path: old,
            new_dirfd: 0,
            new_path: new,
            flags: 0,
        })
        .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::Unsupported);
    }
}
