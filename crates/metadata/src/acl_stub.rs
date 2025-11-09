#![cfg(all(
    feature = "acl",
    any(
        target_os = "macos",
        target_os = "ios",
        target_os = "tvos",
        target_os = "watchos",
    )
))]

use crate::MetadataError;
use std::path::Path;

/// Stub ACL synchronisation for Apple platforms.
///
/// Apple's libSystem lacks the Linux-specific `acl_from_mode` helper, so the
/// ACL preservation feature is effectively unavailable. The stub mirrors the
/// behaviour of builds compiled without ACL support by performing no work.
pub fn sync_acls(
    source: &Path,
    destination: &Path,
    follow_symlinks: bool,
) -> Result<(), MetadataError> {
    let _ = (source, destination, follow_symlinks);
    Ok(())
}
