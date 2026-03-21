//! Transfer and deletion statistics wire format encoding and decoding.
//!
//! This module implements the wire format for exchanging transfer statistics
//! between rsync processes. The format varies by protocol version:
//!
//! - Protocol 30+: Uses varlong30 encoding with 3-byte minimum
//! - Protocol 29: Adds flist build/transfer time fields
//! - Protocol < 29: Basic stats only
//!
//! # Submodules
//!
//! - `transfer` - Transfer statistics struct, constructors, and wire format
//! - `delete` - Deletion statistics struct and wire format
//! - `display` - Display formatting for transfer statistics

mod delete;
mod display;
mod transfer;

#[cfg(test)]
mod tests;

pub use delete::DeleteStats;
pub use transfer::TransferStats;
