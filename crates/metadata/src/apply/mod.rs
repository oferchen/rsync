//! Metadata application orchestration.
//!
//! Re-exports the public API for applying ownership, permissions, and
//! timestamps to files, directories, and symbolic links. Internal logic
//! is split across focused submodules following the single-responsibility
//! principle.

mod ownership;
mod permissions;
mod timestamps;

#[cfg(test)]
mod tests;

use crate::error::MetadataError;
use crate::options::{AttrsFlags, MetadataOptions};
use std::fs;
#[cfg(unix)]
use std::os::fd::BorrowedFd;
use std::path::Path;

/// Applies metadata from `metadata` to the destination directory.
///
/// Preserves permission bits (best-effort on non-Unix targets) and
/// nanosecond timestamps. Delegates to [`apply_directory_metadata_with_options`]
/// with default options (all preservation flags enabled).
// upstream: rsync.c:set_file_attrs() - directory metadata application
pub fn apply_directory_metadata(
    destination: &Path,
    metadata: &fs::Metadata,
) -> Result<(), MetadataError> {
    apply_directory_metadata_with_options(destination, metadata, MetadataOptions::default())
}

/// Applies metadata from `metadata` to the destination directory using explicit options.
///
/// Applies ownership, permissions, and timestamps in the same order as
/// upstream rsync's `set_file_attrs()`: chown, chmod, then utimensat.
// upstream: rsync.c:set_file_attrs() - order: chown → chmod → utimensat
pub fn apply_directory_metadata_with_options(
    destination: &Path,
    metadata: &fs::Metadata,
    options: MetadataOptions,
) -> Result<(), MetadataError> {
    ownership::set_owner_like(metadata, destination, true, &options, None)?;
    permissions::apply_permissions_with_chmod(destination, metadata, &options, None)?;
    if options.times() {
        timestamps::set_timestamp_like(metadata, destination, true, None, Some(&options))?;
    }
    // upstream: rsync.c:589 - directories skip atime (`ATTRS_SKIP_ATIME`)
    // regardless of `--atimes`; only files get atime preservation.
    Ok(())
}

/// Applies metadata from `metadata` to the destination file.
///
/// Preserves permission bits (best-effort on non-Unix targets) and
/// nanosecond timestamps. Delegates to [`apply_file_metadata_with_options`]
/// with default options (all preservation flags enabled).
// upstream: rsync.c:set_file_attrs() - file metadata application
pub fn apply_file_metadata(
    destination: &Path,
    metadata: &fs::Metadata,
) -> Result<(), MetadataError> {
    apply_file_metadata_with_options(destination, metadata, &MetadataOptions::default())
}

/// Applies file metadata using explicit [`MetadataOptions`].
///
/// Applies ownership, permissions, timestamps, and creation time in the
/// same order as upstream rsync's `set_file_attrs()`.
// upstream: rsync.c:set_file_attrs() - order: chown → chmod → utimensat → crtime
pub fn apply_file_metadata_with_options(
    destination: &Path,
    metadata: &fs::Metadata,
    options: &MetadataOptions,
) -> Result<(), MetadataError> {
    ownership::set_owner_like(metadata, destination, true, options, None)?;
    permissions::apply_permissions_with_chmod(destination, metadata, options, None)?;
    // upstream: rsync.c:587-612 - mtime and atime are handled independently
    if options.times() {
        timestamps::set_timestamp_like(metadata, destination, true, None, Some(options))?;
    } else if options.atimes() {
        // upstream: rsync.c:604-612 - atime applied when SKIP_MTIME but not SKIP_ATIME
        timestamps::apply_atime_only_from_metadata(
            metadata,
            destination,
            None,
            options.keep_dirlinks(),
        )?;
    }
    if options.crtimes() {
        timestamps::apply_crtime_from_source_metadata(destination, metadata)?;
    }
    Ok(())
}

