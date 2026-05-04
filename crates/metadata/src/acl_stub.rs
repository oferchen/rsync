#![cfg(all(
    feature = "acl",
    any(target_os = "ios", target_os = "tvos", target_os = "watchos")
))]

use crate::MetadataError;
use protocol::acl::{AclCache, RsyncAcl};
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

/// Reads the filesystem ACL for `path` and converts it to an [`RsyncAcl`].
///
/// On iOS/tvOS/watchOS, returns a fake ACL derived from mode.
#[allow(clippy::module_name_repetitions)]
pub fn get_rsync_acl(path: &Path, mode: u32, is_default: bool) -> RsyncAcl {
    let _ = path;
    if is_default {
        RsyncAcl::new()
    } else {
        RsyncAcl::from_mode(mode)
    }
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
    mode: Option<u32>,
) -> Result<(), MetadataError> {
    let _ = (
        destination,
        cache,
        access_ndx,
        default_ndx,
        follow_symlinks,
        mode,
    );
    warn_acl_unsupported();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn sync_acls_returns_ok() {
        let src = Path::new("/nonexistent/src");
        let dst = Path::new("/nonexistent/dst");
        let result = sync_acls(src, dst, false);
        assert!(result.is_ok());
    }

    #[test]
    fn get_rsync_acl_non_default_returns_from_mode() {
        let path = Path::new("/nonexistent/file");
        let acl = get_rsync_acl(path, 0o755, false);
        let from_mode = RsyncAcl::from_mode(0o755);
        assert_eq!(acl, from_mode);
    }

    #[test]
    fn get_rsync_acl_default_returns_empty() {
        let path = Path::new("/nonexistent/file");
        let acl = get_rsync_acl(path, 0o755, true);
        let empty = RsyncAcl::new();
        assert_eq!(acl, empty);
    }

    #[test]
    fn apply_acls_from_cache_returns_ok() {
        let dst = Path::new("/nonexistent/dst");
        let cache = AclCache::new();
        let result = apply_acls_from_cache(dst, &cache, 0, None, false, None);
        assert!(result.is_ok());
    }
}
