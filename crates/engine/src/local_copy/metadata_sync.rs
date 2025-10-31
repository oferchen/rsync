//! Helpers for synchronizing extended attributes and ACLs.

#[cfg(any(feature = "acl", feature = "xattr"))]
use std::path::Path;

use rsync_meta::MetadataError;

use super::LocalCopyError;

#[cfg(any(feature = "acl", feature = "xattr"))]
use super::LocalCopyExecution;

#[cfg(feature = "acl")]
use rsync_meta::sync_acls;
#[cfg(feature = "xattr")]
use rsync_meta::sync_xattrs;

#[cfg(feature = "xattr")]
pub(crate) fn sync_xattrs_if_requested(
    preserve_xattrs: bool,
    mode: LocalCopyExecution,
    source: &Path,
    destination: &Path,
    follow_symlinks: bool,
) -> Result<(), LocalCopyError> {
    if preserve_xattrs && !mode.is_dry_run() {
        sync_xattrs(source, destination, follow_symlinks).map_err(map_metadata_error)?;
    }
    Ok(())
}

#[cfg(feature = "acl")]
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