/// Pre-applies upstream's `dest_mode()` chmod for callers that have the
/// pre-transfer destination stat in hand.
///
/// See `permissions::apply_dest_mode_pre_transfer` for the full
/// upstream-reference documentation.
#[cfg(unix)]
pub fn apply_dest_mode_pre_transfer(
    destination: &Path,
    source_metadata: &fs::Metadata,
    options: &MetadataOptions,
    pre_transfer_meta: Option<&fs::Metadata>,
) -> Result<(), MetadataError> {
    permissions::apply_dest_mode_pre_transfer(
        destination,
        source_metadata,
        options,
        pre_transfer_meta,
    )
}

/// Reports whether a transfer-root directory self-locks under the configured
/// `--chmod` modifiers, returning the tweaked permission bits alongside the
/// verdict, or `None` when no `--chmod` is configured.
///
/// `am_root` is sampled through the same libc `geteuid` the chmod apply path
/// uses, so the self-lock decision and the on-disk fixup agree under
/// `fakeroot`. See [`crate::transfer_root_self_locks`] for the mechanism.
// upstream: rsync.c:set_file_attrs() new_mode + generator.c:1512 fixup.
#[cfg(unix)]
pub fn transfer_root_chmod_self_lock(
    destination: &Path,
    metadata: &fs::Metadata,
    options: &MetadataOptions,
    existing: Option<&fs::Metadata>,
) -> Result<Option<(u32, bool)>, MetadataError> {
    let Some(tweaked) =
        permissions::chmod_directory_target_mode(destination, metadata, options, existing)?
    else {
        return Ok(None);
    };
    let running_as_root = nix::unistd::geteuid().is_root();
    Ok(Some((
        tweaked,
        crate::transfer_root_self_locks(tweaked, running_as_root),
    )))
}

/// Applies file metadata using an open file descriptor for efficiency.
///
/// When an fd is available (e.g. after writing a file), this avoids redundant
/// path lookups by using `fchmod`/`fchown`/`futimens` instead of their
/// path-based equivalents. Falls back to path-based operations where fd-based
/// variants are unavailable (e.g. chmod modifiers that need a fresh stat).
#[cfg(unix)]
pub fn apply_file_metadata_with_fd(
    destination: &Path,
    metadata: &fs::Metadata,
    options: &MetadataOptions,
    fd: BorrowedFd<'_>,
) -> Result<(), MetadataError> {
    ownership::set_owner_like_with_fd(metadata, destination, options, fd, None)?;
    permissions::apply_permissions_with_chmod_fd(destination, metadata, options, Some(fd), None)?;
    // upstream: rsync.c:587-612 - mtime and atime are handled independently
    if options.times() {
        timestamps::set_timestamp_with_fd(metadata, destination, fd, None, Some(options))?;
    } else if options.atimes() {
        timestamps::apply_atime_only_from_metadata_with_fd(metadata, destination, fd, None)?;
    }
    // crtime is always path-based (setattrlist on macOS) - no fd variant exists
    if options.crtimes() {
        timestamps::apply_crtime_from_source_metadata(destination, metadata)?;
    }
    Ok(())
}

/// Applies only the file metadata fields that differ from `existing`.
///
/// Compares each metadata field (ownership, permissions, timestamps) against
/// the destination's current state and skips syscalls for values that already
/// match. This eliminates redundant `chown`/`chmod`/`utimensat` calls on the
/// no-change transfer path.
pub fn apply_file_metadata_if_changed(
    destination: &Path,
    metadata: &fs::Metadata,
    existing: &fs::Metadata,
    options: &MetadataOptions,
) -> Result<(), MetadataError> {
    let restat_after_chown =
        ownership::set_owner_like(metadata, destination, true, options, Some(existing))?;
    // upstream: rsync.c:564-567 - the chown may have cleared setuid/setgid bits,
    // so re-stat before the chmod compare so they get re-applied.
    let refreshed;
    let existing = if restat_after_chown {
        refreshed = fs::metadata(destination).map_err(|error| {
            MetadataError::new("inspect destination permissions", destination, error)
        })?;
        &refreshed
    } else {
        existing
    };
    permissions::apply_permissions_with_chmod(destination, metadata, options, Some(existing))?;
    // upstream: rsync.c:587-612 - mtime and atime are handled independently
    if options.times() {
        timestamps::set_timestamp_like(metadata, destination, true, Some(existing), Some(options))?;
    } else if options.atimes() {
        timestamps::apply_atime_only_from_metadata(
            metadata,
            destination,
            Some(existing),
            options.keep_dirlinks(),
        )?;
    }
    if options.crtimes() {
        timestamps::apply_crtime_from_source_metadata(destination, metadata)?;
    }
    Ok(())
}

