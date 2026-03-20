// Patch note (oc-rsync):
// - Removed the #[cfg(not(unix))] variant of `base_mode_for_permissions`,
//   which was never called on non-Unix targets and triggered a dead_code
//   error when building for Windows with `-D warnings`.
//   The function is only needed on Unix and is only referenced inside a
//   #[cfg(unix)] block, so restricting it to Unix preserves behavior and
//   keeps non-Unix builds clean.

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
/// nanosecond timestamps.
pub fn apply_directory_metadata(
    destination: &Path,
    metadata: &fs::Metadata,
) -> Result<(), MetadataError> {
    apply_directory_metadata_with_options(destination, metadata, MetadataOptions::default())
}

/// Applies metadata from `metadata` to the destination directory using explicit options.
pub fn apply_directory_metadata_with_options(
    destination: &Path,
    metadata: &fs::Metadata,
    options: MetadataOptions,
) -> Result<(), MetadataError> {
    ownership::set_owner_like(metadata, destination, true, &options, None)?;
    permissions::apply_permissions_with_chmod(destination, metadata, &options, None)?;
    if options.times() {
        timestamps::set_timestamp_like(metadata, destination, true, None)?;
    }
    Ok(())
}

/// Applies metadata from `metadata` to the destination file.
///
/// Preserves permission bits (best-effort on non-Unix targets) and
/// nanosecond timestamps.
pub fn apply_file_metadata(
    destination: &Path,
    metadata: &fs::Metadata,
) -> Result<(), MetadataError> {
    apply_file_metadata_with_options(destination, metadata, &MetadataOptions::default())
}

/// Applies file metadata using explicit [`MetadataOptions`].
pub fn apply_file_metadata_with_options(
    destination: &Path,
    metadata: &fs::Metadata,
    options: &MetadataOptions,
) -> Result<(), MetadataError> {
    ownership::set_owner_like(metadata, destination, true, options, None)?;
    permissions::apply_permissions_with_chmod(destination, metadata, options, None)?;
    if options.times() {
        timestamps::set_timestamp_like(metadata, destination, true, None)?;
    }
    if options.crtimes() {
        timestamps::apply_crtime_from_source_metadata(destination, metadata)?;
    }
    Ok(())
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
    if options.times() {
        timestamps::set_timestamp_with_fd(metadata, destination, fd, None)?;
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
    ownership::set_owner_like(metadata, destination, true, options, Some(existing))?;
    permissions::apply_permissions_with_chmod(destination, metadata, options, Some(existing))?;
    if options.times() {
        timestamps::set_timestamp_like(metadata, destination, true, Some(existing))?;
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
    ownership::set_owner_like_with_fd(metadata, destination, options, fd, Some(existing))?;
    permissions::apply_permissions_with_chmod_fd(
        destination,
        metadata,
        options,
        Some(fd),
        Some(existing),
    )?;
    if options.times() {
        timestamps::set_timestamp_with_fd(metadata, destination, fd, Some(existing))?;
    }
    // crtime is always path-based (setattrlist on macOS) - no fd variant exists
    if options.crtimes() {
        timestamps::apply_crtime_from_source_metadata(destination, metadata)?;
    }
    Ok(())
}

/// Applies metadata from `metadata` to the destination symbolic link without
/// following the link target.
pub fn apply_symlink_metadata(
    destination: &Path,
    metadata: &fs::Metadata,
) -> Result<(), MetadataError> {
    apply_symlink_metadata_with_options(destination, metadata, &MetadataOptions::default())
}

/// Applies symbolic link metadata using explicit [`MetadataOptions`].
pub fn apply_symlink_metadata_with_options(
    destination: &Path,
    metadata: &fs::Metadata,
    options: &MetadataOptions,
) -> Result<(), MetadataError> {
    ownership::set_owner_like(metadata, destination, false, options, None)?;
    if options.times() {
        timestamps::set_timestamp_like(metadata, destination, false, None)?;
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
    apply_metadata_with_attrs_flags(
        destination,
        entry,
        options,
        cached_meta,
        AttrsFlags::empty(),
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
    ownership::apply_ownership_from_entry(destination, entry, options, cached_meta.as_ref())?;

    permissions::apply_permissions_from_entry(destination, entry, options, cached_meta.as_ref())?;

    // upstream: rsync.c:597 - `if (!(flags & ATTRS_SKIP_MTIME) && !same_mtime(...))`
    if options.times() && !attrs_flags.skip_mtime() {
        timestamps::apply_timestamps_from_entry(destination, entry, options, cached_meta.as_ref())?;
    }

    // When both times() and atimes() are set and SKIP_MTIME is active but not SKIP_ATIME,
    // we still need to apply atime. The `apply_timestamps_from_entry` handles both mtime
    // and atime together, so when SKIP_MTIME is set but atimes should still be preserved,
    // we apply atime-only here.
    // upstream: rsync.c:604 - `if (!(flags & ATTRS_SKIP_ATIME))`
    if options.atimes() && attrs_flags.skip_mtime() && !attrs_flags.skip_atime() {
        timestamps::apply_atime_only_from_entry(destination, entry, cached_meta.as_ref())?;
    }

    // upstream: rsync.c:615 - `if (crtimes_ndx && !(flags & ATTRS_SKIP_CRTIME))`
    if options.crtimes() && entry.crtime() != 0 && !attrs_flags.skip_crtime() {
        timestamps::apply_crtime_from_entry(destination, entry)?;
    }

    Ok(())
}
