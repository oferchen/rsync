//! Stub `IORING_OP_STATX` module mirroring [`crate::io_uring::statx`].

pub use crate::io_uring_common::{IORING_OP_STATX, STATX_MIN_KERNEL};
use std::ffi::CStr;
use std::io;
use std::path::Path;

/// Borrowed arguments for an `IORING_OP_STATX` submission. Stub
/// definition mirrors the Linux module's struct shape so cross-platform
/// call sites compile without `cfg`-gating; the stub never submits an
/// SQE.
#[derive(Debug)]
pub struct StatxArgs<'a> {
    /// Directory file descriptor that resolves `pathname`.
    pub dirfd: i32,
    /// Path to stat.
    pub pathname: &'a CStr,
    /// Flags passed to the kernel.
    pub flags: i32,
    /// Mask of fields to request.
    pub mask: u32,
    /// Output buffer (unused on this platform).
    pub statx_buf: &'a mut [u8; 256],
}

/// Result of a single statx operation within a batch.
pub type StatxResult = io::Result<()>;

/// Always returns `false` on this platform.
#[must_use]
pub fn statx_supported() -> bool {
    false
}

/// Always returns `Unsupported` on this platform.
pub fn build_statx_sqe(_args: &mut StatxArgs<'_>) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "IORING_OP_STATX is not available on this platform",
    ))
}

/// Stub mirror of the Linux `build_statx_sqe_unchecked`.
pub fn build_statx_sqe_unchecked(_args: &mut StatxArgs<'_>) {}

/// Always returns `Unsupported` on this platform.
pub fn submit_statx_blocking(
    _dirfd: i32,
    _pathname: &CStr,
    _flags: i32,
    _mask: u32,
) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "IORING_OP_STATX is not available on this platform",
    ))
}

/// Always returns `Unsupported` for each path on this platform.
pub fn submit_statx_batch(paths: &[&Path], _follow_symlinks: bool) -> io::Result<Vec<StatxResult>> {
    Ok(paths
        .iter()
        .map(|_| {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "statx is not available on this platform",
            ))
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stub_reports_unsupported() {
        assert!(!statx_supported());
    }

    #[test]
    fn stub_constants_match_linux_uapi() {
        assert_eq!(IORING_OP_STATX, 21);
        assert_eq!(STATX_MIN_KERNEL, (5, 11));
    }

    #[test]
    fn stub_submit_statx_batch_returns_unsupported_for_each_path() {
        let dir = tempfile::tempdir().unwrap();
        let p1 = dir.path().join("a.txt");
        let p2 = dir.path().join("b.txt");
        std::fs::write(&p1, b"a").unwrap();
        std::fs::write(&p2, b"b").unwrap();

        let paths: Vec<&Path> = vec![p1.as_path(), p2.as_path()];
        let results = submit_statx_batch(&paths, true).unwrap();
        assert_eq!(results.len(), 2);
        for result in results {
            assert_eq!(result.unwrap_err().kind(), io::ErrorKind::Unsupported);
        }
    }

    #[test]
    fn stub_submit_statx_batch_empty() {
        let paths: &[&Path] = &[];
        let results = submit_statx_batch(paths, true).unwrap();
        assert!(results.is_empty());
    }
}
