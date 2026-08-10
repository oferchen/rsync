//! Protocol 30+ capability negotiation.
//!
//! This module implements the `negotiate_the_strings()` function from upstream
//! rsync (compat.c:534-585), which negotiates checksum and compression algorithms
//! between client and server for Protocol 30+.
//!
//! # Protocol Flow
//!
//! For protocol versions >= 30, after the compatibility flags exchange, peers
//! negotiate which checksum and compression algorithms to use. The flow differs
//! between SSH mode and daemon mode:
//!
//! ## SSH Mode (Bidirectional)
//!
//! 1. Both sides send their supported algorithm lists
//! 2. Both sides read each other's lists
//! 3. Both independently select the first mutually supported algorithm
//!
//! ## Daemon Mode (Unidirectional)
//!
//! 1. Server sends its supported algorithm lists and uses defaults locally
//! 2. Client reads server's lists and selects from them
//! 3. Client does NOT send a response (unidirectional flow)
//!
//! # References
//!
//! - Upstream: `compat.c:534-585` (negotiate_the_strings)
//! - Upstream: `compat.c:332-391` (parse_negotiate_str, recv_negotiate_str)

mod algorithms;
mod negotiate;

pub use algorithms::{ChecksumAlgorithm, CompressionAlgorithm};
pub use negotiate::{
    NegotiationConfig, NegotiationResult, negotiate_capabilities,
    negotiate_capabilities_with_override,
};

#[cfg(test)]
#[allow(clippy::uninlined_format_args)]
mod tests;
