#![cfg(all(
    feature = "acl",
    any(target_os = "ios", target_os = "tvos", target_os = "watchos")
))]

use crate::MetadataError;
use std::path::Path;
use std::sync::Once;

/// Emits a one-time warning that ACLs are not supported on this platform.
///
/// This function uses [`Once`] to ensure the warning is only printed once per
/// process lifetime, regardless of how many times ACL synchronization is attempted.
///
/// # Platform Support
///
/// This warning is emitted on iOS, tvOS, and watchOS platforms which lack
/// full POSIX ACL support in their file systems.
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

/// Stub ACL synchronisation for iOS/tvOS/watchOS platforms.
///
/// These Apple platforms lack full POSIX ACL support. The stub mirrors the
/// behaviour of builds compiled without ACL support by performing no work.
/// macOS has a separate implementation using the `exacl` crate.
///
/// # Arguments
///
/// * `source` - Source path (unused, required for API compatibility)
/// * `destination` - Destination path (unused, required for API compatibility)
/// * `follow_symlinks` - Whether to follow symlinks (unused, required for API compatibility)
///
/// # Returns
///
/// Always returns `Ok(())` after emitting a one-time warning to stderr.
///
/// # Platform Support
///
/// Only compiled on iOS, tvOS, and watchOS targets when the `acl` feature is enabled.
/// Other platforms use platform-specific implementations.
///
/// # Upstream Reference
///
/// - `options.c:1854`: "ACLs are not supported on this %s\n"
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
