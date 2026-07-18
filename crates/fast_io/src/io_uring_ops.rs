//! Convenience wrappers that try io_uring fast paths and fall back to
//! portable `std::fs` operations on unsupported platforms or kernels.
//!
//! Each `try_*` function returns `Option<io::Result<_>>` so callers can
//! distinguish "io_uring not available" (`None`) from "io_uring tried and
//! returned an error" (`Some(Err(_))`). The combined [`hard_link`] helper
//! folds the try-then-fallback pattern into a single call for the common
//! case where the caller does not care which mechanism was used.

use crate::io_uring::StatxResult;
#[cfg(all(target_os = "linux", feature = "io_uring"))]
use crate::io_uring::{
    LinkAtArgs, RenameAt2Args, linkat_supported, renameat2_blocking, renameat2_supported,
    statx_supported, submit_linkat_blocking, submit_statx_batch,
};

/// Attempts to rename a file via io_uring `IORING_OP_RENAMEAT`.
///
/// On Linux with kernel 5.11+ and io_uring available, submits a blocking
/// RENAMEAT2 SQE on a transient ring and returns the result. On all other
/// platforms, or when the kernel lacks the opcode, returns `None` so the
/// caller can fall back to `std::fs::rename`.
///
/// This follows the same try-or-fallback pattern used by the splice and
/// copy-file-range paths: the caller checks the `Option` and falls through
/// to the portable implementation when `None` is returned.
///
/// # Errors
///
/// Returns `Some(Err(...))` when io_uring is available and the rename was
/// submitted but the kernel returned an error (e.g., `ENOENT`, `EACCES`).
pub fn try_rename_via_io_uring(
    old_path: &std::path::Path,
    new_path: &std::path::Path,
) -> Option<std::io::Result<()>> {
    try_rename_via_io_uring_impl(old_path, new_path)
}

#[cfg(all(target_os = "linux", feature = "io_uring"))]
fn try_rename_via_io_uring_impl(
    old_path: &std::path::Path,
    new_path: &std::path::Path,
) -> Option<std::io::Result<()>> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    if !renameat2_supported() {
        return None;
    }
    let old_c = match CString::new(old_path.as_os_str().as_bytes()) {
        Ok(c) => c,
        Err(_) => return Some(Err(std::io::Error::other("path contains interior NUL"))),
    };
    let new_c = match CString::new(new_path.as_os_str().as_bytes()) {
        Ok(c) => c,
        Err(_) => return Some(Err(std::io::Error::other("path contains interior NUL"))),
    };
    let args = RenameAt2Args {
        old_dir_fd: libc::AT_FDCWD,
        old_path: &old_c,
        new_dir_fd: libc::AT_FDCWD,
        new_path: &new_c,
        flags: 0,
    };
    match renameat2_blocking(args) {
        Ok(result) if result < 0 => Some(Err(std::io::Error::from_raw_os_error(-result))),
        Ok(_) => Some(Ok(())),
        Err(e) if e.kind() == std::io::ErrorKind::Unsupported => None,
        Err(e) => Some(Err(e)),
    }
}

#[cfg(not(all(target_os = "linux", feature = "io_uring")))]
fn try_rename_via_io_uring_impl(
    _old_path: &std::path::Path,
    _new_path: &std::path::Path,
) -> Option<std::io::Result<()>> {
    None
}

/// Attempts to create a hard link via io_uring `IORING_OP_LINKAT`.
///
/// On Linux with kernel 5.15+ and io_uring available, submits a blocking
/// LINKAT SQE on a transient ring and returns the result. On all other
/// platforms, or when the kernel lacks the opcode, returns `None` so the
/// caller can fall back to `std::fs::hard_link`.
///
/// # Errors
///
/// Returns `Some(Err(...))` when io_uring is available and the link was
/// submitted but the kernel returned an error (e.g., `EEXIST`, `EACCES`).
pub fn try_hard_link_via_io_uring(
    src_path: &std::path::Path,
    dst_path: &std::path::Path,
) -> Option<std::io::Result<()>> {
    try_hard_link_via_io_uring_impl(src_path, dst_path)
}

