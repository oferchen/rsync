//! ACL wire protocol types.

use super::super::entry::RsyncAcl;

/// ACL type for wire protocol.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AclType {
    /// Access ACL (file permissions).
    Access,
    /// Default ACL (inherited by new files in directory).
    Default,
}

/// Result of receiving an ACL from the wire.
#[derive(Debug)]
pub enum RecvAclResult {
    /// Cache hit - use the ACL at this index.
    CacheHit(u32),
    /// Literal ACL data was received.
    Literal(RsyncAcl),
}
