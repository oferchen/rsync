//! `statx` syscall support detection and wrappers for Linux 4.11+.

#[cfg(all(target_os = "linux", not(target_env = "musl")))]
use std::io;
#[cfg(all(target_os = "linux", not(target_env = "musl")))]
use std::path::Path;

#[cfg(all(target_os = "linux", not(target_env = "musl")))]
use super::types::StatxResult;

/// Checks if statx syscall is available.
///
/// Returns true on Linux 4.11+ where statx is supported.
/// The result is cached after the first call using a probe syscall.
#[cfg(all(target_os = "linux", not(target_env = "musl")))]
#[must_use]
pub fn has_statx_support() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};

    // 0 = unknown, 1 = supported, 2 = not supported
    static CACHED: AtomicU8 = AtomicU8::new(0);

    match CACHED.load(Ordering::Relaxed) {
        1 => return true,
        2 => return false,
        _ => {}
    }

    use std::ffi::CString;

    let path = CString::new(".").unwrap();
    let mut statx_buf: libc::statx = unsafe { std::mem::zeroed() };

    let ret = unsafe {
        libc::syscall(
            libc::SYS_statx,
            libc::AT_FDCWD,
            path.as_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
            libc::STATX_BASIC_STATS,
            &mut statx_buf,
        )
    };

    let supported = ret == 0;
    CACHED.store(if supported { 1 } else { 2 }, Ordering::Relaxed);
    supported
}

/// Returns whether the platform supports the `statx` syscall (always `false` on non-Linux).
#[cfg(any(not(target_os = "linux"), target_env = "musl"))]
#[must_use]
pub fn has_statx_support() -> bool {
    false
}

/// Fetches metadata using statx (Linux 4.11+) and returns a lightweight
/// `StatxResult` instead of a full `fs::Metadata`.
///
/// This avoids the overhead of Rust's standard library metadata construction
/// and lets the kernel skip computing unrequested fields via the mask parameter.
///
/// # Errors
///
/// Returns an error if the statx syscall fails (e.g., ENOENT, ENOSYS).
#[cfg(all(target_os = "linux", not(target_env = "musl")))]
pub fn statx<P: AsRef<Path>>(path: P, follow_symlinks: bool) -> io::Result<StatxResult> {
    statx_with_mask(
        libc::AT_FDCWD,
        path.as_ref(),
        follow_symlinks,
        libc::STATX_BASIC_STATS,
    )
}

/// Fetches only the modification time using statx.
///
/// Requests only `STATX_MTIME` from the kernel, which is the minimum needed
/// for rsync change detection. This reduces kernel overhead compared to
/// fetching all metadata fields.
///
/// # Errors
///
/// Returns an error if the statx syscall fails or is not supported.
#[cfg(all(target_os = "linux", not(target_env = "musl")))]
pub fn statx_mtime<P: AsRef<Path>>(path: P, follow_symlinks: bool) -> io::Result<(i64, u32)> {
    let result = statx_with_mask(
        libc::AT_FDCWD,
        path.as_ref(),
        follow_symlinks,
        libc::STATX_MTIME,
    )?;
    Ok((result.mtime_sec, result.mtime_nsec))
}

/// Fetches only size and mtime using statx (common for rsync change detection).
///
/// # Errors
///
/// Returns an error if the statx syscall fails or is not supported.
#[cfg(all(target_os = "linux", not(target_env = "musl")))]
pub fn statx_size_and_mtime<P: AsRef<Path>>(
    path: P,
    follow_symlinks: bool,
) -> io::Result<(u64, i64, u32)> {
    let result = statx_with_mask(
        libc::AT_FDCWD,
        path.as_ref(),
        follow_symlinks,
        libc::STATX_SIZE | libc::STATX_MTIME,
    )?;
    Ok((result.size, result.mtime_sec, result.mtime_nsec))
}

/// Core statx wrapper that accepts a directory fd and field mask.
///
/// This is the low-level building block used by all other statx functions.
/// The `dir_fd` parameter enables directory-relative lookups (AT_FDCWD for
/// absolute paths, or an open directory fd for relative paths).
#[cfg(all(target_os = "linux", not(target_env = "musl")))]
fn statx_with_mask(
    dir_fd: i32,
    path: &Path,
    follow_symlinks: bool,
    mask: u32,
) -> io::Result<StatxResult> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let path_bytes = path.as_os_str().as_bytes();
    let c_path = CString::new(path_bytes)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, format!("invalid path: {e}")))?;

    let flags = if follow_symlinks {
        0i32
    } else {
        libc::AT_SYMLINK_NOFOLLOW
    };

    let mut statx_buf: libc::statx = unsafe { std::mem::zeroed() };

    let ret = unsafe {
        libc::syscall(
            libc::SYS_statx,
            dir_fd,
            c_path.as_ptr(),
            flags,
            mask,
            &mut statx_buf,
        )
    };

    if ret != 0 {
        return Err(io::Error::last_os_error());
    }

    Ok(StatxResult {
        mode: statx_buf.stx_mode as u32,
        size: statx_buf.stx_size,
        mtime_sec: statx_buf.stx_mtime.tv_sec,
        mtime_nsec: statx_buf.stx_mtime.tv_nsec,
        uid: statx_buf.stx_uid,
        gid: statx_buf.stx_gid,
        ino: statx_buf.stx_ino,
        nlink: statx_buf.stx_nlink,
        rdev_major: statx_buf.stx_rdev_major,
        rdev_minor: statx_buf.stx_rdev_minor,
    })
}
