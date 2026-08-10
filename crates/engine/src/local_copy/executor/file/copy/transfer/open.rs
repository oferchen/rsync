//! Source file opening with optional `O_NOATIME` support.
//!
//! On Linux/Android, files are opened with `O_NOATIME` when requested to avoid
//! updating access times during transfers. This mirrors upstream rsync behavior
//! in `sender.c`. On other platforms, `O_NOATIME` is unavailable and the flag
//! is silently ignored.
//!
//! On macOS, large source files additionally receive an
//! `fcntl(fd, F_NOCACHE, 1)` advisory hint via `fast_io::apply_sequential_read_hint`.
//! This is the macOS analogue of Linux's `posix_fadvise(POSIX_FADV_DONTNEED)`:
//! the data is read once for a transfer and will not be revisited, so leaving
//! it in the unified buffer cache evicts unrelated hot pages.

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
///
/// On macOS the resulting handle additionally receives the sequential-read
/// advisory (`F_NOCACHE`) for files at or above
/// `fast_io::F_NOCACHE_THRESHOLD`. The handle is queried for its size via
/// `metadata()` so callers do not need to know it. The hint is best-effort:
/// filesystems that reject the flag silently fall back to cached I/O.
pub(in crate::local_copy) fn open_source_file(
    path: &Path,
    use_noatime: bool,
) -> io::Result<fs::File> {
    let file = if use_noatime && let Some(file) = try_open_noatime(path)? {
        file
    } else {
        fs::File::open(path)?
    };
    apply_macos_read_hint(&file);
    Ok(file)
}

/// Applies the macOS `F_NOCACHE` advisory hint for sequential source reads.
///
/// Best-effort: errors and non-macOS platforms are silently ignored. The
/// helper queries the file's own metadata for the size hint so the call site
/// stays small.
fn apply_macos_read_hint(file: &fs::File) {
    if let Ok(metadata) = file.metadata() {
        let _ = fast_io::apply_sequential_read_hint(file, metadata.len());
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn open_source_file_returns_readable_handle() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("source.bin");
        std::fs::write(&path, b"payload").unwrap();

        let mut file = open_source_file(&path, false).expect("open source");
        let mut contents = Vec::new();
        file.read_to_end(&mut contents).unwrap();
        assert_eq!(contents, b"payload");
    }

    #[test]
    fn open_source_file_large_file_succeeds_with_hint() {
        // The macOS `F_NOCACHE` hint runs through `apply_macos_read_hint` for
        // every successful open. Verify the helper does not perturb reads even
        // for files above the threshold. On non-macOS platforms the hint is a
        // no-op and the test still validates the open path.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("large_source.bin");
        let payload = vec![0xABu8; (fast_io::F_NOCACHE_THRESHOLD + 16) as usize];
        std::fs::write(&path, &payload).unwrap();

        let mut file = open_source_file(&path, false).expect("open large source");
        let mut contents = Vec::new();
        file.read_to_end(&mut contents).unwrap();
        assert_eq!(contents.len(), payload.len());
        assert_eq!(contents, payload);
    }
}