/// fd-based variant of [`apply_file_metadata_if_changed`].
///
/// Combines fd-based syscalls with comparison guards - only issues
/// `fchown`/`fchmod`/`futimens` when the value actually differs from
/// `existing`.
#[cfg(unix)]
pub fn apply_file_metadata_with_fd_if_changed(
    destination: &Path,
    metadata: &fs::Metadata,
    existing: &fs::Metadata,
    options: &MetadataOptions,
    fd: BorrowedFd<'_>,
) -> Result<(), MetadataError> {
    let restat_after_chown =
        ownership::set_owner_like_with_fd(metadata, destination, options, fd, Some(existing))?;
    // upstream: rsync.c:564-567 - the chown may have cleared setuid/setgid bits,
    // so re-stat before the chmod compare so they get re-applied.
    let refreshed;
    let existing = if restat_after_chown {
        refreshed = fs::metadata(destination).map_err(|error| {
            MetadataError::new("inspect destination permissions", destination, error)
        })?;
        &refreshed
    } else {
        existing
    };
    permissions::apply_permissions_with_chmod_fd(
        destination,
        metadata,
        options,
        Some(fd),
        Some(existing),
    )?;
    // upstream: rsync.c:587-612 - mtime and atime are handled independently
    if options.times() {
        timestamps::set_timestamp_with_fd(
            metadata,
            destination,
            fd,
            Some(existing),
            Some(options),
        )?;
    } else if options.atimes() {
        timestamps::apply_atime_only_from_metadata_with_fd(
            metadata,
            destination,
            fd,
            Some(existing),
        )?;
    }
    // crtime is always path-based (setattrlist on macOS) - no fd variant exists
    if options.crtimes() {
        timestamps::apply_crtime_from_source_metadata(destination, metadata)?;
    }
    Ok(())
}

