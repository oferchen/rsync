//! Lightweight `statx(2)` wrappers for file-list metadata fetching.
//!
//! Owns the statx syscall surface that `flist::batched_stat` consumes, per the
//! two-owner unsafe policy: I/O syscalls live in `fast_io`. The wrappers are
//! built entirely on `rustix::fs::statx`, so this module contains no
//! `unsafe`; `flist` re-exports `StatxResult` and the free functions under
//! their original paths.
//!
//! Everything except `has_statx_support` is gated to non-musl Linux,
//! mirroring the gating this code carried in `flist` so per-target behaviour
//! is unchanged. `has_statx_support` compiles everywhere and reports `false`
//! where the syscall is not wrapped.

#[cfg(all(target_os = "linux", not(target_env = "musl")))]
use std::ffi::OsStr;
#[cfg(all(target_os = "linux", not(target_env = "musl")))]
use std::io;
#[cfg(all(target_os = "linux", not(target_env = "musl")))]
use std::os::fd::{AsFd, BorrowedFd};
#[cfg(all(target_os = "linux", not(target_env = "musl")))]
use std::path::Path;

/// Lightweight metadata result from statx(2).
///
/// Contains only the fields rsync needs during file list generation,
/// avoiding the overhead of constructing a full `fs::Metadata`. On Linux 4.11+
/// the kernel can skip computing unwanted fields when the request mask
/// excludes them.
#[cfg(all(target_os = "linux", not(target_env = "musl")))]
#[derive(Debug, Clone)]
pub struct StatxResult {
    /// File type and permission bits (stx_mode).
    pub mode: u32,
    /// File size in bytes.
    pub size: u64,
    /// Last modification time (seconds since epoch).
    pub mtime_sec: i64,
    /// Last modification time (nanoseconds component).
    pub mtime_nsec: u32,
    /// User ID of the owner.
    pub uid: u32,
    /// Group ID of the owner.
    pub gid: u32,
    /// Inode number.
    pub ino: u64,
    /// Number of hard links.
    pub nlink: u32,
    /// Device ID major.
    pub rdev_major: u32,
    /// Device ID minor.
    pub rdev_minor: u32,
}

#[cfg(all(target_os = "linux", not(target_env = "musl")))]
impl StatxResult {
    /// Returns true if this entry is a regular file.
    #[must_use]
    pub fn is_file(&self) -> bool {
        (self.mode & libc::S_IFMT) == libc::S_IFREG
    }

    /// Returns true if this entry is a directory.
    #[must_use]
    pub fn is_dir(&self) -> bool {
        (self.mode & libc::S_IFMT) == libc::S_IFDIR
    }

    /// Returns true if this entry is a symbolic link.
    #[must_use]
    pub fn is_symlink(&self) -> bool {
        (self.mode & libc::S_IFMT) == libc::S_IFLNK
    }

    /// Returns the permission bits (lower 12 bits of mode).
    #[must_use]
    pub fn permissions(&self) -> u32 {
        self.mode & 0o7777
    }
}

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

    let supported = statx_with_mask(
        rustix::fs::CWD,
        ".",
        false,
        rustix::fs::StatxFlags::BASIC_STATS,
    )
    .is_ok();
    CACHED.store(if supported { 1 } else { 2 }, Ordering::Relaxed);
    supported
}

/// Returns whether the platform supports the `statx` syscall (always `false`
/// on non-Linux and musl targets, where the wrapper is not compiled).
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
        rustix::fs::CWD,
        path.as_ref(),
        follow_symlinks,
        rustix::fs::StatxFlags::BASIC_STATS,
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
        rustix::fs::CWD,
        path.as_ref(),
        follow_symlinks,
        rustix::fs::StatxFlags::MTIME,
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
        rustix::fs::CWD,
        path.as_ref(),
        follow_symlinks,
        rustix::fs::StatxFlags::SIZE | rustix::fs::StatxFlags::MTIME,
    )?;
    Ok((result.size, result.mtime_sec, result.mtime_nsec))
}

