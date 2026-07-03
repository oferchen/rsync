//! Helpers for synchronizing extended attributes and ACLs.

use super::LocalCopyError;
use ::metadata::MetadataError;

#[cfg(any(
    all(unix, any(feature = "acl", feature = "xattr")),
    all(windows, feature = "acl")
))]
use std::path::Path;

#[cfg(any(
    all(unix, any(feature = "acl", feature = "xattr")),
    all(windows, feature = "acl")
))]
use super::LocalCopyExecution;

#[cfg(all(unix, feature = "xattr"))]
use super::FilterProgram;

#[cfg(all(any(unix, windows), feature = "acl"))]
use ::metadata::sync_acls;

#[cfg(all(unix, feature = "xattr"))]
use ::metadata::sync_xattrs;

#[cfg(all(unix, feature = "xattr"))]
use ::metadata::nfsv4_acl::sync_nfsv4_acls;

/// Synchronizes extended attributes from source to destination if requested.
///
/// If a filter program with xattr rules is provided, only attributes
/// that pass the filter are synchronized. Unsupported-filesystem errors
/// are silently ignored.
///
/// # Errors
///
/// Returns [`LocalCopyError`] if xattr synchronization fails.
#[cfg(all(unix, feature = "xattr"))]
pub(crate) fn sync_xattrs_if_requested(
    preserve_xattrs: bool,
    mode: LocalCopyExecution,
    source: &Path,
    destination: &Path,
    follow_symlinks: bool,
    filter_program: Option<&FilterProgram>,
) -> Result<(), LocalCopyError> {
    if preserve_xattrs && !mode.is_dry_run() {
        if let Some(program) = filter_program {
            if program.has_xattr_rules() {
                let filter = |name: &str| program.allows_xattr(name);
                sync_xattrs(source, destination, follow_symlinks, Some(&filter))
                    .map_err(map_metadata_error)?;
            } else {
                sync_xattrs(source, destination, follow_symlinks, None)
                    .map_err(map_metadata_error)?;
            }
        } else {
            sync_xattrs(source, destination, follow_symlinks, None).map_err(map_metadata_error)?;
        }
    }
    Ok(())
}

/// Synchronizes POSIX/extended ACLs from source to destination if requested.
///
/// No-op when `preserve_acls` is false or in dry-run mode.
/// Unsupported-filesystem errors are silently ignored.
///
/// # Errors
///
/// Returns [`LocalCopyError`] if ACL synchronization fails.
#[cfg(all(any(unix, windows), feature = "acl"))]
pub(crate) fn sync_acls_if_requested(
    preserve_acls: bool,
    mode: LocalCopyExecution,
    source: &Path,
    destination: &Path,
    follow_symlinks: bool,
) -> Result<(), LocalCopyError> {
    if preserve_acls && !mode.is_dry_run() {
        sync_acls(source, destination, follow_symlinks).map_err(map_metadata_error)?;
    }
    Ok(())
}

/// Synchronize NFSv4 ACLs from source to destination if preservation is requested.
///
/// NFSv4 ACLs are stored in the `system.nfs4_acl` extended attribute and use
/// a different permission model than POSIX ACLs (ACE-based with inheritance).
/// This function copies the NFSv4 ACL from source to destination when:
/// - `preserve_nfsv4_acls` is true
/// - The operation is not a dry run
/// - The source has an NFSv4 ACL
#[cfg(all(unix, feature = "xattr"))]
pub(crate) fn sync_nfsv4_acls_if_requested(
    preserve_nfsv4_acls: bool,
    mode: LocalCopyExecution,
    source: &Path,
    destination: &Path,
    follow_symlinks: bool,
) -> Result<(), LocalCopyError> {
    if preserve_nfsv4_acls && !mode.is_dry_run() {
        sync_nfsv4_acls(source, destination, follow_symlinks).map_err(map_metadata_error)?;
    }
    Ok(())
}

/// Stores the effective fake-super `user.rsync.%stat` xattr on the destination.
///
/// Under `--fake-super` the source may be a placeholder whose real
/// mode/uid/gid/rdev live in its own `user.rsync.%stat` xattr (written by an
/// earlier fake-super receive). The metadata-apply step only saw the
/// placeholder's raw `fs::Metadata`, so it stored the wrong ownership. Rewrite
/// the destination xattr from [`::metadata::effective_source_stat`], which
/// prefers the source's recorded stat and falls back to its `fs::Metadata`.
///
/// No-op unless `--fake-super` is active together with ownership preservation,
/// matching `metadata::apply::ownership::set_owner_like`'s fake-super gate.
// upstream: xattrs.c:set_stat_xattr() driven by x_lstat()/get_stat_xattr()
#[cfg(all(unix, feature = "xattr"))]
pub(crate) fn store_effective_fake_super_if_requested(
    options: &::metadata::MetadataOptions,
    source: &Path,
    destination: &Path,
    metadata: &std::fs::Metadata,
) -> Result<(), LocalCopyError> {
    let ownership_requested = options.owner()
        || options.group()
        || options.owner_override().is_some()
        || options.group_override().is_some();
    if !options.fake_super_enabled() || !ownership_requested {
        return Ok(());
    }

    // Only forward a placeholder's recorded stat. When the source carries its
    // own `user.rsync.%stat` (an earlier fake-super receive), it - not the
    // placeholder's raw perms - is the source of truth for uid/gid/mode/rdev.
    // For a real-file source, the ownership + permission apply steps already
    // wrote or removed the destination `%stat` following upstream's
    // set_stat_xattr write-or-remove rule, so re-storing here would resurrect a
    // shim upstream deliberately dropped for a faithful same-owner copy.
    // upstream: xattrs.c:get_stat_xattr() consumed via x_lstat().
    let Ok(Some(mut stat)) = ::metadata::load_fake_super(source) else {
        return Ok(());
    };

    // A `--chmod` tweak makes the destination's deflected mode (written by the
    // permission step) authoritative; keep it rather than the placeholder's.
    if let Ok(Some(existing)) = ::metadata::load_fake_super(destination) {
        if options.chmod().is_some() {
            stat.mode = existing.mode;
        }
        if existing == stat {
            return Ok(());
        }
    }

    let _ = metadata;
    ::metadata::store_fake_super(destination, &stat).map_err(|error| {
        LocalCopyError::io(
            "store fake-super metadata",
            destination.to_path_buf(),
            error,
        )
    })
}

/// Converts a [`MetadataError`] into a [`LocalCopyError`].
pub(crate) fn map_metadata_error(error: MetadataError) -> LocalCopyError {
    let (context, path, source) = error.into_parts();
    LocalCopyError::io(context, path, source)
}