/// Fast check whether all metadata attributes already match the destination.
///
/// Mirrors upstream `generator.c:461 unchanged_attrs()` - a pure in-memory
/// comparison that avoids the function-call overhead of the full
/// [`apply_metadata_with_cached_stat`] path. Returns `true` when every
/// preserved attribute (permissions, ownership, timestamps) matches the
/// cached stat, so the caller can skip the metadata-application chain
/// entirely on the no-change quick-check path.
///
/// # Upstream Reference
///
/// - `generator.c:461-502` - `unchanged_attrs()` checks `perms_differ`,
///   `ownership_differs`, `any_time_differs`, `acls_differ`, `xattrs_differ`
/// - `generator.c:1809-1814` - quick-check match calls `set_file_attrs` only
///   when `unchanged_attrs` would fail (implicit - upstream always calls
///   `set_file_attrs` but its internal guards skip every syscall when nothing
///   differs)
#[inline]
pub fn metadata_unchanged(
    entry: &protocol::flist::FileEntry,
    options: &MetadataOptions,
    cached_meta: &fs::Metadata,
) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;

        // upstream: generator.c:487-488 - perms_differ(file, sxp)
        if options.permissions() && (cached_meta.mode() & 0o7777) != (entry.permissions() & 0o7777)
        {
            return false;
        }

        // upstream: generator.c:489-490 - ownership_differs(file, sxp)
        if options.owner() {
            if let Some(uid) = entry.uid() {
                if cached_meta.uid() != uid {
                    return false;
                }
            }
        }
        if options.group() {
            if let Some(gid) = entry.gid() {
                if cached_meta.gid() != gid {
                    return false;
                }
            }
        }

        // upstream: generator.c:485-486 - any_time_differs(sxp, file, fname)
        // Compare raw seconds + nanoseconds directly instead of constructing
        // FileTime structs on the hot path.
        if options.times()
            && (cached_meta.mtime() != entry.mtime()
                || cached_meta.mtime_nsec() as u32 != entry.mtime_nsec())
        {
            return false;
        }

        // upstream: rsync.c unchanged_attrs - atime comparison uses seconds only
        if options.atimes()
            && entry.atime() != 0
            && (cached_meta.atime() != entry.atime() || cached_meta.atime_nsec() != 0)
        {
            return false;
        }
    }

    #[cfg(not(unix))]
    {
        if options.times() {
            let current_mtime = filetime::FileTime::from_last_modification_time(cached_meta);
            let entry_mtime = filetime::FileTime::from_unix_time(entry.mtime(), entry.mtime_nsec());
            if current_mtime != entry_mtime {
                return false;
            }
        }

        if options.atimes() && entry.atime() != 0 {
            let current_atime = filetime::FileTime::from_last_access_time(cached_meta);
            let entry_atime = filetime::FileTime::from_unix_time(entry.atime(), 0);
            if current_atime != entry_atime {
                return false;
            }
        }
    }

    // upstream: generator.c:495-502 - chmod modifiers applied on top of the
    // entry's mode. Evaluate the modifier against the current stat and only
    // fall through to the full apply path when the result would differ.
    #[cfg(unix)]
    if let Some(chmod) = options.chmod() {
        use std::os::unix::fs::MetadataExt;
        let base_mode = if options.permissions() {
            entry.permissions()
        } else {
            cached_meta.mode()
        };
        let new_mode = chmod.apply(base_mode, cached_meta.file_type());
        if (cached_meta.mode() & 0o7777) != (new_mode & 0o7777) {
            return false;
        }
    }
    #[cfg(not(unix))]
    if options.chmod().is_some() {
        return false;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if let Some(uid) = options.owner_override() {
            if cached_meta.uid() != uid {
                return false;
            }
        }
        if let Some(gid) = options.group_override() {
            if cached_meta.gid() != gid {
                return false;
            }
        }
    }
    #[cfg(not(unix))]
    if options.owner_override().is_some() || options.group_override().is_some() {
        return false;
    }

    true
}

/// Applies metadata from `metadata` to the destination symbolic link without
/// following the link target. Delegates to [`apply_symlink_metadata_with_options`]
/// with default options.
// upstream: rsync.c:set_file_attrs() - symlink path uses AT_SYMLINK_NOFOLLOW
pub fn apply_symlink_metadata(
    destination: &Path,
    metadata: &fs::Metadata,
) -> Result<(), MetadataError> {
    apply_symlink_metadata_with_options(destination, metadata, &MetadataOptions::default())
}

/// Applies symbolic link metadata using explicit [`MetadataOptions`].
///
/// Only ownership and timestamps are applied - permissions are not preserved
/// for symlinks because most systems ignore symlink permission bits.
// upstream: rsync.c:set_file_attrs() - skips chmod for symlinks
pub fn apply_symlink_metadata_with_options(
    destination: &Path,
    metadata: &fs::Metadata,
    options: &MetadataOptions,
) -> Result<(), MetadataError> {
    ownership::set_owner_like(metadata, destination, false, options, None)?;
    if options.times() {
        timestamps::set_timestamp_like(metadata, destination, false, None, Some(options))?;
    }
    Ok(())
}

