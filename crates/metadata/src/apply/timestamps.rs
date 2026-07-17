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
    let modified = FileTime::from_last_modification_time(metadata);

    // upstream: rsync.c:588-589 - atime is written only when `--atimes`/`-U`
    // is active AND the node is not a directory (`!atimes_ndx || S_ISDIR`
    // sets ATTRS_SKIP_ATIME). Otherwise the destination's access time is left
    // unchanged. upstream: rsync.c:609 - when atime IS applied its nanosecond
    // field is forced to zero.
    let apply_atime = options.is_some_and(|o| o.atimes()) && !metadata.file_type().is_dir();
    let accessed = apply_atime.then(|| {
        FileTime::from_unix_time(FileTime::from_last_access_time(metadata).unix_seconds(), 0)
    });

    // upstream: rsync.c:597-612 - mtime and atime are checked independently;
    // the utimensat is only skipped when ALL relevant timestamps match.
    if let Some(existing) = existing {
        let mtime_matches = FileTime::from_last_modification_time(existing) == modified;
        let atime_matches = match accessed {
            Some(accessed) => FileTime::from_last_access_time(existing) == accessed,
            None => true,
        };
        if mtime_matches && atime_matches {
            return Ok(());
        }
    }

    // upstream: rsync.c:set_file_attrs() applies times through `utimensat` on
    // the path, never opening the target. filetime's follow variant
    // (`set_file_times`) opens the file first (`File::open`) before calling
    // `File::set_times`, which blocks indefinitely on a real FIFO that has no
    // reader/writer peer. Route special files (and symlinks) through the
    // `AT_SYMLINK_NOFOLLOW` `utimensat` variant, which sets the node's times
    // without opening it. For a non-symlink special file NOFOLLOW is
    // semantically identical to a follow, since the node is not a symlink.
    #[cfg(unix)]
    let open_free_path = !follow_symlinks || is_special_file(metadata);
    #[cfg(not(unix))]
    let open_free_path = !follow_symlinks;

    let result = match accessed {
        Some(accessed) => {
            if open_free_path {
                set_symlink_file_times(destination, accessed, modified)
            } else {
                set_file_times(destination, accessed, modified)
            }
        }
        None => set_mtime_omit_atime(destination, modified, open_free_path),
    };

    if let Err(error) = result {
        // upstream: util1.c set_times() is best-effort. Setting times on a
        // special file (device/fifo/socket) via utimensat can fail with
        // ENXIO/EROFS/EOPNOTSUPP on some kernels/filesystems (e.g. a char/block
        // device node with no backing media); upstream does not treat that as
        // fatal. Swallow those errnos for non-regular files only.
        #[cfg(unix)]
        if is_special_file(metadata) && is_tolerable_special_time_error(&error) {
            return Ok(());
        }
        return Err(MetadataError::new(
            "preserve timestamps",
            destination,
            error,
        ));
    }

    Ok(())
}

/// Reports whether the metadata describes a device, FIFO, or socket - the
/// special-file types for which timestamp application is best-effort.
#[cfg(unix)]
fn is_special_file(metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::FileTypeExt;
    let file_type = metadata.file_type();
    file_type.is_block_device()
        || file_type.is_char_device()
        || file_type.is_fifo()
        || file_type.is_socket()
}

/// Errnos upstream tolerates when setting times on a special file.
#[cfg(unix)]
fn is_tolerable_special_time_error(error: &io::Error) -> bool {
    matches!(
        error.raw_os_error(),
        Some(libc::ENXIO) | Some(libc::EROFS) | Some(libc::EOPNOTSUPP)
    )
}

