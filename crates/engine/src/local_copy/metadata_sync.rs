//! Helpers for synchronizing extended attributes and ACLs.

use ::metadata::MetadataError;
use super::LocalCopyError;

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

pub(crate) fn map_metadata_error(error: MetadataError) -> LocalCopyError {
    let (context, path, source) = error.into_parts();
    LocalCopyError::io(context, path, source)
}
