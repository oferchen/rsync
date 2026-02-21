//! Universal no-op ACL stub for platforms without ACL support.
//!
//! This module provides a no-op `sync_acls` for platforms where ACL
//! preservation is not available — either because the `acl` feature
//! is disabled, or because the platform has no ACL implementation
//! (e.g., Windows, Android).

use crate::MetadataError;
use std::path::Path;
use std::sync::Once;

/// Emits a one-time warning that ACLs are not supported on this platform.
///
/// # Upstream Reference
///
/// Matches upstream rsync behavior of informing users when ACL support
/// is requested but unavailable (options.c:1854).
fn warn_acl_unsupported() {
    static WARN_ONCE: Once = Once::new();
    WARN_ONCE.call_once(|| {
        eprintln!("warning: ACLs are not supported on this platform; skipping ACL preservation");
    });
}

/// No-op ACL synchronisation with a one-time warning.
///
/// Emits a warning on first call, then returns `Ok(())` for all
/// subsequent calls. Matches upstream rsync behavior of informing
/// the user when ACL preservation is requested but unavailable.
pub fn sync_acls(
    _source: &Path,
    _destination: &Path,
    _follow_symlinks: bool,
) -> Result<(), MetadataError> {
    warn_acl_unsupported();
    Ok(())
}