/// Sets only the mtime on a path node, leaving the access time unchanged.
///
/// Mirrors upstream's `ATTRS_SKIP_ATIME` behaviour (`rsync.c:588-589`): when
/// `--atimes`/`-U` is off, or for any directory, the access time is left
/// untouched rather than being written to the mtime value. On Unix this uses
/// `utimensat` with `UTIME_OMIT` for the access-time slot so no access-time
/// syscall side effect leaks in; `open_free` selects the
/// `AT_SYMLINK_NOFOLLOW` variant used for symlinks and special files
/// (device/FIFO/socket), which updates the node's own mtime without opening
/// it (a peerless FIFO would otherwise block `File::open`). On non-Unix
/// targets `filetime::set_file_mtime` provides the equivalent mtime-only
/// update.
// upstream: rsync.c:588-589 - `!atimes_ndx || S_ISDIR` sets ATTRS_SKIP_ATIME
fn set_mtime_omit_atime(
    destination: &Path,
    mtime: FileTime,
    open_free: bool,
) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        let times = rustix::fs::Timestamps {
            last_access: rustix::fs::Timespec {
                tv_sec: 0,
                tv_nsec: rustix::fs::UTIME_OMIT,
            },
            last_modification: rustix::fs::Timespec {
                tv_sec: mtime.unix_seconds(),
                tv_nsec: mtime.nanoseconds().into(),
            },
        };
        let flags = if open_free {
            rustix::fs::AtFlags::SYMLINK_NOFOLLOW
        } else {
            rustix::fs::AtFlags::empty()
        };
        rustix::fs::utimensat(rustix::fs::CWD, destination, &times, flags).map_err(io::Error::from)
    }
    #[cfg(not(unix))]
    {
        let _ = open_free;
        filetime::set_file_mtime(destination, mtime)
    }
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
    let modified = FileTime::from_last_modification_time(metadata);

    // upstream: rsync.c:588-589 - atime is written only under `--atimes`/`-U`
    // and never for directories; rsync.c:609 - the applied atime nsec is 0.
    let apply_atime = options.is_some_and(|o| o.atimes()) && !metadata.file_type().is_dir();
    let accessed = apply_atime.then(|| {
        FileTime::from_unix_time(FileTime::from_last_access_time(metadata).unix_seconds(), 0)
    });

    // upstream: rsync.c:597-612 - mtime and atime are checked independently
    if let Some(existing) = existing {
        let mtime_matches = FileTime::from_last_modification_time(existing) == modified;
        let atime_matches = match accessed {
            Some(accessed) => FileTime::from_last_access_time(existing) == accessed,
            None => true,
        };
        if mtime_matches && atime_matches {
            return Ok(());
        }
    }

    // upstream: rsync.c:588-589,609 - omit the access time (UTIME_OMIT) when
    // it must not be written; otherwise set it with a zeroed nanosecond field.
    let last_access = match accessed {
        Some(accessed) => rustix::fs::Timespec {
            tv_sec: accessed.unix_seconds(),
            tv_nsec: 0,
        },
        None => rustix::fs::Timespec {
            tv_sec: 0,
            tv_nsec: rustix::fs::UTIME_OMIT,
        },
    };
    let timestamps = rustix::fs::Timestamps {
        last_access,
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
    // upstream: rsync.c:609 - the applied access time's nanosecond field is 0.
    let source_atime =
        FileTime::from_unix_time(FileTime::from_last_access_time(metadata).unix_seconds(), 0);

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
    // upstream: rsync.c:609 - the applied access time's nanosecond field is 0.
    let source_atime =
        FileTime::from_unix_time(FileTime::from_last_access_time(metadata).unix_seconds(), 0);

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

    // upstream: rsync.c:588-589 - the access time is written only under
    // `--atimes`/`-U` and never for directories (`!atimes_ndx || S_ISDIR`
    // sets ATTRS_SKIP_ATIME); otherwise it is left unchanged (UTIME_OMIT)
    // rather than being clobbered with the mtime value.
    let atime = if options.atimes() && !entry.file_type().is_dir() && entry.atime() != 0 {
        Some(FileTime::from_unix_time(entry.atime(), 0))
    } else {
        None
    };

    // upstream: rsync.c:set_file_attrs() - skips utimensat when timestamps match
    let needs_utime = match cached_meta {
        Some(meta) => {
            let current_mtime = FileTime::from_last_modification_time(meta);
            if current_mtime != mtime {
                true
            } else if let Some(atime) = atime {
                FileTime::from_last_access_time(meta) != atime
            } else {
                false
            }
        }
        None => true,
    };

    if needs_utime {
        set_entry_times(destination, entry, atime, mtime, "preserve timestamps")?;
    }

    Ok(())
}

