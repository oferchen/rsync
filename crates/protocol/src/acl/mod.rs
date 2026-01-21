//! ACL wire protocol encoding and decoding.
//!
//! This module implements rsync's ACL (Access Control List) wire protocol
//! for transmitting POSIX ACLs between systems. The protocol uses an
//! index-based caching system similar to xattrs.
//!
//! # Wire Protocol Overview
//!
//! ACLs are transmitted in two parts:
//!
//! 1. **Index**: A varint indicating whether this ACL matches a previously
//!    sent ACL (cache hit) or requires literal data (cache miss).
//!
//! 2. **Literal data** (when index is 0): The ACL components encoded as:
//!    - Flags byte indicating which standard entries are present
//!    - Standard entries (user_obj, group_obj, mask_obj, other_obj)
//!    - Named user/group entries list (ida_entries)
//!
//! # Access Bit Encoding
//!
//! Access bits are shifted left by 2 and the lower 2 bits encode flags:
//! - Bit 0: `XFLAG_NAME_FOLLOWS` - User/group name string follows
//! - Bit 1: `XFLAG_NAME_IS_USER` - Entry is for a user (vs group)
//!
//! This encoding keeps high bits clear for efficient varint encoding.
//!
//! # Upstream Reference
//!
//! - `acls.c` lines 37-88: ACL data structures
//! - `acls.c` lines 580-668: `send_ida_entries`, `send_rsync_acl`, `send_acl`
//! - `acls.c` lines 670-800: `recv_acl_access`, `recv_ida_entries`, `recv_rsync_acl`

mod constants;
mod entry;
mod wire;

pub use constants::*;
pub use entry::{AclCache, IdAccess, IdaEntries, RsyncAcl};
pub use wire::{
    AclType, RecvAclResult, recv_acl, recv_ida_entries, recv_rsync_acl, send_acl, send_ida_entries,
    send_rsync_acl,
};

#[cfg(test)]
mod tests;
