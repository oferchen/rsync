//! No-op xattr stubs for platforms without extended attribute support.
//!
//! On non-Unix platforms or when the `xattr` feature is disabled,
//! extended attributes are not available. This module provides no-op
//! versions of `sync_xattrs` and `apply_xattrs_from_list` so callers
//! can use the same API unconditionally.

use crate::error::MetadataError;
use protocol::xattr::XattrList;
use std::path::Path;
use std::sync::Once;

/// Emits a one-time warning that extended attributes are not supported.
fn warn_xattr_unsupported() {
    static WARN_ONCE: Once = Once::new();
    WARN_ONCE.call_once(|| {
        eprintln!(
            "warning: extended attributes are not supported on this platform; skipping xattr preservation"
        );
    });
}

/// Synchronises extended attributes from `source` to `destination`.
///
/// On platforms without xattr support, emits a one-time warning and
/// returns `Ok(())`.
pub fn sync_xattrs(
    _source: &Path,
    _destination: &Path,
    _follow_symlinks: bool,
    _filter: Option<&dyn Fn(&str) -> bool>,
) -> Result<(), MetadataError> {
    warn_xattr_unsupported();
    Ok(())
}

/// Reads xattr data from a file and returns it as a wire-format `XattrList`.
///
/// On platforms without xattr support, returns an empty list.
pub fn read_xattrs_for_wire(
    _path: &Path,
    _follow_symlinks: bool,
    _am_root: bool,
    _checksum_seed: i32,
) -> Result<XattrList, MetadataError> {
    Ok(XattrList::new())
}

/// Applies parsed xattrs from a wire protocol [`XattrList`] to a destination file.
///
/// On platforms without xattr support, emits a one-time warning and
/// returns `Ok(())`.
pub fn apply_xattrs_from_list(
    _destination: &Path,
    _xattr_list: &XattrList,
    _follow_symlinks: bool,
) -> Result<(), MetadataError> {
    warn_xattr_unsupported();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn sync_xattrs_returns_ok() {
        let src = Path::new("/nonexistent/src");
        let dst = Path::new("/nonexistent/dst");
        let result = sync_xattrs(src, dst, false, None);
        assert!(result.is_ok());
    }

    #[test]
    fn sync_xattrs_with_filter_returns_ok() {
        let src = Path::new("/nonexistent/src");
        let dst = Path::new("/nonexistent/dst");
        let filter = |_name: &str| true;
        let result = sync_xattrs(src, dst, true, Some(&filter));
        assert!(result.is_ok());
    }

    #[test]
    fn read_xattrs_for_wire_returns_empty_list() {
        let path = Path::new("/nonexistent/file");
        let result = read_xattrs_for_wire(path, false, false, 0).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn read_xattrs_for_wire_as_root_returns_empty_list() {
        let path = Path::new("/nonexistent/file");
        let result = read_xattrs_for_wire(path, true, true, 42).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn apply_xattrs_from_list_returns_ok() {
        let dst = Path::new("/nonexistent/dst");
        let xattr_list = XattrList::new();
        let result = apply_xattrs_from_list(dst, &xattr_list, false);
        assert!(result.is_ok());
    }

    #[test]
    fn apply_xattrs_from_list_follow_symlinks_returns_ok() {
        let dst = Path::new("/nonexistent/dst");
        let xattr_list = XattrList::new();
        let result = apply_xattrs_from_list(dst, &xattr_list, true);
        assert!(result.is_ok());
    }
}