/// Applies mtime/atime to a node materialised from a wire `FileEntry`, choosing
/// an open-free `utimensat` for special files.
///
/// filetime's follow variant (`set_file_times`) opens the target with
/// `File::open` before calling `File::set_times`, which blocks forever on a
/// FIFO that has no reader/writer peer - exactly the node the protocol receiver
/// materialises via `create_specials`. Device, FIFO, and socket nodes are never
/// symlinks, so the `AT_SYMLINK_NOFOLLOW` variant (`set_symlink_file_times`)
/// sets their times without opening them, matching upstream `set_file_attrs()`
/// which applies times through `utimensat` on the path and never opens the
/// target. Tolerable special-file errnos are swallowed as best-effort, mirroring
/// [`set_timestamp_like`].
// upstream: rsync.c:set_file_attrs() - utimensat on the path, never opens the node
fn set_entry_times(
    destination: &Path,
    entry: &protocol::flist::FileEntry,
    atime: Option<FileTime>,
    mtime: FileTime,
    context: &'static str,
) -> Result<(), MetadataError> {
    let is_special = entry.is_device() || entry.is_special();
    // upstream: rsync.c:588-589 - `atime = None` reproduces ATTRS_SKIP_ATIME,
    // leaving the destination's access time untouched (UTIME_OMIT).
    let result = match atime {
        Some(atime) => {
            if is_special {
                set_symlink_file_times(destination, atime, mtime)
            } else {
                set_file_times(destination, atime, mtime)
            }
        }
        None => set_mtime_omit_atime(destination, mtime, is_special),
    };

    if let Err(error) = result {
        #[cfg(unix)]
        if is_special && is_tolerable_special_time_error(&error) {
            return Ok(());
        }
        return Err(MetadataError::new(context, destination, error));
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

    // upstream: rsync.c:588-589 - a symlink is never a directory, so its atime
    // is written only under `--atimes`/`-U`; otherwise it is left unchanged
    // (UTIME_OMIT) rather than being clobbered with the mtime value.
    let atime = if options.atimes() && entry.atime() != 0 {
        Some(FileTime::from_unix_time(entry.atime(), 0))
    } else {
        None
    };

    // upstream: rsync.c:set_file_attrs() - skips utimensat when timestamps match
    let needs_utime = match cached_meta {
        Some(meta) => {
            let current_mtime = FileTime::from_last_modification_time(meta);
            if current_mtime != mtime {
                true
            } else if let Some(atime) = atime {
                FileTime::from_last_access_time(meta) != atime
            } else {
                false
            }
        }
        None => true,
    };

    if needs_utime {
        // upstream: rsync.c:set_times() uses lutimes/utimensat(AT_SYMLINK_NOFOLLOW)
        // on the symlink itself. `open_free = true` selects that NOFOLLOW variant.
        match atime {
            Some(atime) => set_symlink_file_times(destination, atime, mtime)
                .map_err(|error| MetadataError::new("preserve timestamps", destination, error))?,
            None => set_mtime_omit_atime(destination, mtime, true)
                .map_err(|error| MetadataError::new("preserve timestamps", destination, error))?,
        }
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
        set_entry_times(
            destination,
            entry,
            Some(atime),
            mtime,
            "preserve access time",
        )?;
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

/// Converts a Unix-epoch second count to a Windows `FILETIME` tick count
/// (100-ns intervals since 1601-01-01).
///
/// Returns `None` when the value predates the FILETIME epoch (a negative tick
/// count) or when the multiply/add overflows `i64`, so callers can skip the
/// `SetFileTime` call rather than write a bogus creation time. Defined for
/// `test` on every platform so the conversion math is covered cross-platform,
/// even though `set_crtime` only consumes it on Windows.
#[cfg(any(windows, test))]
fn unix_secs_to_filetime_ticks(secs: i64) -> Option<u64> {
    /// 100-ns ticks between the FILETIME epoch (1601-01-01) and the Unix epoch.
    const EPOCH_DIFFERENCE_100NS: i64 = 116_444_736_000_000_000;

    secs.checked_mul(10_000_000)
        .and_then(|t| t.checked_add(EPOCH_DIFFERENCE_100NS))
        .and_then(|t| u64::try_from(t).ok())
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

/// Sets the creation time (birth time) of a file or directory on Windows via
/// `SetFileTime`.
///
/// `SetFileTime`'s first time argument is the NTFS creation time. The handle is
/// opened with the minimal `FILE_WRITE_ATTRIBUTES` access right plus
/// `FILE_FLAG_BACKUP_SEMANTICS` so directories can be opened by the same call,
/// mirroring the reparse-handle helper. `secs` is a Unix epoch second count; it
/// is converted to a `FILETIME` (100-ns ticks since 1601-01-01). Pre-1601 or
/// overflowing inputs leave the destination crtime untouched rather than write
/// a bogus value.
// upstream: rsync.c uses utimensat for mtime/atime; crtime uses SetFileTime on Windows
#[cfg(windows)]
#[allow(unsafe_code)]
fn set_crtime(path: &Path, secs: i64) -> Result<(), MetadataError> {
    use std::os::windows::ffi::OsStrExt;

    use fast_io::to_extended_path;
    use windows::Win32::Foundation::{CloseHandle, FILETIME, HANDLE};
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
        SetFileTime,
    };

    /// `FILE_WRITE_ATTRIBUTES` access right (`winnt.h`). The `windows` crate
    /// exposes this only via `FILE_ACCESS_RIGHTS`; pinning the value locally
    /// keeps the call site small and version-independent.
    const FILE_WRITE_ATTRIBUTES: u32 = 0x0100;

    let to_err = |e: std::io::Error| MetadataError::new("set creation time", path, e);

    // Convert the Unix-epoch seconds to a FILETIME tick count up front; if the
    // value predates 1601 or overflows, skip the call rather than corrupt the
    // destination's creation time.
    let ticks = match unix_secs_to_filetime_ticks(secs) {
        Some(t) => t,
        None => return Ok(()),
    };
    let creation = FILETIME {
        dwLowDateTime: ticks as u32,
        dwHighDateTime: (ticks >> 32) as u32,
    };

    /// RAII guard that closes the opened handle on drop.
    struct OwnedHandle(HANDLE);
    impl Drop for OwnedHandle {
        fn drop(&mut self) {
            // SAFETY: `self.0` was returned by `CreateFileW` below and is owned
            // uniquely by this guard for its lifetime.
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }

    let wide: Vec<u16> = to_extended_path(path)
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    // SAFETY: `wide` is a NUL-terminated UTF-16 path owned for the duration of
    // the call. `FILE_FLAG_BACKUP_SEMANTICS` is the documented flag that lets
    // the same call open a directory as well as a file; no input/output
    // buffers besides the path are passed.
    let handle = unsafe {
        CreateFileW(
            windows::core::PCWSTR(wide.as_ptr()),
            FILE_WRITE_ATTRIBUTES,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            None,
        )
    }
    .map_err(|_| to_err(std::io::Error::last_os_error()))?;
    let handle = OwnedHandle(handle);

    // SAFETY: `handle.0` is a valid open handle with `FILE_WRITE_ATTRIBUTES`;
    // `creation` outlives the call (it is dropped at function end, after this
    // statement). The access- and write-time pointers are `None`, so only the
    // creation time is modified.
    unsafe { SetFileTime(handle.0, Some(&creation as *const FILETIME), None, None) }
        .map_err(|_| to_err(std::io::Error::last_os_error()))?;
    Ok(())
}

/// No-op stub for platforms where creation time cannot be set (Linux birthtime
/// is generally not settable; other non-macOS, non-Windows targets lack an API).
#[cfg(not(any(target_os = "macos", windows)))]
fn set_crtime(_path: &Path, _secs: i64) -> Result<(), MetadataError> {
    Ok(())
}

#[cfg(all(test, unix))]
mod fifo_hang_regression {
    use crate::{MetadataOptions, apply_metadata_from_file_entry, create_fifo_node_from_parts};
    use protocol::flist::FileEntry;
    use std::os::unix::fs::MetadataExt;
    use std::sync::mpsc;
    use std::time::Duration;

    /// Issue #223: the protocol receiver materialises a FIFO via `create_specials`
    /// and then applies metadata from the wire `FileEntry`. Timestamp application
    /// must NOT open the node - `File::open` on a FIFO with no peer blocks
    /// forever, deadlocking the wire receiver against the sender. The fix routes
    /// special files through the open-free `utimensat(AT_SYMLINK_NOFOLLOW)`. This
    /// test runs the apply on a worker thread and fails via timeout if it blocks,
    /// so a regression surfaces as a fast failure rather than a hung CI job.
    #[test]
    fn applying_times_to_fifo_does_not_block_open() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let fifo = tmp.path().join("f");
        create_fifo_node_from_parts(&fifo, 0o644, false, false).expect("create fifo");

        let mut entry = FileEntry::new_fifo("f".into(), 0o644);
        entry.set_mtime(1_000_000_000, 0);
        let options = MetadataOptions::new().preserve_times(true);

        let path = fifo.clone();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(apply_metadata_from_file_entry(&path, &entry, &options));
        });

        let result = rx.recv_timeout(Duration::from_secs(10)).expect(
            "apply_metadata_from_file_entry must not block opening a FIFO (issue #223 receiver hang)",
        );
        result.expect("metadata application should succeed on a fifo");

        let meta = std::fs::symlink_metadata(&fifo).expect("stat fifo");
        assert_eq!(
            meta.mtime(),
            1_000_000_000,
            "the open-free utimensat path must still set the FIFO's mtime",
        );
    }
}

#[cfg(all(test, unix))]
mod special_time_tests {
    use super::{is_special_file, is_tolerable_special_time_error};
    use std::io;

    #[test]
    fn tolerates_special_file_time_errnos() {
        for errno in [libc::ENXIO, libc::EROFS, libc::EOPNOTSUPP] {
            assert!(
                is_tolerable_special_time_error(&io::Error::from_raw_os_error(errno)),
                "errno {errno} should be tolerated for special files"
            );
        }
        assert!(!is_tolerable_special_time_error(
            &io::Error::from_raw_os_error(libc::EACCES)
        ));
        assert!(!is_tolerable_special_time_error(
            &io::Error::from_raw_os_error(libc::ENOENT)
        ));
    }

    #[test]
    fn regular_file_is_not_special() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("regular.txt");
        std::fs::write(&path, b"x").expect("write");
        let meta = std::fs::metadata(&path).expect("metadata");
        assert!(!is_special_file(&meta), "regular file must not be special");
        let dir_meta = std::fs::metadata(tmp.path()).expect("dir metadata");
        assert!(!is_special_file(&dir_meta), "directory must not be special");
    }
}

