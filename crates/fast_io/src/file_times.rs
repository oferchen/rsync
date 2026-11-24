//! File timestamps read from [`fs::Metadata`] and written to a path.
//!
//! `filetime` 0.2.30 deprecates its own readers and setters in favour of
//! `std::fs::Metadata::{modified, accessed}` and `std::fs::set_times`, but
//! `set_times` and `set_times_nofollow` are not stable on the pinned
//! toolchain. These functions keep the [`FileTime`] values the workspace
//! compares and stores:
//!
//! - the readers take the platform stat fields directly, so they cannot fail
//!   and keep every bit of precision: nanoseconds on Unix, 100 ns intervals on
//!   Windows, and times before 1970;
//! - [`set_path_times`] writes through `utimensat(2)` on Unix and through a
//!   handle opened for `FILE_WRITE_ATTRIBUTES` on Windows, leaving a `None`
//!   slot unchanged and touching a symlink itself when `follow_symlinks` is
//!   false.

use std::fs;
use std::io;
use std::path::Path;

use filetime::FileTime;

/// Modification time of `metadata`.
///
/// Equal to `FileTime::from_system_time(metadata.modified()?)` on every
/// supported platform, without the error path: Unix reads `st_mtime` and
/// `st_mtime_nsec`, Windows reads `ftLastWriteTime`.
#[must_use]
pub fn modified_filetime(metadata: &fs::Metadata) -> FileTime {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        unix_filetime(metadata.mtime(), metadata.mtime_nsec())
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        windows_filetime(metadata.last_write_time())
    }
}

/// Access time of `metadata`.
///
/// Equal to `FileTime::from_system_time(metadata.accessed()?)` on every
/// supported platform, without the error path: Unix reads `st_atime` and
/// `st_atime_nsec`, Windows reads `ftLastAccessTime`.
#[must_use]
pub fn accessed_filetime(metadata: &fs::Metadata) -> FileTime {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        unix_filetime(metadata.atime(), metadata.atime_nsec())
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        windows_filetime(metadata.last_access_time())
    }
}

/// Sets the access and modification times of `path`.
///
/// A `None` slot leaves that timestamp unchanged. With `follow_symlinks`
/// false a symlink's own times are set rather than its target's.
///
/// # Errors
///
/// Returns the operating system's error when `path` cannot be resolved or
/// its times cannot be written, and `InvalidInput` when `path` contains an
/// interior NUL (Unix) or a time cannot be represented (Windows).
pub fn set_path_times(
    path: &Path,
    atime: Option<FileTime>,
    mtime: Option<FileTime>,
    follow_symlinks: bool,
) -> io::Result<()> {
    #[cfg(unix)]
    {
        crate::dir_sandbox::at_syscalls::utimensat_path(path, atime, mtime, follow_symlinks)
    }
    #[cfg(windows)]
    {
        windows_set_times(path, atime, mtime, follow_symlinks)
    }
}

#[cfg(unix)]
fn unix_filetime(seconds: i64, nanoseconds: i64) -> FileTime {
    // POSIX keeps `st_*_nsec` in [0, 1_000_000_000), so it fits a `u32`.
    FileTime::from_unix_time(seconds, nanoseconds as u32)
}

/// 100 ns intervals per second in a Windows `FILETIME`.
#[cfg(windows)]
const INTERVALS_PER_SECOND: u64 = 10_000_000;

/// Seconds from 1601-01-01, the `FILETIME` epoch, to the Unix epoch.
#[cfg(windows)]
const UNIX_EPOCH_AFTER_1601: i64 = 11_644_473_600;

#[cfg(windows)]
fn windows_filetime(intervals: u64) -> FileTime {
    // `intervals / 10^7` is below 2^41 and the remainder times 100 is below
    // 10^9, so both conversions are lossless.
    let seconds = (intervals / INTERVALS_PER_SECOND) as i64 - UNIX_EPOCH_AFTER_1601;
    let nanoseconds = (intervals % INTERVALS_PER_SECOND) as u32 * 100;
    FileTime::from_unix_time(seconds, nanoseconds)
}

#[cfg(windows)]
fn windows_set_times(
    path: &Path,
    atime: Option<FileTime>,
    mtime: Option<FileTime>,
    follow_symlinks: bool,
) -> io::Result<()> {
    use std::fs::{FileTimes, OpenOptions};
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_WRITE_ATTRIBUTES,
    };

    // FILE_FLAG_BACKUP_SEMANTICS lets a directory be opened; the reparse
    // flag opens a symlink itself instead of its target.
    let mut flags = FILE_FLAG_BACKUP_SEMANTICS;
    if !follow_symlinks {
        flags |= FILE_FLAG_OPEN_REPARSE_POINT;
    }
    let file = OpenOptions::new()
        .access_mode(FILE_WRITE_ATTRIBUTES)
        .custom_flags(flags)
        .open(path)?;
    let mut times = FileTimes::new();
    if let Some(atime) = atime {
        times = times.set_accessed(system_time(atime)?);
    }
    if let Some(mtime) = mtime {
        times = times.set_modified(system_time(mtime)?);
    }
    file.set_times(times)
}