#[cfg(all(target_os = "linux", feature = "io_uring"))]
fn try_hard_link_via_io_uring_impl(
    src_path: &std::path::Path,
    dst_path: &std::path::Path,
) -> Option<std::io::Result<()>> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    if !linkat_supported() {
        return None;
    }
    let old_c = match CString::new(src_path.as_os_str().as_bytes()) {
        Ok(c) => c,
        Err(_) => return Some(Err(std::io::Error::other("path contains interior NUL"))),
    };
    let new_c = match CString::new(dst_path.as_os_str().as_bytes()) {
        Ok(c) => c,
        Err(_) => return Some(Err(std::io::Error::other("path contains interior NUL"))),
    };
    let args = LinkAtArgs {
        old_dirfd: libc::AT_FDCWD,
        old_path: &old_c,
        new_dirfd: libc::AT_FDCWD,
        new_path: &new_c,
        flags: 0,
    };
    match submit_linkat_blocking(args) {
        Ok(_) => Some(Ok(())),
        Err(e) if e.kind() == std::io::ErrorKind::Unsupported => None,
        Err(e) => Some(Err(e)),
    }
}

#[cfg(not(all(target_os = "linux", feature = "io_uring")))]
fn try_hard_link_via_io_uring_impl(
    _src_path: &std::path::Path,
    _dst_path: &std::path::Path,
) -> Option<std::io::Result<()>> {
    None
}

/// Attempts to stat files via io_uring `IORING_OP_STATX` batch submission.
///
/// On Linux with kernel 5.11+ and io_uring available, submits all paths
/// as independent STATX SQEs on a single ring and returns the results.
/// On all other platforms, or when the kernel lacks the opcode, returns
/// `None` so the caller can fall back to synchronous stat calls.
///
/// # Arguments
///
/// * `paths` - Slice of paths to stat.
/// * `follow_symlinks` - If `true`, follows symlinks (like `stat`);
///   if `false`, does not follow (like `lstat`).
///
/// # Returns
///
/// - `Some(Ok(results))` when io_uring statx is available and all
///   submissions succeeded (individual paths may still have errors).
/// - `None` when io_uring statx is not available on this platform/kernel.
/// - `Some(Err(...))` for ring-level failures.
#[must_use]
pub fn try_statx_batch_via_io_uring(
    paths: &[&std::path::Path],
    follow_symlinks: bool,
) -> Option<std::io::Result<Vec<StatxResult>>> {
    try_statx_batch_via_io_uring_impl(paths, follow_symlinks)
}

#[cfg(all(target_os = "linux", feature = "io_uring"))]
fn try_statx_batch_via_io_uring_impl(
    paths: &[&std::path::Path],
    follow_symlinks: bool,
) -> Option<std::io::Result<Vec<StatxResult>>> {
    if !statx_supported() {
        return None;
    }
    Some(submit_statx_batch(paths, follow_symlinks))
}

#[cfg(not(all(target_os = "linux", feature = "io_uring")))]
fn try_statx_batch_via_io_uring_impl(
    _paths: &[&std::path::Path],
    _follow_symlinks: bool,
) -> Option<std::io::Result<Vec<StatxResult>>> {
    None
}

/// Creates a hard link from `src` to `dst`, trying io_uring first.
///
/// On Linux 5.15+ with io_uring `IORING_OP_LINKAT` support, the link is
/// submitted as an asynchronous SQE on a transient ring, avoiding a
/// synchronous `linkat(2)` syscall. On all other platforms, older kernels,
/// or when the `io_uring` feature is disabled, falls back to
/// [`std::fs::hard_link`].
///
/// This is the recommended single entry point for hard-link creation across
/// the codebase. It consolidates the try-io_uring-then-fallback pattern so
/// callers do not need to handle the `Option` from
/// [`try_hard_link_via_io_uring`] themselves.
///
/// # Errors
///
/// Returns an error when both the io_uring path and the `std::fs::hard_link`
/// fallback fail (e.g., `EEXIST`, `EACCES`, `EXDEV`).
///
/// # Upstream reference
///
/// Upstream rsync uses synchronous `link(2)` / `linkat(2)` for hardlink
/// creation (`hlink.c`). The io_uring fast path is a latency optimisation.
pub fn hard_link(src: &std::path::Path, dst: &std::path::Path) -> std::io::Result<()> {
    if let Some(result) = try_hard_link_via_io_uring(src, dst) {
        return result;
    }
    logging::debug_log!(
        Io,
        2,
        "io_uring LINKAT unavailable, falling back to std::fs::hard_link for {}",
        dst.display()
    );
    std::fs::hard_link(src, dst)
}

