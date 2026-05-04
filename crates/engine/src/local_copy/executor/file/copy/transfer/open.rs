//! Source file opening with optional `O_NOATIME` support.
//!
//! On Linux/Android, files are opened with `O_NOATIME` when requested to avoid
//! updating access times during transfers. This mirrors upstream rsync behavior
//! in `sender.c`. On other platforms, `O_NOATIME` is unavailable and the flag
//! is silently ignored.

use std::fs;
use std::io;
use std::path::Path;

#[cfg(any(target_os = "linux", target_os = "android"))]
use std::os::unix::fs::OpenOptionsExt;

#[cfg(any(target_os = "linux", target_os = "android"))]
use libc::{self, EACCES, EINVAL, ENOTSUP, EPERM, EROFS};

#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering};

#[cfg(test)]
use crate::local_copy::LocalCopyError;

#[cfg(test)]
static FSYNC_CALL_COUNT: AtomicUsize = AtomicUsize::new(0);

/// Opens a source file for reading, optionally with `O_NOATIME`.
///
/// When `use_noatime` is true and the platform supports it, the file is opened
/// with `O_NOATIME` to preserve access timestamps. If the `O_NOATIME` open
/// fails with a permission-related error, falls back to a normal open.
pub(in crate::local_copy) fn open_source_file(
    path: &Path,
    use_noatime: bool,
) -> io::Result<fs::File> {
    if use_noatime && let Some(file) = try_open_noatime(path)? {
        return Ok(file);
    }
    fs::File::open(path)
}

/// Attempts to open a file with `O_NOATIME` on Linux/Android.
///
/// Returns `Ok(Some(file))` on success, `Ok(None)` if the flag is rejected
/// (EPERM, EACCES, EINVAL, ENOTSUP, EROFS), or propagates other errors.
#[cfg(any(target_os = "linux", target_os = "android"))]
fn try_open_noatime(path: &Path) -> io::Result<Option<fs::File>> {
    let mut options = fs::OpenOptions::new();
    options.read(true).custom_flags(libc::O_NOATIME);
    match options.open(path) {
        Ok(file) => Ok(Some(file)),
        Err(error) => match error.raw_os_error() {
            Some(EPERM | EACCES | EINVAL | ENOTSUP | EROFS) => Ok(None),
            _ => Err(error),
        },
    }
}

#[cfg(not(any(target_os = "linux", target_os = "android")))]
fn try_open_noatime(_path: &Path) -> io::Result<Option<fs::File>> {
    Ok(None)
}

/// Syncs a destination file and records the fsync call (test-only).
#[cfg(test)]
#[allow(dead_code)]
pub(in crate::local_copy) fn sync_destination_file(
    writer: &mut fs::File,
    path: &Path,
) -> Result<(), LocalCopyError> {
    writer
        .sync_all()
        .map_err(|error| LocalCopyError::io("fsync destination file", path, error))?;
    record_fsync_call();
    Ok(())
}

#[cfg(test)]
#[allow(dead_code)]
fn record_fsync_call() {
    FSYNC_CALL_COUNT.fetch_add(1, Ordering::Relaxed);
}

/// Resets the fsync call counter and returns its previous value.
#[cfg(test)]
#[allow(dead_code)]
pub(crate) fn take_fsync_call_count() -> usize {
    FSYNC_CALL_COUNT.swap(0, Ordering::Relaxed)
}
