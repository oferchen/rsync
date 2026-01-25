//! Helpers for synchronizing extended attributes and ACLs.

use super::LocalCopyError;
use ::metadata::MetadataError;

#[cfg(all(unix, any(feature = "acl", feature = "xattr")))]
use std::path::Path;

#[cfg(all(unix, any(feature = "acl", feature = "xattr")))]
use super::LocalCopyExecution;

#[cfg(all(unix, feature = "xattr"))]
use super::FilterProgram;

#[cfg(all(unix, feature = "acl"))]
use ::metadata::sync_acls;

#[cfg(all(unix, feature = "xattr"))]
use ::metadata::sync_xattrs;

#[cfg(all(unix, feature = "xattr"))]
use ::metadata::nfsv4_acl::sync_nfsv4_acls;

/// Synchronizes extended attributes from source to destination if requested.
///
/// This is a conditional wrapper around [`sync_xattrs`] that applies filtering
/// rules and only performs synchronization when appropriate.
///
/// # Arguments
///
/// * `preserve_xattrs` - Whether to preserve extended attributes
/// * `mode` - Execution mode (dry run vs actual execution)
/// * `source` - Source file path
/// * `destination` - Destination file path
/// * `follow_symlinks` - Whether to follow symlinks
/// * `filter_program` - Optional filter program for xattr filtering
///
/// # Filtering
///
/// If a filter program is provided and has xattr rules, only attributes
/// that pass the filter are synchronized. Otherwise, all xattrs are copied.
///
/// # Errors
///
/// Returns [`LocalCopyError`] if xattr synchronization fails, except for
/// unsupported filesystem errors which are silently ignored.
///
/// # Platform Support
///
/// Only available on Unix platforms with the `xattr` feature enabled.
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
/// This is a conditional wrapper around [`sync_acls`] that only performs ACL
/// synchronization when:
/// - `preserve_acls` is `true`
/// - The operation is not a dry run
///
/// # Arguments
///
/// * `preserve_acls` - Whether to preserve ACLs (controlled by user options)
/// * `mode` - Execution mode (dry run vs actual execution)
/// * `source` - Source file path
/// * `destination` - Destination file path
/// * `follow_symlinks` - Whether to follow symlinks (symbolic links don't support ACLs)
///
/// # Errors
///
/// Returns [`LocalCopyError`] if ACL synchronization fails, except for
/// unsupported filesystem errors which are silently ignored.
///
/// # Platform Support
///
/// Only available on Unix platforms with the `acl` feature enabled.
/// Supports:
/// - **Linux**: POSIX ACLs (access and default)
/// - **macOS**: Extended ACLs (NFSv4-style)
/// - **FreeBSD**: POSIX and NFSv4 ACLs
#[cfg(all(unix, feature = "acl"))]
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
///
/// This helper extracts the error components (context, path, source) from
/// a metadata operation error and wraps them in a local copy error.
///
/// # Arguments
///
/// * `error` - The metadata error to convert
///
/// # Returns
///
/// A [`LocalCopyError`] containing the same error information in a format
/// appropriate for local copy operations.
pub(crate) fn map_metadata_error(error: MetadataError) -> LocalCopyError {
    let (context, path, source) = error.into_parts();
    LocalCopyError::io(context, path, source)
}
