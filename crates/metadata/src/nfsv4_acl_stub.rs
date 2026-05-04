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
#[must_use]
pub fn has_nfsv4_acl(_path: &Path, _follow_symlinks: bool) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn nfs4_acl_xattr_constant_value() {
        assert_eq!(NFS4_ACL_XATTR, "system.nfs4_acl");
    }

    #[test]
    fn get_nfsv4_acl_returns_none() {
        let path = Path::new("/nonexistent/file");
        let result = get_nfsv4_acl(path, false).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn get_nfsv4_acl_follow_symlinks_returns_none() {
        let path = Path::new("/nonexistent/file");
        let result = get_nfsv4_acl(path, true).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn set_nfsv4_acl_with_none_returns_ok() {
        let path = Path::new("/nonexistent/file");
        let result = set_nfsv4_acl(path, None, false);
        assert!(result.is_ok());
    }

    #[test]
    fn set_nfsv4_acl_with_acl_returns_ok() {
        let path = Path::new("/nonexistent/file");
        let acl = Nfs4Acl::default();
        let result = set_nfsv4_acl(path, Some(&acl), true);
        assert!(result.is_ok());
    }

    #[test]
    fn set_nfsv4_acl_with_nonempty_acl_returns_ok() {
        let path = Path::new("/nonexistent/file");
        let acl = Nfs4Acl {
            aces: vec![Nfs4Ace {
                ace_type: AceType::Allow,
                flags: AceFlags(0),
                mask: AccessMask(0x1f),
                who: "OWNER@".to_string(),
            }],
        };
        let result = set_nfsv4_acl(path, Some(&acl), false);
        assert!(result.is_ok());
    }

    #[test]
    fn sync_nfsv4_acls_returns_ok() {
        let src = Path::new("/nonexistent/src");
        let dst = Path::new("/nonexistent/dst");
        let result = sync_nfsv4_acls(src, dst, false);
        assert!(result.is_ok());
    }

    #[test]
    fn has_nfsv4_acl_returns_false() {
        let path = Path::new("/nonexistent/file");
        assert!(!has_nfsv4_acl(path, false));
        assert!(!has_nfsv4_acl(path, true));
    }

    #[test]
    fn nfs4_acl_default_is_empty() {
        let acl = Nfs4Acl::default();
        assert!(acl.aces.is_empty());
    }

    #[test]
    fn ace_type_values() {
        assert_eq!(AceType::Allow as u32, 0);
        assert_eq!(AceType::Deny as u32, 1);
    }

    #[test]
    fn ace_flags_default_is_zero() {
        let flags = AceFlags::default();
        assert_eq!(flags.0, 0);
    }

    #[test]
    fn access_mask_default_is_zero() {
        let mask = AccessMask::default();
        assert_eq!(mask.0, 0);
    }

    #[test]
    fn nfs4_ace_equality() {
        let ace1 = Nfs4Ace {
            ace_type: AceType::Allow,
            flags: AceFlags(0),
            mask: AccessMask(0x1f),
            who: "OWNER@".to_string(),
        };
        let ace2 = ace1.clone();
        assert_eq!(ace1, ace2);
    }

    #[test]
    fn nfs4_acl_clone() {
        let acl = Nfs4Acl {
            aces: vec![Nfs4Ace {
                ace_type: AceType::Deny,
                flags: AceFlags(1),
                mask: AccessMask(0xff),
                who: "GROUP@".to_string(),
            }],
        };
        let cloned = acl.clone();
        assert_eq!(acl, cloned);
    }
}
