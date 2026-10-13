//! Protocol 30+ capability negotiation.
//!
//! This module implements the `negotiate_the_strings()` function from upstream
//! rsync (compat.c:538-574), which negotiates checksum and compression algorithms
//! between client and server for Protocol 30+.
//!
//! # Protocol Flow
//!
//! For protocol versions >= 30, after the compatibility flags exchange, peers
//! negotiate which checksum and compression algorithms to use. When the peer
//! advertised `CF_VARINT_FLIST_FLAGS`, the exchange is the same in SSH and
//! daemon mode:
//!
//! 1. Both sides send their supported algorithm lists
//! 2. Both sides read each other's lists
//! 3. Each side independently selects a mutually supported algorithm
//!
//! Without that flag no lists cross the wire and both sides fall back to the
//! old-style defaults.
//!
//! # References
//!
//! - Upstream: `compat.c:538-574` (negotiate_the_strings)
//! - Upstream: `compat.c:333-407` (parse_negotiate_str, recv_negotiate_str)

mod algorithms;
mod env_list;
mod failure;
mod negotiate;

pub use algorithms::{ChecksumAlgorithm, CompressionAlgorithm};
pub use negotiate::{
    NegotiationConfig, NegotiationResult, negotiate_capabilities,
    negotiate_capabilities_with_override,
};

#[cfg(test)]
#[allow(clippy::uninlined_format_args)]
mod tests;
