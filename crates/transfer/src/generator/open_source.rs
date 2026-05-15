//! Source-file opening with optional `O_NOATIME` propagation.
//!
//! Mirrors upstream `syscall.c:228 do_open` and `syscall.c:687
//! do_open_nofollow` (the latter added `O_NOATIME` propagation in
//! rsync 3.4.2). On Linux/Android, when `--open-noatime` is set the
//! source file is opened with `OpenOptionsExt::custom_flags(O_NOATIME)`.
//! Permission failures (`EPERM`, `EACCES`) and filesystems that reject
//! the flag (`EINVAL`, `ENOTSUP`, `EROFS`) fall back to a plain
//! `File::open`, matching the local-copy executor's helper.
//!
//! On every other target the function is a thin wrapper over
//! `File::open(path)` because `O_NOATIME` is not defined.

use std::fs;
use std::io;
use std::path::Path;

#[cfg(any(target_os = "linux", target_os = "android"))]
use std::os::unix::fs::OpenOptionsExt;

#[cfg(any(target_os = "linux", target_os = "android"))]
use libc::{EACCES, EINVAL, ENOTSUP, EPERM, EROFS, O_NOATIME};

/// Opens a source file for reading, honouring `--open-noatime`.
///
/// upstream: syscall.c do_open / do_open_nofollow (3.4.2 propagates
/// `O_NOATIME` through both paths via the `open_noatime` global).
pub(super) fn open_source_with_noatime(path: &Path, use_noatime: bool) -> io::Result<fs::File> {
    if use_noatime {
        if let Some(file) = try_open_noatime(path)? {
            return Ok(file);
        }
    }
    fs::File::open(path)
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn try_open_noatime(path: &Path) -> io::Result<Option<fs::File>> {
    let mut options = fs::OpenOptions::new();
    options.read(true).custom_flags(O_NOATIME);
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn open_source_with_noatime_disabled_returns_readable_handle() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("source.bin");
        std::fs::write(&path, b"payload").unwrap();

        let mut file = open_source_with_noatime(&path, false).unwrap();
        let mut contents = Vec::new();
        file.read_to_end(&mut contents).unwrap();
        assert_eq!(contents, b"payload");
    }

    #[test]
    fn open_source_with_noatime_enabled_returns_readable_handle() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("source.bin");
        std::fs::write(&path, b"payload").unwrap();

        // On non-Linux this falls through to plain File::open; on Linux it
        // either succeeds via O_NOATIME or falls back on EPERM/EROFS/etc.
        let mut file = open_source_with_noatime(&path, true).unwrap();
        let mut contents = Vec::new();
        file.read_to_end(&mut contents).unwrap();
        assert_eq!(contents, b"payload");
    }

    /// Linux-only regression test for upstream 3.4.2 `O_NOATIME` parity.
    ///
    /// Backdates the source file's atime, reads through
    /// `open_source_with_noatime(_, true)`, and confirms the on-disk atime
    /// did not advance. Skips silently when the host filesystem rejects
    /// `O_NOATIME` (tmpfs in restricted containers; some overlayfs setups).
    #[cfg(target_os = "linux")]
    #[test]
    fn open_source_with_noatime_preserves_atime_on_linux() {
        use std::os::unix::fs::MetadataExt;
        use std::time::{Duration, SystemTime};

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("source.bin");
        std::fs::write(&path, b"payload-for-atime-check").unwrap();

        // Backdate atime to 1 hour ago so a fresh access would be visible.
        let past = SystemTime::now() - Duration::from_secs(3600);
        let past_ft = filetime::FileTime::from_system_time(past);
        filetime::set_file_atime(&path, past_ft).unwrap();

        let before = std::fs::metadata(&path).unwrap().atime();

        // Skip if the open returns success via the EPERM/EROFS fallback
        // path rather than via the O_NOATIME open.
        let mut options = fs::OpenOptions::new();
        options.read(true).custom_flags(O_NOATIME);
        if options.open(&path).is_err() {
            eprintln!("skip: O_NOATIME not honoured by this filesystem");
            return;
        }

        let mut file = open_source_with_noatime(&path, true).unwrap();
        let mut contents = Vec::new();
        file.read_to_end(&mut contents).unwrap();
        drop(file);

        let after = std::fs::metadata(&path).unwrap().atime();
        assert_eq!(
            before, after,
            "O_NOATIME should preserve atime (before={before}, after={after})"
        );
    }
}
