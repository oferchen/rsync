#![cfg(all(
    feature = "acl",
    any(target_os = "ios", target_os = "tvos", target_os = "watchos")
))]

use crate::MetadataError;
use protocol::acl::AclCache;
use std::path::Path;
use std::sync::Once;

/// Emits a one-time warning that ACLs are not supported on this platform.
///
/// Uses [`Once`] so the message appears at most once per process lifetime.
/// Mirrors `options.c:1854` upstream behaviour.
fn warn_acl_unsupported() {
    static WARN_ONCE: Once = Once::new();
    WARN_ONCE.call_once(|| {
        eprintln!("warning: ACLs are not supported on this platform; skipping ACL preservation");
    });
}

/// Stub ACL synchronisation for iOS/tvOS/watchOS.
///
/// These platforms lack full POSIX ACL support. Emits a one-time warning and
/// returns `Ok(())`. macOS uses a separate implementation via the `exacl` crate.
#[allow(clippy::module_name_repetitions)]
pub fn sync_acls(
    source: &Path,
    destination: &Path,
    follow_symlinks: bool,
) -> Result<(), MetadataError> {
    let _ = (source, destination, follow_symlinks);
    warn_acl_unsupported();
    Ok(())
}

/// Applies parsed ACLs from an [`AclCache`] to a destination file.
///
/// On iOS/tvOS/watchOS platforms without ACL support, emits a one-time
/// warning and returns `Ok(())`.
#[allow(clippy::module_name_repetitions)]
pub fn apply_acls_from_cache(
    destination: &Path,
    cache: &AclCache,
    access_ndx: u32,
    default_ndx: Option<u32>,
    follow_symlinks: bool,
) -> Result<(), MetadataError> {
    let _ = (destination, cache, access_ndx, default_ndx, follow_symlinks);
    warn_acl_unsupported();
    Ok(())
}
