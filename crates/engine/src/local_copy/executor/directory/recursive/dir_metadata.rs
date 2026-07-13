//! Final directory metadata application and completion recording.
//!
//! Applies ownership, permissions, timestamps, ACLs, and extended attributes
//! to directories after all their contents have been transferred.

// upstream: receiver.c - directory metadata finalization after recv_files()

use std::fs;
#[cfg(unix)]
use std::io;
use std::path::{Path, PathBuf};

#[cfg(any(
    all(unix, any(feature = "acl", feature = "xattr")),
    all(windows, feature = "acl")
))]
use crate::local_copy::LocalCopyExecution;
#[cfg(all(any(unix, windows), feature = "acl"))]
use crate::local_copy::sync_acls_if_requested;
#[cfg(all(unix, feature = "xattr"))]
use crate::local_copy::sync_xattrs_if_requested;
use crate::local_copy::{CopyContext, LocalCopyError, LocalCopyRecord, map_metadata_error};
use ::metadata::apply_directory_metadata_with_options;

/// Applies final metadata to a directory after all contents have been processed.
///
/// This includes permissions, timestamps (unless omit_dir_times is enabled),
/// extended attributes, and ACLs. When `relative` covers more than one
/// component, propagates the source's directory mtime onto each intermediate
/// component materialized by `--relative` so they do not carry wall-clock
/// timestamps from `create_dir_all`.
pub(super) fn apply_final_directory_metadata(
    context: &mut CopyContext,
    source: &Path,
    destination: &Path,
    metadata: &fs::Metadata,
    relative: Option<&Path>,
    #[cfg(any(
        all(unix, any(feature = "acl", feature = "xattr")),
        all(windows, feature = "acl")
    ))]
    mode: LocalCopyExecution,
    #[cfg(all(unix, feature = "xattr"))] preserve_xattrs: bool,
    #[cfg(all(any(unix, windows), feature = "acl"))] preserve_acls: bool,
) -> Result<(), LocalCopyError> {
    let metadata_options = if context.omit_dir_times_enabled() {
        context.metadata_options().preserve_times(false)
    } else {
        context.metadata_options()
    };
    apply_directory_metadata_with_options(destination, metadata, metadata_options.clone())
        .map_err(map_metadata_error)?;

    // Record the directory and its source mtime for the final touch-up pass.
    // Late in-directory mutations (delayed-update renames, deletions, backups)
    // bump this directory's mtime after we set it here, so a single final pass
    // re-applies the recorded source mtime once everything else is done.
    // upstream: generator.c:2093 touch_up_dirs() re-touches directory mtimes
    // after the delayed-update phase completes.
    if metadata_options.times() {
        context.record_finalized_directory(destination, metadata);
    }

    // upstream: generator.c:1410 - implied parent dirs are finalized via
    // set_file_attrs() when --implied-dirs is active (the default). With
    // --no-implied-dirs upstream skips them via FLAG_IMPLIED_DIR.
    if let Some(rel) = relative
        && context.implied_dirs_enabled()
    {
        apply_relative_intermediate_dir_mtimes(source, destination, rel, &metadata_options)?;
    }

    #[cfg(all(unix, feature = "xattr"))]
    sync_xattrs_if_requested(
        preserve_xattrs,
        mode,
        source,
        destination,
        true,
        context.filter_program(),
    )?;

    #[cfg(all(any(unix, windows), feature = "acl"))]
    sync_acls_if_requested(preserve_acls, mode, source, destination, true)?;

    // Suppress unused variable warnings when features are disabled
    let _ = source;

    Ok(())
}

/// Reproduces upstream's transfer-root self-lock when a `--chmod` strips the
/// root directory's owner-execute bit.
///
/// upstream: generator.c:1503-1520 - the generator chmods a directory to its
/// tweaked mode and then re-adds owner-`rwx` (`do_chmod_at(fname, mode | S_IRWXU)`)
/// so it can write the directory's contents. The transfer root is addressed as
/// `dst/.`, so that re-add chmod must resolve `.` *inside* `dst`; a tweak that
/// removed owner-execute makes it fail with `EACCES` (generator.c:1514 "failed
/// to modify permissions on %s") and the generator can no longer stat or create
/// the root's contents. Nothing under it transfers and rsync exits 23.
/// Non-root directories are addressed by name and never take this path, so the
/// caller scopes the check to the transfer root (`relative == None`).
///
/// Returns `Ok(Some(error))` when the root self-locks - after leaving it at the
/// strict tweaked mode and recording an I/O error (exit 23) - so the caller
/// skips the root's contents and its metadata finalization. Returns `Ok(None)`
/// when no `--chmod` is active, the tweak keeps owner execute, or the root has
/// not been materialised yet.
#[cfg(unix)]
pub(super) fn enforce_transfer_root_self_lock(
    context: &mut CopyContext,
    destination: &Path,
    metadata: &fs::Metadata,
) -> Result<Option<LocalCopyError>, LocalCopyError> {
    use std::os::unix::fs::PermissionsExt;

    let existing = fs::symlink_metadata(destination).ok();
    let Some(existing_meta) = existing.as_ref().filter(|meta| meta.is_dir()) else {
        return Ok(None);
    };

    let options = context.metadata_options();
    let Some((tweaked, self_locks)) = ::metadata::transfer_root_chmod_self_lock(
        destination,
        metadata,
        &options,
        Some(existing_meta),
    )
    .map_err(map_metadata_error)?
    else {
        return Ok(None);
    };
    if !self_locks {
        return Ok(None);
    }

    // Apply the strict tweaked mode to the root, self-locking it exactly as
    // upstream's set_file_attrs() does before it fails to re-add owner-rwx.
    fs::set_permissions(destination, fs::Permissions::from_mode(tweaked))
        .map_err(|error| LocalCopyError::io("modify permissions on", destination, error))?;

    // upstream: generator.c:1514 do_chmod_at("dst/.", mode | S_IRWXU) - fails
    // with EACCES because `.` can no longer be resolved inside the now
    // owner-non-executable root. Trigger the identical OS error to report it.
    let dot = destination.join(".");
    let io_error = match fs::set_permissions(&dot, fs::Permissions::from_mode(tweaked | 0o700)) {
        Err(error) => error,
        Ok(()) => io::Error::new(io::ErrorKind::PermissionDenied, "Permission denied"),
    };
    context.record_io_error();
    Ok(Some(LocalCopyError::io(
        "modify permissions on",
        dot,
        io_error,
    )))
}

