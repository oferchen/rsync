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
use std::sync::Once;

/// Emits a one-time warning that ACLs are not supported on this platform.
///
/// This matches upstream rsync behavior of informing users when ACL support
/// is requested but unavailable (options.c:1854).
fn warn_acl_unsupported() {
    static WARN_ONCE: Once = Once::new();
    WARN_ONCE.call_once(|| {
        eprintln!("warning: ACLs are not supported on this platform; skipping ACL preservation");
    });
}

/// Stub ACL synchronisation for Apple platforms.
///
/// Apple's libSystem lacks the Linux-specific `acl_from_mode` helper, so the
/// ACL preservation feature is effectively unavailable. The stub mirrors the
/// behaviour of builds compiled without ACL support by performing no work.
///
/// # Upstream Reference
///
/// - `options.c:1854`: "ACLs are not supported on this %s\n"
pub fn sync_acls(
    source: &Path,
    destination: &Path,
    follow_symlinks: bool,
) -> Result<(), MetadataError> {
    let _ = (source, destination, follow_symlinks);
    warn_acl_unsupported();
    Ok(())
}
