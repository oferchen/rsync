//! Directory-relative stat operations using `openat`/`fstatat`.

use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::Path;

#[cfg(feature = "parallel")]
use rayon::prelude::*;

use super::types::FstatResult;
#[cfg(all(target_os = "linux", not(target_env = "musl")))]
use super::types::StatxResult;

/// Batch metadata fetcher for directory entries.
///
/// Uses `openat`/`fstatat` to reduce path resolution overhead when
/// fetching metadata for many files in the same directory.
pub struct DirectoryStatBatch {
    _dir_file: fs::File,
    dir_fd: std::os::unix::io::RawFd,
}

impl DirectoryStatBatch {
    /// Opens a directory for batched stat operations.
    ///
    /// # Errors
    ///
    /// Returns an error if the directory cannot be opened.
    pub fn open<P: AsRef<Path>>(dir_path: P) -> io::Result<Self> {
        use std::os::unix::io::AsRawFd;

        let dir = fs::File::open(dir_path.as_ref())?;
        let dir_fd = dir.as_raw_fd();

        Ok(Self {
            _dir_file: dir,
            dir_fd,
        })
    }

    /// Stats a file relative to the directory.
    ///
    /// Uses `fstatat` to avoid full path resolution, returning a lightweight
    /// `FstatResult` constructed directly from the syscall output (no second stat).
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be stat'd.
    pub fn stat_relative(&self, name: &OsString, follow_symlinks: bool) -> io::Result<FstatResult> {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;

        let name_bytes = name.as_bytes();
        let c_name = CString::new(name_bytes).map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("invalid filename: {e}"),
            )
        })?;

        let flags = if follow_symlinks {
            0
        } else {
            libc::AT_SYMLINK_NOFOLLOW
        };

        let mut stat_buf: libc::stat = unsafe { std::mem::zeroed() };

        let ret = unsafe { libc::fstatat(self.dir_fd, c_name.as_ptr(), &mut stat_buf, flags) };

        if ret != 0 {
            return Err(io::Error::last_os_error());
        }

        Ok(FstatResult::from_stat(&stat_buf))
    }

    /// Stats a file relative to the directory using statx (Linux 4.11+).
    ///
    /// Returns a lightweight `StatxResult` directly from the statx syscall,
    /// avoiding construction of `fs::Metadata`. Falls back to `stat_relative()`
    /// on older kernels.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be stat'd.
    #[cfg(all(target_os = "linux", not(target_env = "musl")))]
    pub fn statx_relative(
        &self,
        name: &OsString,
        follow_symlinks: bool,
    ) -> io::Result<StatxResult> {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;

        let name_bytes = name.as_bytes();
        let c_name = CString::new(name_bytes).map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("invalid filename: {e}"),
            )
        })?;

        let flags = if follow_symlinks {
            0i32
        } else {
            libc::AT_SYMLINK_NOFOLLOW
        };

        let mut statx_buf: libc::statx = unsafe { std::mem::zeroed() };

        let ret = unsafe {
            libc::syscall(
                libc::SYS_statx,
                self.dir_fd,
                c_name.as_ptr(),
                flags,
                libc::STATX_BASIC_STATS,
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

    /// Stats multiple files in the directory in parallel.
    #[cfg(feature = "parallel")]
    pub fn stat_batch_relative(
        &self,
        names: &[OsString],
        follow_symlinks: bool,
    ) -> Vec<io::Result<FstatResult>> {
        // Ordering: results must correspond 1:1 with input names by position.
        // Preserved by par_iter().map().collect() (rayon preserves index order).
        // Violation mismatches metadata with file names.
        names
            .par_iter()
            .map(|name| self.stat_relative(name, follow_symlinks))
            .collect()
    }

    /// Stats multiple files sequentially (non-parallel fallback).
    #[cfg(not(feature = "parallel"))]
    pub fn stat_batch_relative(
        &self,
        names: &[OsString],
        follow_symlinks: bool,
    ) -> Vec<io::Result<FstatResult>> {
        names
            .iter()
            .map(|name| self.stat_relative(name, follow_symlinks))
            .collect()
    }
}
