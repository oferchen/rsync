//! Directory-relative stat operations using `openat`/`fstatat`.

use std::ffi::OsString;
use std::fs;
use std::io;
use std::os::fd::AsFd;
use std::path::Path;

#[cfg(feature = "parallel")]
use rayon::prelude::*;

use super::types::FstatResult;
#[cfg(all(target_os = "linux", not(target_env = "musl")))]
use fast_io::statx::StatxResult;

/// Batch metadata fetcher for directory entries.
///
/// Uses `openat`/`fstatat` to reduce path resolution overhead when
/// fetching metadata for many files in the same directory. The syscalls
/// themselves are issued by `fast_io`, the I/O-syscall owner crate, so
/// this crate stays free of `unsafe`.
pub struct DirectoryStatBatch {
    dir: fs::File,
}

impl DirectoryStatBatch {
    /// Opens a directory for batched stat operations.
    ///
    /// # Errors
    ///
    /// Returns an error if the directory cannot be opened.
    pub fn open<P: AsRef<Path>>(dir_path: P) -> io::Result<Self> {
        Ok(Self {
            dir: fs::File::open(dir_path.as_ref())?,
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
        let at = if follow_symlinks {
            fast_io::fstatat_follow(self.dir.as_fd(), name)?
        } else {
            fast_io::fstatat_nofollow(self.dir.as_fd(), name)?
        };
        Ok(FstatResult::from_at_metadata(&at))
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
        fast_io::statx::statx_at(self.dir.as_fd(), name, follow_symlinks)
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
