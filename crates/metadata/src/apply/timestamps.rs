//! Timestamp propagation for mtime, atime, and creation time.
//!
//! Provides path-based and fd-based timestamp application using nanosecond
//! precision via the [`filetime`] crate. Includes creation time (crtime)
//! support on macOS via `setattrlist(2)` with a no-op stub for other platforms.

use crate::error::MetadataError;
use crate::options::MetadataOptions;
use filetime::{FileTime, set_file_times, set_symlink_file_times};
use std::fs;
use std::path::Path;

#[cfg(unix)]
use std::io;
#[cfg(unix)]
use std::os::fd::BorrowedFd;

/// Applies timestamps (atime + mtime) to a path, optionally following symlinks.
pub(super) fn set_timestamp_like(
    metadata: &fs::Metadata,
    destination: &Path,
    follow_symlinks: bool,
    existing: Option<&fs::Metadata>,
) -> Result<(), MetadataError> {
    let accessed = FileTime::from_last_access_time(metadata);
    let modified = FileTime::from_last_modification_time(metadata);

    if let Some(existing) = existing {
        if FileTime::from_last_modification_time(existing) == modified {
            return Ok(());
        }
    }

    if follow_symlinks {
        set_file_times(destination, accessed, modified)
            .map_err(|error| MetadataError::new("preserve timestamps", destination, error))?
    } else {
        set_symlink_file_times(destination, accessed, modified)
            .map_err(|error| MetadataError::new("preserve timestamps", destination, error))?
    }

    Ok(())
}

/// fd-based variant of [`set_timestamp_like`] that uses `futimens` on the open fd.
#[cfg(unix)]
pub(super) fn set_timestamp_with_fd(
    metadata: &fs::Metadata,
    destination: &Path,
    fd: BorrowedFd<'_>,
    existing: Option<&fs::Metadata>,
) -> Result<(), MetadataError> {
    let accessed = FileTime::from_last_access_time(metadata);
    let modified = FileTime::from_last_modification_time(metadata);

    if let Some(existing) = existing {
        if FileTime::from_last_modification_time(existing) == modified {
            return Ok(());
        }
    }

    let timestamps = rustix::fs::Timestamps {
        last_access: rustix::fs::Timespec {
            tv_sec: accessed.unix_seconds(),
            tv_nsec: accessed.nanoseconds().into(),
        },
        last_modification: rustix::fs::Timespec {
            tv_sec: modified.unix_seconds(),
            tv_nsec: modified.nanoseconds().into(),
        },
    };

    rustix::fs::futimens(fd, &timestamps).map_err(|error| {
        MetadataError::new("preserve timestamps", destination, io::Error::from(error))
    })?;

    Ok(())
}

/// Applies mtime (and atime when `--atimes`) from a protocol `FileEntry`.
pub(super) fn apply_timestamps_from_entry(
    destination: &Path,
    entry: &protocol::flist::FileEntry,
    options: &MetadataOptions,
    cached_meta: Option<&fs::Metadata>,
) -> Result<(), MetadataError> {
    let mtime = FileTime::from_unix_time(entry.mtime(), entry.mtime_nsec());

    // upstream: rsync preserves the source atime when --atimes is set;
    // otherwise it uses mtime for both atime and mtime (the default).
    let atime = if options.atimes() && entry.atime() != 0 {
        FileTime::from_unix_time(entry.atime(), 0)
    } else {
        mtime
    };

    // Optimization: skip syscall if timestamps already match.
    // Mirrors upstream's redundant-utimensat avoidance.
    let needs_utime = match cached_meta {
        Some(meta) => {
            let current_mtime = FileTime::from_last_modification_time(meta);
            if current_mtime != mtime {
                true
            } else if options.atimes() {
                let current_atime = FileTime::from_last_access_time(meta);
                current_atime != atime
            } else {
                false
            }
        }
        None => true,
    };

    if needs_utime {
        set_file_times(destination, atime, mtime)
            .map_err(|error| MetadataError::new("preserve timestamps", destination, error))?;
    }

    Ok(())
}