/// Non-Unix stub: permission-driven self-lock does not apply without POSIX
/// directory-execute traversal semantics.
#[cfg(not(unix))]
pub(super) fn enforce_transfer_root_self_lock(
    _context: &mut CopyContext,
    _destination: &Path,
    _metadata: &fs::Metadata,
) -> Result<Option<LocalCopyError>, LocalCopyError> {
    Ok(None)
}

/// Records directory completion statistics and pending records.
#[inline]
pub(super) fn record_directory_completion(
    context: &mut CopyContext,
    creation_record_pending: bool,
    pending_record: Option<LocalCopyRecord>,
) {
    context.summary_mut().record_directory_total();
    if creation_record_pending {
        context.summary_mut().record_directory();
    }
    if let Some(record) = pending_record {
        context.record(record);
    }
}

/// Propagates source mtime/permissions onto each intermediate directory
/// materialized along the `--relative` chain.
///
/// Upstream rsync's `generator.c::make_path()` walks the same chain and each
/// implied parent is finalized by `recv_generator()` with the source dir's
/// metadata. Our local-copy executor materializes the chain via
/// `prepare_parent_directory` + `create_dir_all`, which leaves intermediate
/// components stamped with the current wall-clock time and trips the
/// `relative` testsuite check.
///
/// For `relative = down/3/deep` we replay every ancestor (`down`, `down/3`)
/// against its source counterpart and apply the same directory metadata
/// options used for the leaf. The leaf itself is handled by the caller and
/// is skipped here.
fn apply_relative_intermediate_dir_mtimes(
    source: &Path,
    destination: &Path,
    relative: &Path,
    metadata_options: &::metadata::MetadataOptions,
) -> Result<(), LocalCopyError> {
    let Some(source_root) = strip_relative_suffix(source, relative) else {
        return Ok(());
    };
    let Some(destination_root) = strip_relative_suffix(destination, relative) else {
        return Ok(());
    };

    let components: Vec<&std::ffi::OsStr> = relative.iter().collect();
    if components.len() <= 1 {
        return Ok(());
    }

    let mut accumulated = PathBuf::new();
    for component in &components[..components.len() - 1] {
        accumulated.push(component);
        let src_dir = source_root.join(&accumulated);
        let dst_dir = destination_root.join(&accumulated);

        let src_meta = match fs::symlink_metadata(&src_dir) {
            Ok(meta) if meta.file_type().is_dir() => meta,
            _ => continue,
        };

        if !dst_dir.is_dir() {
            continue;
        }

        apply_directory_metadata_with_options(&dst_dir, &src_meta, metadata_options.clone())
            .map_err(map_metadata_error)?;
    }

    Ok(())
}

/// Strips `relative` from the trailing path components of `path`, returning
/// the prefix root. Mirrors how the executor joins `<root>/<relative>` to
/// derive per-source destinations.
fn strip_relative_suffix(path: &Path, relative: &Path) -> Option<PathBuf> {
    let path_components: Vec<_> = path.components().collect();
    let rel_components: Vec<_> = relative.components().collect();
    if rel_components.len() > path_components.len() {
        return None;
    }
    let split = path_components.len() - rel_components.len();
    for (idx, rel) in rel_components.iter().enumerate() {
        if path_components[split + idx].as_os_str() != rel.as_os_str() {
            return None;
        }
    }
    let mut root = PathBuf::new();
    for component in &path_components[..split] {
        root.push(component.as_os_str());
    }
    Some(root)
}

#[cfg(test)]
mod tests {
    use super::strip_relative_suffix;
    use std::path::{Path, PathBuf};

    #[test]
    fn strip_relative_suffix_drops_matching_tail() {
        let path = PathBuf::from("/dst/down/3/deep");
        let relative = Path::new("down/3/deep");
        assert_eq!(
            strip_relative_suffix(&path, relative),
            Some(PathBuf::from("/dst")),
        );
    }

    #[test]
    fn strip_relative_suffix_returns_none_on_mismatch() {
        let path = PathBuf::from("/dst/other/3/deep");
        let relative = Path::new("down/3/deep");
        assert_eq!(strip_relative_suffix(&path, relative), None);
    }

    #[test]
    fn strip_relative_suffix_returns_none_when_relative_longer() {
        let path = PathBuf::from("/dst/deep");
        let relative = Path::new("down/3/deep");
        assert_eq!(strip_relative_suffix(&path, relative), None);
    }
}
