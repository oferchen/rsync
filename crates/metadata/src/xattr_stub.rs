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