/// Applies only the access time from a `FileEntry`, preserving the
/// destination's existing mtime.
///
/// Used when `ATTRS_SKIP_MTIME` is active but `ATTRS_SKIP_ATIME` is not.
///
/// # Upstream Reference
///
/// - `rsync.c:604-612` - atime is applied independently of mtime when
///   `!(flags & ATTRS_SKIP_ATIME)` but `(flags & ATTRS_SKIP_MTIME)`.
pub(super) fn apply_atime_only_from_entry(
    destination: &Path,
    entry: &protocol::flist::FileEntry,
    cached_meta: Option<&fs::Metadata>,
) -> Result<(), MetadataError> {
    let atime = if entry.atime() != 0 {
        FileTime::from_unix_time(entry.atime(), 0)
    } else {
        return Ok(());
    };

    let needs_update = match cached_meta {
        Some(meta) => {
            let current_atime = FileTime::from_last_access_time(meta);
            current_atime != atime
        }
        None => true,
    };

    if needs_update {
        let mtime = match cached_meta {
            Some(meta) => FileTime::from_last_modification_time(meta),
            None => {
                let meta = fs::metadata(destination).map_err(|error| {
                    MetadataError::new("read current timestamps", destination, error)
                })?;
                FileTime::from_last_modification_time(&meta)
            }
        };
        set_file_times(destination, atime, mtime)
            .map_err(|error| MetadataError::new("preserve access time", destination, error))?;
    }

    Ok(())
}

/// Applies the creation time from source `fs::Metadata` to the destination.
///
/// Reads the birth time via `metadata.created()` and applies it using
/// `set_crtime`. On platforms where `created()` is unavailable, this is a no-op.
// upstream: rsync.c:615 - crtime application after file transfer
pub(super) fn apply_crtime_from_source_metadata(
    destination: &Path,
    metadata: &fs::Metadata,
) -> Result<(), MetadataError> {
    if let Ok(created) = metadata.created() {
        if let Ok(duration) = created.duration_since(std::time::UNIX_EPOCH) {
            let secs = duration.as_secs() as i64;
            if secs > 0 {
                set_crtime(destination, secs)?;
            }
        }
    }
    Ok(())
}

/// Applies the creation time from a `FileEntry` to the destination file.
///
/// On macOS this uses `setattrlist(2)` with `ATTR_CMN_CRTIME`. On other
/// platforms this is a no-op since creation time is not universally settable.
pub(super) fn apply_crtime_from_entry(
    destination: &Path,
    entry: &protocol::flist::FileEntry,
) -> Result<(), MetadataError> {
    let crtime_secs = entry.crtime();
    set_crtime(destination, crtime_secs)
}

/// Sets the creation time (birth time) of a file on macOS via `setattrlist(2)`.
///
/// # Safety
///
/// Calls into the libc `setattrlist` function through FFI. The `attrlist`
/// struct and buffer are stack-allocated with correct layout and size,
/// and the path is converted to a NUL-terminated C string before passing.
// upstream: rsync.c uses utimensat for mtime/atime; crtime uses setattrlist on macOS
#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
fn set_crtime(path: &Path, secs: i64) -> Result<(), MetadataError> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    #[repr(C)]
    struct AttrBuf {
        timespec: libc::timespec,
    }

    let c_path = CString::new(path.as_os_str().as_bytes()).map_err(|_| {
        MetadataError::new(
            "set creation time",
            path,
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "path contains NUL byte"),
        )
    })?;

    let mut attrlist: libc::attrlist = unsafe { std::mem::zeroed() };
    attrlist.bitmapcount = libc::ATTR_BIT_MAP_COUNT;
    attrlist.commonattr = libc::ATTR_CMN_CRTIME;

    let buf = AttrBuf {
        timespec: libc::timespec {
            tv_sec: secs,
            tv_nsec: 0,
        },
    };

    // SAFETY: `c_path` is a valid NUL-terminated C string, `attrlist` is
    // zeroed and then configured with valid bitmap values, and `buf` is a
    // repr(C) struct with the exact layout expected by `setattrlist(2)`.
    let ret = unsafe {
        libc::setattrlist(
            c_path.as_ptr(),
            &attrlist as *const _ as *mut _,
            &buf as *const _ as *mut libc::c_void,
            std::mem::size_of::<AttrBuf>(),
            0,
        )
    };

    if ret != 0 {
        return Err(MetadataError::new(
            "set creation time",
            path,
            std::io::Error::last_os_error(),
        ));
    }
    Ok(())
}

/// No-op stub for platforms where creation time cannot be set.
#[cfg(not(target_os = "macos"))]
fn set_crtime(_path: &Path, _secs: i64) -> Result<(), MetadataError> {
    Ok(())
}