/// Applies metadata from a protocol `FileEntry` to a destination symbolic link
/// without following the link target.
///
/// Mirrors [`apply_metadata_from_file_entry`] but uses `lstat` for the cached
/// stat and `lutimes` / `utimensat(AT_SYMLINK_NOFOLLOW)` for timestamps so the
/// link's own mtime is updated instead of the target's. Permissions are not
/// preserved for symlinks because most systems ignore symlink permission bits;
/// ownership (when applicable) is applied with `AT_SYMLINK_NOFOLLOW`.
///
/// This is the receiver-side counterpart to [`apply_symlink_metadata`] that
/// works directly with `FileEntry` metadata from the wire protocol, so the
/// network receiver does not need to construct an [`fs::Metadata`] instance
/// before calling it.
///
/// # Upstream Reference
///
/// - `rsync.c:set_file_attrs()` - skips chmod for symlinks
/// - `rsync.c:set_times()` - uses `lutimes` when the target is a symlink
/// - `generator.c:1592` - `set_file_attrs(fname, file, NULL, NULL, 0)` runs
///   after `atomic_create` -> `do_symlink` so the new symlink's mtime matches
///   the sender's `F_MOD_NSEC_or_0(file)`.
pub fn apply_symlink_metadata_from_entry(
    destination: &Path,
    entry: &protocol::flist::FileEntry,
    options: &MetadataOptions,
) -> Result<(), MetadataError> {
    let cached_meta = fs::symlink_metadata(destination).ok();

    #[cfg(unix)]
    ownership::apply_symlink_ownership_from_entry(
        destination,
        entry,
        options,
        cached_meta.as_ref(),
    )?;
    #[cfg(not(unix))]
    {
        let _ = entry;
        let _ = options;
        let _ = cached_meta.as_ref();
    }

    if options.times() {
        timestamps::apply_symlink_timestamps_from_entry(
            destination,
            entry,
            options,
            cached_meta.as_ref(),
        )?;
    }

    Ok(())
}

/// Applies metadata from a protocol `FileEntry` to the destination file.
///
/// This is the receiver-side counterpart to [`apply_file_metadata`] that works
/// directly with `FileEntry` metadata from the wire protocol, avoiding the need
/// to construct an [`fs::Metadata`] instance.
///
/// # Examples
///
/// ```no_run
/// use metadata::{apply_metadata_from_file_entry, MetadataOptions};
/// use protocol::flist::FileEntry;
/// use std::path::Path;
///
/// # fn example(file_entry: &FileEntry) -> Result<(), metadata::MetadataError> {
/// let dest_path = Path::new("/path/to/reconstructed/file.txt");
///
/// let options = MetadataOptions::new()
///     .preserve_permissions(true)
///     .preserve_times(true);
///
/// apply_metadata_from_file_entry(dest_path, file_entry, &options)?;
/// # Ok(())
/// # }
/// ```
pub fn apply_metadata_from_file_entry(
    destination: &Path,
    entry: &protocol::flist::FileEntry,
    options: &MetadataOptions,
) -> Result<(), MetadataError> {
    let cached_meta = fs::metadata(destination).ok();
    apply_metadata_with_cached_stat(destination, entry, options, cached_meta)
}

/// Applies metadata using a pre-cached `stat` result.
///
/// Same as [`apply_metadata_from_file_entry`] but avoids an extra `stat`
/// syscall when the caller already has the destination's metadata (e.g.
/// from a quick-check comparison).
pub fn apply_metadata_with_cached_stat(
    destination: &Path,
    entry: &protocol::flist::FileEntry,
    options: &MetadataOptions,
    cached_meta: Option<fs::Metadata>,
) -> Result<(), MetadataError> {
    apply_metadata_with_pre_transfer_stat(destination, entry, options, cached_meta, None)
}

/// Applies metadata using both a cached post-rename stat and a pre-transfer
/// stat.
///
/// Identical to [`apply_metadata_with_cached_stat`] except the additional
/// `pre_transfer_meta` argument lets the receiver mirror upstream
/// `rsync.c:dest_mode()`: when `-p`/`-E`/`--chmod` are all off, upstream
/// still chmods a freshly-renamed temp file back to the pre-transfer
/// destination's permission bits (`exists=true` branch) or to the
/// umask-masked source mode (`exists=false` branch). Without the pre-
/// transfer stat the receiver would feed the temp file's `0o600`/umask-
/// default mode into the heuristic.
pub fn apply_metadata_with_pre_transfer_stat(
    destination: &Path,
    entry: &protocol::flist::FileEntry,
    options: &MetadataOptions,
    cached_meta: Option<fs::Metadata>,
    pre_transfer_meta: Option<fs::Metadata>,
) -> Result<(), MetadataError> {
    apply_metadata_with_attrs_flags_and_pre_transfer(
        destination,
        entry,
        options,
        cached_meta,
        AttrsFlags::empty(),
        pre_transfer_meta,
    )
}

