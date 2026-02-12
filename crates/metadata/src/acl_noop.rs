//! Universal no-op ACL stub for platforms without ACL support.
//!
//! This module provides a no-op `sync_acls` for platforms where ACL
//! preservation is not available — either because the `acl` feature
//! is disabled, or because the platform has no ACL implementation
//! (e.g., Windows, Android).

use crate::MetadataError;
use std::path::Path;

/// No-op ACL synchronisation.
///
/// Silently succeeds on platforms without ACL support, matching the
/// pattern established by `acl_stub.rs` for Apple mobile platforms.
pub fn sync_acls(
    _source: &Path,
    _destination: &Path,
    _follow_symlinks: bool,
) -> Result<(), MetadataError> {
    Ok(())
}