#[cfg(test)]
mod rename_dispatch_tests {
    use super::*;
    use std::fs;

    #[test]
    fn try_rename_via_io_uring_renames_or_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("rename_src.txt");
        let dst = dir.path().join("rename_dst.txt");
        fs::write(&src, b"rename payload").unwrap();

        match try_rename_via_io_uring(&src, &dst) {
            Some(Ok(())) => {
                // io_uring path succeeded - verify file moved.
                assert!(!src.exists());
                assert_eq!(fs::read(&dst).unwrap(), b"rename payload");
            }
            Some(Err(e)) => {
                panic!("io_uring rename returned error: {e}");
            }
            None => {
                // Not available on this platform/kernel - file untouched.
                assert!(src.exists());
                assert!(!dst.exists());
            }
        }
    }

    #[test]
    fn try_rename_via_io_uring_returns_none_consistently() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("consistency_src.txt");
        let dst = dir.path().join("consistency_dst.txt");
        fs::write(&src, b"data").unwrap();

        let first = try_rename_via_io_uring(&src, &dst).is_some();
        // If first call consumed the file, recreate for second probe.
        if first {
            fs::write(&src, b"data").unwrap();
            let _ = fs::remove_file(&dst);
        }
        let second = try_rename_via_io_uring(&src, &dst).is_some();
        assert_eq!(
            first, second,
            "availability must be consistent across calls"
        );
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn try_rename_via_io_uring_returns_none_on_non_linux() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("non_linux_src.txt");
        let dst = dir.path().join("non_linux_dst.txt");
        fs::write(&src, b"data").unwrap();

        assert!(
            try_rename_via_io_uring(&src, &dst).is_none(),
            "must return None on non-Linux platforms"
        );
        assert!(src.exists(), "source must be untouched");
    }
}

#[cfg(test)]
mod hard_link_dispatch_tests {
    use super::*;
    use std::fs;

    #[test]
    fn try_hard_link_via_io_uring_links_or_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("link_src.txt");
        let dst = dir.path().join("link_dst.txt");
        fs::write(&src, b"link payload").unwrap();

        match try_hard_link_via_io_uring(&src, &dst) {
            Some(Ok(())) => {
                // io_uring path succeeded - verify hard link created.
                assert!(src.exists());
                assert!(dst.exists());
                assert_eq!(fs::read(&dst).unwrap(), b"link payload");
            }
            Some(Err(e)) => {
                panic!("io_uring hard_link returned error: {e}");
            }
            None => {
                // Not available on this platform/kernel.
                assert!(src.exists());
                assert!(!dst.exists());
            }
        }
    }

    #[test]
    fn try_hard_link_via_io_uring_returns_none_consistently() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("link_consistency_src.txt");
        let dst1 = dir.path().join("link_consistency_dst1.txt");
        let dst2 = dir.path().join("link_consistency_dst2.txt");
        fs::write(&src, b"data").unwrap();

        let first = try_hard_link_via_io_uring(&src, &dst1).is_some();
        let second = try_hard_link_via_io_uring(&src, &dst2).is_some();
        assert_eq!(
            first, second,
            "availability must be consistent across calls"
        );
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn try_hard_link_via_io_uring_returns_none_on_non_linux() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("non_linux_link_src.txt");
        let dst = dir.path().join("non_linux_link_dst.txt");
        fs::write(&src, b"data").unwrap();

        assert!(
            try_hard_link_via_io_uring(&src, &dst).is_none(),
            "must return None on non-Linux platforms"
        );
        assert!(!dst.exists(), "destination must not exist");
    }
}

#[cfg(test)]
mod statx_dispatch_tests {
    use super::*;
    use std::fs;

    #[test]
    fn try_statx_batch_via_io_uring_stats_or_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("statx_dispatch.txt");
        fs::write(&file, b"dispatch payload").unwrap();

