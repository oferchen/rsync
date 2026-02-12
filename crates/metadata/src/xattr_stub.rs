//! No-op xattr stubs for platforms without extended attribute support.
//!
//! On non-Unix platforms or when the `xattr` feature is disabled,
//! extended attributes are not available. This module provides a
//! no-op `sync_xattrs` so callers can use the same API unconditionally.

use crate::error::MetadataError;
use std::path::Path;

/// Synchronises extended attributes from `source` to `destination`.
///
/// On platforms without xattr support, this is a no-op that silently succeeds.
pub fn sync_xattrs(
    _source: &Path,
    _destination: &Path,
    _follow_symlinks: bool,
    _filter: Option<&dyn Fn(&str) -> bool>,
) -> Result<(), MetadataError> {
    Ok(())
}
