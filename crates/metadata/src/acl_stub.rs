#![cfg(all(
    feature = "acl",
    any(target_os = "ios", target_os = "tvos", target_os = "watchos")
))]

use crate::AclIdMapper;
use crate::MetadataError;
use protocol::acl::{AclCache, RsyncAcl};
use std::path::Path;
use std::sync::Once;

/// Emits a one-time warning that ACLs are not supported on iOS/tvOS/watchOS.
///
/// upstream: `options.c:1854` - "ACLs are not supported on this %s\n".
fn warn_acl_unsupported() {
    static WARN_ONCE: Once = Once::new();
    WARN_ONCE.call_once(|| {
        eprintln!("warning: ACLs are not supported on this platform; skipping ACL preservation");
    });
}

/// Stub ACL synchronisation for iOS/tvOS/watchOS platforms.
///
/// These Apple platforms lack full POSIX ACL support. The stub mirrors the
/// behaviour of builds compiled without ACL support by performing no work
/// and emitting a one-time warning to stderr. macOS has a separate
/// implementation using the `exacl` crate.
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

/// Synthesises an [`RsyncAcl`] from `mode`, since iOS/tvOS/watchOS cannot read a
/// filesystem ACL.
///
/// A default-ACL request (`is_default`) yields an empty ACL; otherwise the ACL
/// is derived from the mode bits.
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
    id_map: Option<&AclIdMapper>,
) -> Result<(), MetadataError> {
    let _ = (
        destination,
        cache,
        access_ndx,
        default_ndx,
        follow_symlinks,
        mode,
        id_map,
    );
    warn_acl_unsupported();
    Ok(())
}

/// Returns the umask-derived default permissions for `dir`.
///
/// iOS/tvOS/watchOS lack POSIX default-ACL support, so this stub returns
/// `ACCESSPERMS & ~umask` without emitting `--debug=ACL` output. Mirrors
/// upstream's `#ifdef SUPPORT_ACLS` guard at `generator.c:1337-1340`.
#[allow(clippy::module_name_repetitions)]
#[must_use]
pub fn default_perms_for_dir(dir: &Path, orig_umask: u32) -> u32 {
    let _ = dir;
    0o777u32 & !(orig_umask & 0o777)
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
        let result = apply_acls_from_cache(dst, &cache, 0, None, false, None, None);
        assert!(result.is_ok());
    }
}
