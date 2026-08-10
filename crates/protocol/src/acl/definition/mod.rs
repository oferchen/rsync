//! High-level ACL definition types and wire parsing.
//!
//! Provides a POSIX-oriented view of ACL data as parsed from the rsync wire
//! protocol. These types sit above the raw wire-level structures (`RsyncAcl`,
//! `IdAccess`) and present a unified `AclEntry` list that is easier to work
//! with for permission application and display.
//!
//! # Wire Format
//!
//! When a new ACL is transmitted (not a cache reference), the sender writes:
//!
//! ```text
//! flags      : u8       // XMIT_* bits indicating which standard entries exist
//! [user_obj] : varint   // owner permissions (if XMIT_USER_OBJ)
//! [group_obj]: varint   // owning group permissions (if XMIT_GROUP_OBJ)
//! [mask_obj] : varint   // mask permissions (if XMIT_MASK_OBJ)
//! [other_obj]: varint   // world permissions (if XMIT_OTHER_OBJ)
//! [ida_list] : ...      // named user/group entries (if XMIT_NAME_LIST)
//! ```
//!
//! `read_acl_definition` parses this format and returns a flat `AclDefinition`
//! containing all entries as `AclEntry` values with unified `AclTag` tags.
//!
//! # Upstream Reference
//!
//! - `acls.c` lines 731-800: `recv_rsync_acl()`

mod convert;
mod types;
mod wire;

pub use convert::AclDefinition;
pub use types::{AclEntry, AclPerms, AclTag};
pub use wire::{read_acl_definition, write_acl_definition};

#[cfg(test)]
mod tests;
