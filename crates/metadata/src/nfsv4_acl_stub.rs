//! No-op NFSv4 ACL stubs for platforms without xattr support.
//!
//! On non-Unix platforms or when the `xattr` feature is disabled,
//! NFSv4 ACLs are not available. This module provides no-op
//! implementations of the public API.

use crate::MetadataError;
use std::path::Path;

/// NFSv4 ACL extended attribute name.
pub const NFS4_ACL_XATTR: &str = "system.nfs4_acl";

/// A single NFSv4 Access Control Entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Nfs4Ace {
    /// ACE type (allow/deny).
    pub ace_type: AceType,
    /// ACE flags (inheritance, etc.).
    pub flags: AceFlags,
    /// Permission mask.
    pub mask: AccessMask,
    /// Principal identifier.
    pub who: String,
}

/// NFSv4 ACE type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AceType {
    /// Allow access.
    Allow = 0,
    /// Deny access.
    Deny = 1,
}

/// NFSv4 ACE flags (stub — bitflags not needed for no-op).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AceFlags(pub u32);

/// NFSv4 access mask (stub — bitflags not needed for no-op).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AccessMask(pub u32);

/// A complete NFSv4 ACL.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Nfs4Acl {
    /// The list of ACEs.
    pub aces: Vec<Nfs4Ace>,
}

/// Retrieves the NFSv4 ACL from a file.
///
/// On platforms without xattr support, always returns `Ok(None)`.
pub fn get_nfsv4_acl(
    _path: &Path,
    _follow_symlinks: bool,
) -> Result<Option<Nfs4Acl>, MetadataError> {
    Ok(None)
}

/// Sets the NFSv4 ACL on a file.
///
/// On platforms without xattr support, this is a no-op.
pub fn set_nfsv4_acl(
    _path: &Path,
    _acl: Option<&Nfs4Acl>,
    _follow_symlinks: bool,
) -> Result<(), MetadataError> {
    Ok(())
}

/// Synchronises NFSv4 ACLs between two files.
///
/// On platforms without xattr support, this is a no-op.
pub fn sync_nfsv4_acls(
    _source: &Path,
    _destination: &Path,
    _follow_symlinks: bool,
) -> Result<(), MetadataError> {
    Ok(())
}

/// Checks if a file has an NFSv4 ACL.
///
/// On platforms without xattr support, always returns `false`.
pub fn has_nfsv4_acl(_path: &Path, _follow_symlinks: bool) -> bool {
    false
}