        let paths: Vec<&std::path::Path> = vec![file.as_path()];
        match try_statx_batch_via_io_uring(&paths, true) {
            Some(Ok(results)) => {
                assert_eq!(results.len(), 1);
                assert!(results[0].is_ok(), "existing file should succeed");
            }
            Some(Err(e)) => {
                panic!("io_uring statx batch returned ring error: {e}");
            }
            None => {
                // Not available on this platform/kernel.
            }
        }
    }

    #[test]
    fn try_statx_batch_via_io_uring_returns_none_consistently() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("statx_consistency.txt");
        fs::write(&file, b"data").unwrap();

        let paths: Vec<&std::path::Path> = vec![file.as_path()];
        let first = try_statx_batch_via_io_uring(&paths, true).is_some();
        let second = try_statx_batch_via_io_uring(&paths, true).is_some();
        assert_eq!(
            first, second,
            "availability must be consistent across calls"
        );
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn try_statx_batch_via_io_uring_returns_none_on_non_linux() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("non_linux_statx.txt");
        fs::write(&file, b"data").unwrap();

        let paths: Vec<&std::path::Path> = vec![file.as_path()];
        assert!(
            try_statx_batch_via_io_uring(&paths, true).is_none(),
            "must return None on non-Linux platforms"
        );
    }

    #[test]
    fn try_statx_batch_via_io_uring_empty_input() {
        let paths: Vec<&std::path::Path> = vec![];
        match try_statx_batch_via_io_uring(&paths, true) {
            Some(Ok(results)) => {
                assert!(results.is_empty());
            }
            None => {
                // Not available on this platform.
            }
            Some(Err(e)) => {
                panic!("unexpected error on empty input: {e}");
            }
        }
    }
}

#[cfg(test)]
mod hard_link_convenience_tests {
    use super::*;
    use std::fs;

    /// Verifies `hard_link` creates a valid hard link on any platform,
    /// using io_uring when available and falling back to `std::fs::hard_link`.
    #[test]
    fn hard_link_creates_link() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("hl_src.txt");
        let dst = dir.path().join("hl_dst.txt");
        fs::write(&src, b"hard link payload").unwrap();

        hard_link(&src, &dst).unwrap();

        assert!(src.exists(), "source must still exist after hard link");
        assert!(dst.exists(), "destination must exist after hard link");
        assert_eq!(fs::read(&dst).unwrap(), b"hard link payload");
    }

    /// Verifies that source and destination share the same inode on Unix,
    /// confirming a true hard link rather than a copy.
    #[cfg(unix)]
    #[test]
    fn hard_link_shares_inode() {
        use std::os::unix::fs::MetadataExt;

        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("hl_inode_src.txt");
        let dst = dir.path().join("hl_inode_dst.txt");
        fs::write(&src, b"inode check").unwrap();

        hard_link(&src, &dst).unwrap();

        let src_ino = fs::metadata(&src).unwrap().ino();
        let dst_ino = fs::metadata(&dst).unwrap().ino();
        assert_eq!(src_ino, dst_ino, "hard link must share same inode");
    }

    /// Verifies `hard_link` returns an error when the destination already
    /// exists (EEXIST).
    #[test]
    fn hard_link_fails_when_dst_exists() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("hl_exists_src.txt");
        let dst = dir.path().join("hl_exists_dst.txt");
        fs::write(&src, b"source").unwrap();
        fs::write(&dst, b"existing").unwrap();

        let result = hard_link(&src, &dst);
        assert!(result.is_err(), "must fail when destination exists");
    }

    /// Verifies `hard_link` returns an error when the source does not exist.
    #[test]
    fn hard_link_fails_for_missing_source() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("hl_missing.txt");
        let dst = dir.path().join("hl_missing_dst.txt");

        let result = hard_link(&src, &dst);
        assert!(result.is_err(), "must fail when source does not exist");
    }

    /// Verifies that writing to the source after hard-linking is visible
    /// through the destination path, confirming shared data blocks.
    #[test]
    fn hard_link_shares_data() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("hl_shared_src.txt");
        let dst = dir.path().join("hl_shared_dst.txt");
        fs::write(&src, b"original").unwrap();

        hard_link(&src, &dst).unwrap();

        fs::write(&src, b"modified").unwrap();

        // Read through destination - should see the modification.
        assert_eq!(fs::read(&dst).unwrap(), b"modified");
    }
}
