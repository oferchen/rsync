//! Filesystem helpers for batch replay: symlink creation and metadata application.
//!
//! Symlink creation has platform-specific handling: Unix uses
//! `std::os::unix::fs::symlink`, Windows uses `symlink_file`, and any other
//! target returns [`BatchError::Unsupported`]. Metadata application delegates
//! to the `metadata` crate so that permissions, timestamps, and ownership are
//! applied with the same semantics as a live transfer.

use std::fs;
use std::path::Path;

use crate::error::{BatchError, BatchResult};
use crate::format::BatchFlags;

/// Apply metadata (permissions, timestamps) from a protocol file entry to a
/// destination path.
///
/// Uses the `metadata` crate's [`metadata::apply_metadata_from_file_entry`]
/// to set permissions and modification times on the target file or directory.
/// Ownership is applied only when the corresponding batch flags are set.
///
/// # Errors
///
/// Returns [`BatchError::Io`] if metadata cannot be applied.
pub(super) fn apply_entry_metadata(
    dest_path: &Path,
    entry: &protocol::flist::FileEntry,
    flags: &BatchFlags,
) -> BatchResult<()> {
    let options = metadata::MetadataOptions::new()
        .preserve_permissions(true)
        .preserve_times(true)
        .preserve_owner(flags.preserve_uid)
        .preserve_group(flags.preserve_gid);

    metadata::apply_metadata_from_file_entry(dest_path, entry, &options).map_err(|e| {
        BatchError::Io(std::io::Error::other(format!(
            "failed to apply metadata to '{}': {e}",
            dest_path.display()
        )))
    })?;

    Ok(())
}

/// Apply metadata (timestamps, optionally ownership) from a symlink file entry
/// to a destination symbolic link without following the link target.
///
/// Mirrors upstream `rsync.c:set_file_attrs()` symlink handling: a symlink's
/// own mtime and ownership are updated via `lutimes` / `AT_SYMLINK_NOFOLLOW`
/// chown, but `chmod` is skipped because most platforms ignore the mode bits
/// on a symlink and the call would otherwise follow the link and clobber the
/// target file's permissions.
///
/// This is required during batch replay because phase 1 creates symlinks
/// before phase 2 writes their target files. Calling [`apply_entry_metadata`]
/// on a symlink whose target was just materialised would `chmod` the
/// underlying regular file with the symlink's mode (typically `0777` on
/// macOS), silently overwriting the correct permissions applied by the
/// per-file metadata pass.
///
/// # Errors
///
/// Returns [`BatchError::Io`] if symlink metadata cannot be applied.
pub(super) fn apply_symlink_entry_metadata(
    dest_path: &Path,
    entry: &protocol::flist::FileEntry,
    flags: &BatchFlags,
) -> BatchResult<()> {
    let options = metadata::MetadataOptions::new()
        .preserve_permissions(true)
        .preserve_times(true)
        .preserve_owner(flags.preserve_uid)
        .preserve_group(flags.preserve_gid);

    metadata::apply_symlink_metadata_from_entry(dest_path, entry, &options).map_err(|e| {
        BatchError::Io(std::io::Error::other(format!(
            "failed to apply symlink metadata to '{}': {e}",
            dest_path.display()
        )))
    })?;

    Ok(())
}

/// Create a symlink at `dest_path` pointing to the given `target`.
///
/// On Unix, creates a symbolic link. On other platforms, falls back to
/// file copy (symlink creation is platform-specific).
#[cfg(unix)]
pub(super) fn create_symlink(target: &Path, dest_path: &Path) -> BatchResult<()> {
    // Remove existing entry if present, to mirror upstream rsync behavior
    if dest_path.exists() || dest_path.symlink_metadata().is_ok() {
        let _ = fs::remove_file(dest_path);
    }
    std::os::unix::fs::symlink(target, dest_path).map_err(|e| {
        BatchError::Io(std::io::Error::new(
            e.kind(),
            format!(
                "failed to create symlink '{}' -> '{}': {e}",
                dest_path.display(),
                target.display()
            ),
        ))
    })
}

/// Create a symlink on Windows (best-effort directory detection).
#[cfg(not(unix))]
pub(super) fn create_symlink(target: &Path, dest_path: &Path) -> BatchResult<()> {
    if dest_path.exists() || dest_path.symlink_metadata().is_ok() {
        let _ = fs::remove_file(dest_path);
    }
    // Windows requires knowing whether the target is a file or directory.
    // Default to file symlink; directory symlinks are rare in rsync batch use.
    #[cfg(windows)]
    {
        std::os::windows::fs::symlink_file(target, dest_path).map_err(|e| {
            BatchError::Io(std::io::Error::new(
                e.kind(),
                format!(
                    "failed to create symlink '{}' -> '{}': {e}",
                    dest_path.display(),
                    target.display()
                ),
            ))
        })
    }
    #[cfg(not(windows))]
    {
        let _ = (target, dest_path);
        Err(BatchError::Unsupported(
            "symlink creation not supported on this platform".to_owned(),
        ))
    }
}
