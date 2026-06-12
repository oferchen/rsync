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
///
/// Uses [`set_file_times`] for regular files/directories and
/// [`set_symlink_file_times`] for symlinks. Skips the syscall when both
/// mtime and atime already match `existing`.
///
/// When `options` is provided and `options.atimes()` is true, the atime
/// comparison is included in the skip check so that atime-only changes
/// still trigger a `utimensat` call. Without `options` (or when atimes is
/// disabled), only the mtime is compared.
// upstream: rsync.c:set_file_attrs() - utimensat / lutimensat based on follow flag
pub(super) fn set_timestamp_like(
    metadata: &fs::Metadata,
    destination: &Path,
    follow_symlinks: bool,
    existing: Option<&fs::Metadata>,
    options: Option<&MetadataOptions>,
) -> Result<(), MetadataError> {
    let accessed = FileTime::from_last_access_time(metadata);
    let modified = FileTime::from_last_modification_time(metadata);

    // upstream: rsync.c:597-612 - mtime and atime are checked independently;
    // the utimensat is only skipped when ALL relevant timestamps match.
    if let Some(existing) = existing {
        let mtime_matches = FileTime::from_last_modification_time(existing) == modified;
        let atime_matches = if options.is_some_and(|o| o.atimes()) {
            FileTime::from_last_access_time(existing) == accessed
        } else {
            true
        };
        if mtime_matches && atime_matches {
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
///
/// Avoids a path lookup by operating directly on the file descriptor.
/// Skips the syscall when both mtime and atime already match `existing`.
///
/// When `options` is provided and `options.atimes()` is true, the atime
/// comparison is included in the skip check.
// upstream: rsync.c:set_file_attrs() - futimens path when fd is available
#[cfg(unix)]
pub(super) fn set_timestamp_with_fd(
    metadata: &fs::Metadata,
    destination: &Path,
    fd: BorrowedFd<'_>,
    existing: Option<&fs::Metadata>,
    options: Option<&MetadataOptions>,
) -> Result<(), MetadataError> {
    let accessed = FileTime::from_last_access_time(metadata);
    let modified = FileTime::from_last_modification_time(metadata);

    // upstream: rsync.c:597-612 - mtime and atime are checked independently
    if let Some(existing) = existing {
        let mtime_matches = FileTime::from_last_modification_time(existing) == modified;
        let atime_matches = if options.is_some_and(|o| o.atimes()) {
            FileTime::from_last_access_time(existing) == accessed
        } else {
            true
        };
        if mtime_matches && atime_matches {
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

/// Applies only the access time from source `fs::Metadata`, preserving the
/// destination's existing mtime.
///
/// Used in the local copy path when `--atimes` is active but `--times` is not,
/// mirroring upstream's `set_file_attrs()` behavior where atime and mtime are
/// governed independently by `ATTRS_SKIP_ATIME` / `ATTRS_SKIP_MTIME`.
// upstream: rsync.c:604-612 - atime applied independently of mtime
pub(super) fn apply_atime_only_from_metadata(
    metadata: &fs::Metadata,
    destination: &Path,
    existing: Option<&fs::Metadata>,
) -> Result<(), MetadataError> {
    let source_atime = FileTime::from_last_access_time(metadata);

    if let Some(existing) = existing {
        if FileTime::from_last_access_time(existing) == source_atime {
            return Ok(());
        }
    }

    // Preserve the destination's current mtime - only update atime.
    let dest_mtime = match existing {
        Some(meta) => FileTime::from_last_modification_time(meta),
        None => {
            let meta = fs::metadata(destination).map_err(|error| {
                MetadataError::new("read current timestamps", destination, error)
            })?;
            FileTime::from_last_modification_time(&meta)
        }
    };

    set_file_times(destination, source_atime, dest_mtime)
        .map_err(|error| MetadataError::new("preserve access time", destination, error))?;

    Ok(())
}

/// fd-based variant of [`apply_atime_only_from_metadata`].
///
/// Uses `futimens` on the open fd to set only the atime while preserving the
/// destination's existing mtime.
// upstream: rsync.c:604-612 - atime applied independently of mtime
#[cfg(unix)]
pub(super) fn apply_atime_only_from_metadata_with_fd(
    metadata: &fs::Metadata,
    destination: &Path,
    fd: BorrowedFd<'_>,
    existing: Option<&fs::Metadata>,
) -> Result<(), MetadataError> {
    let source_atime = FileTime::from_last_access_time(metadata);

    if let Some(existing) = existing {
        if FileTime::from_last_access_time(existing) == source_atime {
            return Ok(());
        }
    }

    // Preserve the destination's current mtime - only update atime.
    let dest_mtime = match existing {
        Some(meta) => FileTime::from_last_modification_time(meta),
        None => {
            let meta = fs::metadata(destination).map_err(|error| {
                MetadataError::new("read current timestamps", destination, error)
            })?;
            FileTime::from_last_modification_time(&meta)
        }
    };

    let timestamps = rustix::fs::Timestamps {
        last_access: rustix::fs::Timespec {
            tv_sec: source_atime.unix_seconds(),
            tv_nsec: source_atime.nanoseconds().into(),
        },
        last_modification: rustix::fs::Timespec {
            tv_sec: dest_mtime.unix_seconds(),
            tv_nsec: dest_mtime.nanoseconds().into(),
        },
    };

    rustix::fs::futimens(fd, &timestamps).map_err(|error| {
        MetadataError::new("preserve access time", destination, io::Error::from(error))
    })?;

    Ok(())
}

/// Applies mtime (and atime when `--atimes`) from a protocol `FileEntry`.
///
/// When `--atimes` is not active, both atime and mtime are set to the entry's
/// mtime value. Skips the syscall when both timestamps already match
/// `cached_meta`.
// upstream: rsync.c:597 - `if (!(flags & ATTRS_SKIP_MTIME) && !same_mtime(...))`
pub(super) fn apply_timestamps_from_entry(
    destination: &Path,
    entry: &protocol::flist::FileEntry,
    options: &MetadataOptions,
    cached_meta: Option<&fs::Metadata>,
) -> Result<(), MetadataError> {
    let mtime = FileTime::from_unix_time(entry.mtime(), entry.mtime_nsec());

    // upstream: rsync.c:600-603 - preserves source atime with --atimes, else mtime for both
    let atime = if options.atimes() && entry.atime() != 0 {
        FileTime::from_unix_time(entry.atime(), 0)
    } else {
        mtime
    };

    // upstream: rsync.c:set_file_attrs() - skips utimensat when timestamps match
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

/// Applies mtime (and atime when `--atimes`) from a protocol `FileEntry`
/// to a symbolic link without following the link target.
///
/// Mirrors [`apply_timestamps_from_entry`] but uses `lutimes` /
/// `utimensat(AT_SYMLINK_NOFOLLOW)` via [`set_symlink_file_times`] so the
/// symlink's own mtime is updated instead of the link target's. The
/// receiver invokes this after `do_symlink` so the on-disk link mtime
/// matches the source-side value.
// upstream: rsync.c:set_file_attrs() + set_times() - symlink path uses lutimes
pub(super) fn apply_symlink_timestamps_from_entry(
    destination: &Path,
    entry: &protocol::flist::FileEntry,
    options: &MetadataOptions,
    cached_meta: Option<&fs::Metadata>,
) -> Result<(), MetadataError> {
    let mtime = FileTime::from_unix_time(entry.mtime(), entry.mtime_nsec());

    // upstream: rsync.c:600-603 - preserves source atime with --atimes, else mtime for both
    let atime = if options.atimes() && entry.atime() != 0 {
        FileTime::from_unix_time(entry.atime(), 0)
    } else {
        mtime
    };

    // upstream: rsync.c:set_file_attrs() - skips utimensat when timestamps match
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
        set_symlink_file_times(destination, atime, mtime)
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
// upstream: rsync.c:615 - `if (crtimes_ndx && !(flags & ATTRS_SKIP_CRTIME))`
pub(super) fn apply_crtime_from_entry(
    destination: &Path,
    entry: &protocol::flist::FileEntry,
) -> Result<(), MetadataError> {
    let crtime_secs = entry.crtime();
    set_crtime(destination, crtime_secs)
}

/// Sets the creation time (birth time) of a file on macOS via `setattrlist(2)`.
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

    // SAFETY: `attrlist` is a POD `repr(C)` structure with no validity
    // invariants; the all-zero pattern is a valid initial state. We overwrite
    // the relevant fields immediately below before any `setattrlist` call.
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
