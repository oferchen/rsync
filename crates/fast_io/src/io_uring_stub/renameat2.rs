//! Stub `IORING_OP_RENAMEAT` module mirroring [`crate::io_uring::renameat2`].

pub use crate::io_uring_common::{
    IORING_OP_RENAMEAT, RENAME_EXCHANGE, RENAME_NOREPLACE, RENAME_WHITEOUT,
};
use std::ffi::CStr;
use std::io;
use std::os::raw::c_int;

/// Stub argument struct mirroring the Linux `RenameAt2Args`.
#[derive(Debug, Clone, Copy)]
pub struct RenameAt2Args<'a> {
    /// Directory fd that `old_path` is resolved against.
    pub old_dir_fd: c_int,
    /// Old path (CStr borrow).
    pub old_path: &'a CStr,
    /// Directory fd that `new_path` is resolved against.
    pub new_dir_fd: c_int,
    /// New path (CStr borrow).
    pub new_path: &'a CStr,
    /// Bitwise OR of `RENAME_NOREPLACE`, `RENAME_EXCHANGE`,
    /// `RENAME_WHITEOUT`.
    pub flags: u32,
}

/// Stub opaque SQE returned by [`build_renameat2_sqe_unchecked`] on
/// platforms that lack io_uring. Carries no kernel state; exists only
/// so cross-platform code compiles.
#[derive(Debug, Clone, Copy)]
pub struct StubSqe {
    _private: (),
}

/// Always returns `false` on this platform.
#[must_use]
pub fn renameat2_supported() -> bool {
    false
}

/// Always returns `Unsupported` on this platform.
pub fn build_renameat2_sqe(_args: RenameAt2Args<'_>) -> io::Result<StubSqe> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "IORING_OP_RENAMEAT is not available on this platform",
    ))
}

/// Returns a stub SQE; only useful as a constructor smoke test on
/// non-Linux platforms.
#[must_use]
pub fn build_renameat2_sqe_unchecked(_args: RenameAt2Args<'_>) -> StubSqe {
    StubSqe { _private: () }
}

/// Always returns `Unsupported` on this platform.
pub fn renameat2_blocking(_args: RenameAt2Args<'_>) -> io::Result<i32> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "IORING_OP_RENAMEAT is not available on this platform",
    ))
}