#[cfg(windows)]
fn system_time(time: FileTime) -> io::Result<std::time::SystemTime> {
    use std::time::{Duration, UNIX_EPOCH};

    let seconds = time.unix_seconds();
    let nanoseconds = Duration::from_nanos(u64::from(time.nanoseconds()));
    let whole = Duration::from_secs(seconds.unsigned_abs());
    let base = if seconds >= 0 {
        UNIX_EPOCH.checked_add(whole)
    } else {
        UNIX_EPOCH.checked_sub(whole)
    };
    base.and_then(|base| base.checked_add(nanoseconds))
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "file time out of range"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A modification time before 1970 with sub-second precision that a
    /// 100 ns clock can hold exactly, and an access time after 2038.
    fn sample_times() -> (FileTime, FileTime) {
        let atime = FileTime::from_unix_time(2_200_000_000, 987_654_300);
        let mtime = FileTime::from_unix_time(-86_400 * 365 - 17, 123_456_700);
        (atime, mtime)
    }

    /// The readers return exactly what was written, and exactly what the
    /// deprecated `filetime` constructors returned (`from_system_time` over
    /// `Metadata::{modified, accessed}`), so no precision is lost and times
    /// before 1970 keep their sign.
    #[test]
    fn readers_round_trip_nanoseconds_and_pre_1970_times() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("f");
        fs::write(&path, b"x").expect("write");
        let (atime, mtime) = sample_times();
        set_path_times(&path, Some(atime), Some(mtime), true).expect("set times");

        let meta = fs::metadata(&path).expect("metadata");
        assert_eq!(modified_filetime(&meta), mtime);
        assert_eq!(accessed_filetime(&meta), atime);
        assert_eq!(
            modified_filetime(&meta),
            FileTime::from_system_time(meta.modified().expect("modified"))
        );
        assert_eq!(
            accessed_filetime(&meta),
            FileTime::from_system_time(meta.accessed().expect("accessed"))
        );

        let entry = crate::EntryMetadata::from(meta);
        assert_eq!(entry.modified_filetime(), mtime);
        assert_eq!(entry.accessed_filetime(), atime);
    }

    /// A `None` slot is left as it was.
    #[test]
    fn none_slot_leaves_the_time_unchanged() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("f");
        fs::write(&path, b"x").expect("write");
        let (atime, mtime) = sample_times();
        set_path_times(&path, Some(atime), Some(mtime), true).expect("set both");

        let later = FileTime::from_unix_time(1_700_000_000, 0);
        set_path_times(&path, None, Some(later), true).expect("set mtime");
        let meta = fs::metadata(&path).expect("metadata");
        assert_eq!(modified_filetime(&meta), later);
        assert_eq!(accessed_filetime(&meta), atime);

        set_path_times(&path, Some(later), None, true).expect("set atime");
        let meta = fs::metadata(&path).expect("metadata");
        assert_eq!(modified_filetime(&meta), later);
        assert_eq!(accessed_filetime(&meta), later);
    }

    /// Without `follow_symlinks` the link's own times change and the
    /// target's do not; with it, the target's do.
    #[cfg(unix)]
    #[test]
    fn follow_symlinks_selects_the_link_or_its_target() {
        let dir = tempfile::tempdir().expect("tempdir");
        let target = dir.path().join("target");
        let link = dir.path().join("link");
        fs::write(&target, b"x").expect("write");
        std::os::unix::fs::symlink(&target, &link).expect("symlink");
        let base = FileTime::from_unix_time(1_000_000_000, 0);
        set_path_times(&target, Some(base), Some(base), true).expect("stamp target");

        let link_time = FileTime::from_unix_time(1_100_000_000, 5);
        set_path_times(&link, Some(link_time), Some(link_time), false).expect("stamp link");
        let link_meta = fs::symlink_metadata(&link).expect("link metadata");
        assert_eq!(modified_filetime(&link_meta), link_time);
        assert_eq!(
            modified_filetime(&fs::metadata(&target).expect("target")),
            base
        );

        let followed = FileTime::from_unix_time(1_200_000_000, 0);
        set_path_times(&link, None, Some(followed), true).expect("stamp through link");
        assert_eq!(
            modified_filetime(&fs::metadata(&target).expect("target")),
            followed
        );
        let link_meta = fs::symlink_metadata(&link).expect("link metadata");
        assert_eq!(modified_filetime(&link_meta), link_time);
    }

    #[test]
    fn missing_path_reports_not_found() {
        let dir = tempfile::tempdir().expect("tempdir");
        let time = FileTime::from_unix_time(1_000_000_000, 0);
        let err = set_path_times(&dir.path().join("absent"), Some(time), Some(time), true)
            .expect_err("absent path");
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
    }
}