#[cfg(test)]
mod crtime_conversion_tests {
    use super::unix_secs_to_filetime_ticks;

    // 100-ns ticks from 1601-01-01 to 1970-01-01, the value SetFileTime expects
    // for a Unix-epoch-zero creation time.
    const EPOCH_DIFFERENCE: u64 = 116_444_736_000_000_000;

    #[test]
    fn unix_epoch_maps_to_filetime_epoch_difference() {
        assert_eq!(unix_secs_to_filetime_ticks(0), Some(EPOCH_DIFFERENCE));
    }

    #[test]
    fn one_second_adds_ten_million_ticks() {
        assert_eq!(
            unix_secs_to_filetime_ticks(1),
            Some(EPOCH_DIFFERENCE + 10_000_000)
        );
    }

    #[test]
    fn known_timestamp_round_trips() {
        // 2001-09-09T01:46:40Z == 1_000_000_000 Unix seconds.
        let ticks = unix_secs_to_filetime_ticks(1_000_000_000).expect("in range");
        assert_eq!(ticks, EPOCH_DIFFERENCE + 1_000_000_000 * 10_000_000);
    }

    #[test]
    fn pre_filetime_epoch_returns_none() {
        // Far enough before 1970 to land before 1601 (the FILETIME epoch), so
        // the tick count would be negative and must be rejected.
        assert_eq!(unix_secs_to_filetime_ticks(-12_000_000_000), None);
    }

    #[test]
    fn overflow_returns_none() {
        assert_eq!(unix_secs_to_filetime_ticks(i64::MAX), None);
    }
}
