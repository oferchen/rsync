//! Timestamp helpers for tests, in place of `filetime`'s deprecated API.
//!
//! The setters keep `filetime`'s names and signatures and write through
//! [`fast_io::set_path_times`], which works on the pinned toolchain where
//! `std::fs::set_times` is not yet stable. The readers go through
//! `std::fs::Metadata`, not through `fast_io`'s readers, so a test that
//! compares production output against them keeps an independent oracle.

use std::fs::Metadata;
use std::io;
use std::path::Path;

use filetime::FileTime;

/// Sets the access and modification times of `path`, following symlinks.
///
/// # Errors
///
/// Returns the error [`fast_io::set_path_times`] reports.
pub fn set_file_times<P: AsRef<Path>>(path: P, atime: FileTime, mtime: FileTime) -> io::Result<()> {
    fast_io::set_path_times(path.as_ref(), Some(atime), Some(mtime), true)
}

/// Sets the modification time of `path`, following symlinks; the access time
/// is left unchanged.
///
/// # Errors
///
/// Returns the error [`fast_io::set_path_times`] reports.
pub fn set_file_mtime<P: AsRef<Path>>(path: P, mtime: FileTime) -> io::Result<()> {
    fast_io::set_path_times(path.as_ref(), None, Some(mtime), true)
}

/// Sets the access time of `path`, following symlinks; the modification time
/// is left unchanged.
///
/// # Errors
///
/// Returns the error [`fast_io::set_path_times`] reports.
pub fn set_file_atime<P: AsRef<Path>>(path: P, atime: FileTime) -> io::Result<()> {
    fast_io::set_path_times(path.as_ref(), Some(atime), None, true)
}

/// Sets the access and modification times of `path` itself, without
/// following a symlink.
///
/// # Errors
///
/// Returns the error [`fast_io::set_path_times`] reports.
pub fn set_symlink_file_times<P: AsRef<Path>>(
    path: P,
    atime: FileTime,
    mtime: FileTime,
) -> io::Result<()> {
    fast_io::set_path_times(path.as_ref(), Some(atime), Some(mtime), false)
}

/// Modification time of `metadata`.
///
/// # Panics
///
/// Panics when the platform reports no modification time.
#[must_use]
pub fn mtime_of(metadata: &Metadata) -> FileTime {
    FileTime::from_system_time(metadata.modified().expect("platform reports mtime"))
}

/// Access time of `metadata`.
///
/// # Panics
///
/// Panics when the platform reports no access time.
#[must_use]
pub fn atime_of(metadata: &Metadata) -> FileTime {
    FileTime::from_system_time(metadata.accessed().expect("platform reports atime"))
}
