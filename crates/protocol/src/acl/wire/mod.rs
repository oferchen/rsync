//! ACL wire protocol encoding and decoding functions.
//!
//! Implements the send/receive functions for ACL data exchange,
//! mirroring upstream rsync's `acls.c` implementation.
//!
//! # Submodules
//!
//! - `encoding` - Access bit encoding/decoding for wire transmission
//! - `recv` - Wire protocol receive functions
//! - `send` - Wire protocol send functions
//! - `types` - ACL wire protocol types

mod encoding;
mod recv;
mod send;
mod types;

#[cfg(test)]
mod tests;

pub use recv::{receive_acl_cached, recv_acl, recv_ida_entries, recv_rsync_acl};
pub use send::{send_acl, send_ida_entries, send_rsync_acl};
pub use types::{AclType, RecvAclResult};
