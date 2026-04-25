//! Universal no-op ACL stub for platforms without ACL support.
//!
//! This module provides no-op `sync_acls` and `apply_acls_from_cache` for
//! platforms where ACL preservation is not available - either because the
//! `acl` feature is disabled, or because the platform has no ACL
//! implementation (e.g., Windows, Android).

use crate::MetadataError;
use protocol::acl::{AclCache, RsyncAcl};
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

/// Reads the filesystem ACL for `path` and converts it to an [`RsyncAcl`].
///
/// On platforms without ACL support, returns a fake ACL derived from mode.
pub fn get_rsync_acl(_path: &Path, mode: u32, is_default: bool) -> RsyncAcl {
    if is_default {
        RsyncAcl::new()
    } else {
        RsyncAcl::from_mode(mode)
    }
}

/// Applies parsed ACLs from an [`AclCache`] to a destination file.
///
/// On platforms without ACL support, emits a one-time warning and
/// returns `Ok(())`.
#[allow(clippy::module_name_repetitions)]
pub fn apply_acls_from_cache(
    _destination: &Path,
    _cache: &AclCache,
    _access_ndx: u32,
    _default_ndx: Option<u32>,
    _follow_symlinks: bool,
    _mode: Option<u32>,
) -> Result<(), MetadataError> {
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
    fn sync_acls_follow_symlinks_returns_ok() {
        let src = Path::new("/nonexistent/src");
        let dst = Path::new("/nonexistent/dst");
        let result = sync_acls(src, dst, true);
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

    #[test]
    fn apply_acls_from_cache_with_default_ndx_returns_ok() {
        let dst = Path::new("/nonexistent/dst");
        let cache = AclCache::new();
        let result = apply_acls_from_cache(dst, &cache, 0, Some(1), true, Some(0o644));
        assert!(result.is_ok());
    }
}