/// Stats `name` relative to an open directory using statx (Linux 4.11+).
///
/// The directory fd pins the parent, so the lookup resolves a single
/// component under it rather than re-walking the full path. Consumed by
/// `flist::batched_stat::DirectoryStatBatch::statx_relative`.
///
/// # Errors
///
/// Returns an error if the statx syscall fails (e.g., ENOENT, ENOSYS) or
/// `name` contains an interior NUL byte (EINVAL).
#[cfg(all(target_os = "linux", not(target_env = "musl")))]
pub fn statx_at(
    dir_fd: BorrowedFd<'_>,
    name: &OsStr,
    follow_symlinks: bool,
) -> io::Result<StatxResult> {
    statx_with_mask(
        dir_fd,
        name,
        follow_symlinks,
        rustix::fs::StatxFlags::BASIC_STATS,
    )
}

/// Core statx wrapper that accepts a directory fd and field mask.
///
/// This is the low-level building block used by all other statx functions.
/// The `dirfd` parameter enables directory-relative lookups (`rustix::fs::CWD`
/// for absolute/cwd-relative paths, or an open directory fd for relative
/// names).
#[cfg(all(target_os = "linux", not(target_env = "musl")))]
fn statx_with_mask<P: rustix::path::Arg, Fd: AsFd>(
    dirfd: Fd,
    path: P,
    follow_symlinks: bool,
    mask: rustix::fs::StatxFlags,
) -> io::Result<StatxResult> {
    use rustix::fs::AtFlags;

    let flags = if follow_symlinks {
        AtFlags::empty()
    } else {
        AtFlags::SYMLINK_NOFOLLOW
    };

    let buf = rustix::fs::statx(dirfd, path, flags, mask).map_err(io::Error::from)?;

    Ok(StatxResult {
        mode: u32::from(buf.stx_mode),
        size: buf.stx_size,
        mtime_sec: buf.stx_mtime.tv_sec,
        mtime_nsec: buf.stx_mtime.tv_nsec,
        uid: buf.stx_uid,
        gid: buf.stx_gid,
        ino: buf.stx_ino,
        nlink: buf.stx_nlink,
        rdev_major: buf.stx_rdev_major,
        rdev_minor: buf.stx_rdev_minor,
    })
}

#[cfg(test)]
mod tests {
    use super::has_statx_support;

    /// The cached probe must answer consistently across calls on every target.
    #[test]
    fn has_statx_support_is_consistent() {
        let first = has_statx_support();
        assert_eq!(first, has_statx_support());
        assert_eq!(first, has_statx_support());
    }

    #[cfg(all(target_os = "linux", not(target_env = "musl")))]
    mod linux {
        use super::super::{statx, statx_at, statx_size_and_mtime};
        use crate::statx::has_statx_support;
        use std::os::fd::AsFd;
        use std::os::unix::fs::MetadataExt;

        /// The wrapper must agree with `std::fs` on the identity fields for
        /// the same file, proving the rustix routing fills the struct from the
        /// same inode the standard library reports.
        #[test]
        fn statx_agrees_with_std_metadata() {
            if !has_statx_support() {
                return;
            }
            let temp = tempfile::tempdir().expect("tempdir");
            let path = temp.path().join("probe.bin");
            std::fs::write(&path, b"probe payload").expect("write probe file");

            let sr = statx(&path, false).expect("statx");
            let std_meta = std::fs::symlink_metadata(&path).expect("symlink_metadata");
            assert!(sr.is_file());
            assert_eq!(sr.size, std_meta.len());
            assert_eq!(sr.ino, std_meta.ino());
            assert_eq!(sr.uid, std_meta.uid());
            assert_eq!(sr.gid, std_meta.gid());

            let (size, mtime_sec, _nsec) = statx_size_and_mtime(&path, false).expect("size+mtime");
            assert_eq!(size, std_meta.len());
            assert_eq!(mtime_sec, std_meta.mtime());
        }

        /// The directory-relative form must resolve a bare name under the fd,
        /// and refuse a name with an interior NUL instead of panicking.
        #[test]
        fn statx_at_resolves_relative_names() {
            if !has_statx_support() {
                return;
            }
            let temp = tempfile::tempdir().expect("tempdir");
            std::fs::write(temp.path().join("rel.bin"), b"12345").expect("write");
            let dir = std::fs::File::open(temp.path()).expect("open dir");

            let sr =
                statx_at(dir.as_fd(), std::ffi::OsStr::new("rel.bin"), false).expect("statx_at");
            assert!(sr.is_file());
            assert_eq!(sr.size, 5);

            assert!(statx_at(dir.as_fd(), std::ffi::OsStr::new("no\0nul"), false).is_err());
            assert!(statx_at(dir.as_fd(), std::ffi::OsStr::new("absent"), false).is_err());
        }
    }
}