/// Applies metadata from a [`protocol::flist::FileEntry`] with explicit
/// [`AttrsFlags`] controlling which time attributes to skip.
///
/// This is the full-featured variant that mirrors upstream `set_file_attrs()`
/// in `rsync.c`. Callers pass [`AttrsFlags`] to selectively skip mtime, atime,
/// or crtime application.
///
/// # Upstream Reference
///
/// - `rsync.c:574-625` - `set_file_attrs()` uses `flags` parameter to govern
///   which timestamps are applied and whether the comparison is exact.
/// - `rsync.c:585` - `flags |= ATTRS_SKIP_MTIME | ATTRS_SKIP_ATIME | ATTRS_SKIP_CRTIME`
///   when `omit_dir_times` or `omit_link_times` is active.
/// - `generator.c:1814` - Passes `maybe_ATTRS_REPORT | maybe_ATTRS_ACCURATE_TIME`
///   on quick-check match paths.
pub fn apply_metadata_with_attrs_flags(
    destination: &Path,
    entry: &protocol::flist::FileEntry,
    options: &MetadataOptions,
    cached_meta: Option<fs::Metadata>,
    attrs_flags: AttrsFlags,
) -> Result<(), MetadataError> {
    apply_metadata_with_attrs_flags_and_pre_transfer(
        destination,
        entry,
        options,
        cached_meta,
        attrs_flags,
        None,
    )
}

/// Like [`apply_metadata_with_attrs_flags`] but accepts a pre-transfer
/// destination stat so the permission-apply path can reproduce upstream
/// `rsync.c:dest_mode()` for the receiver chmod loop.
///
/// `pre_transfer_meta` is the destination's metadata captured before any
/// temp-file rename. Pass `Some(meta)` when a destination file existed at
/// transfer start; pass `None` when the destination is brand new.
pub fn apply_metadata_with_attrs_flags_and_pre_transfer(
    destination: &Path,
    entry: &protocol::flist::FileEntry,
    options: &MetadataOptions,
    cached_meta: Option<fs::Metadata>,
    attrs_flags: AttrsFlags,
    pre_transfer_meta: Option<fs::Metadata>,
) -> Result<(), MetadataError> {
    let restat_after_chown =
        ownership::apply_ownership_from_entry(destination, entry, options, cached_meta.as_ref())?;

    // upstream: rsync.c:564-567 - the chown may have cleared setuid/setgid bits,
    // so refresh the cached stat before the chmod compare re-applies them.
    let cached_meta = if restat_after_chown {
        fs::metadata(destination).ok().or(cached_meta)
    } else {
        cached_meta
    };

    permissions::apply_permissions_from_entry(
        destination,
        entry,
        options,
        cached_meta.as_ref(),
        pre_transfer_meta.as_ref(),
    )?;

    // upstream: rsync.c:597 - `if (!(flags & ATTRS_SKIP_MTIME) && !same_mtime(...))`
    if options.times() && !attrs_flags.skip_mtime() {
        timestamps::apply_timestamps_from_entry(destination, entry, options, cached_meta.as_ref())?;
    }

    // upstream: rsync.c:604 - atime applied independently when SKIP_MTIME is set
    // but SKIP_ATIME is not, since apply_timestamps_from_entry handles both together
    if options.atimes() && attrs_flags.skip_mtime() && !attrs_flags.skip_atime() {
        timestamps::apply_atime_only_from_entry(
            destination,
            entry,
            cached_meta.as_ref(),
            options.keep_dirlinks(),
        )?;
    }

    // upstream: rsync.c:615 - `if (crtimes_ndx && !(flags & ATTRS_SKIP_CRTIME))`
    if options.crtimes() && entry.crtime() != 0 && !attrs_flags.skip_crtime() {
        timestamps::apply_crtime_from_entry(destination, entry)?;
    }

    Ok(())
}
