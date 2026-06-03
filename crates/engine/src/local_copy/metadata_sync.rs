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

/// Converts a [`MetadataError`] into a [`LocalCopyError`].
pub(crate) fn map_metadata_error(error: MetadataError) -> LocalCopyError {
    let (context, path, source) = error.into_parts();
    LocalCopyError::io(context, path, source)
}
