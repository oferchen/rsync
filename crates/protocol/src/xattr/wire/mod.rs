//! Wire protocol encoding and decoding for extended attributes.
//!
//! Implements the send/receive functions for xattr data exchange between
//! rsync peers. Supports both full-value and abbreviated (checksum-only)
//! transmission for bandwidth efficiency on large xattr values.
//!
//! # Upstream Reference
//!
//! - `xattrs.c` - `send_xattr_request()`, `recv_xattr_request()`, `send_xattr()`

mod decode;
mod encode;
mod types;

#[cfg(test)]
mod tests;

pub use decode::{
    checksum_matches, read_xattr_definitions, recv_xattr, recv_xattr_request, recv_xattr_values,
};
pub use encode::{send_xattr, send_xattr_request, send_xattr_values};
pub use types::{RecvXattrResult, XattrDefinition, XattrSet};
