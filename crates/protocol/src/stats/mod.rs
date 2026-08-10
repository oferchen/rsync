//! Transfer and deletion statistics wire format encoding and decoding.
//!
//! Wire format varies by protocol version:
//!
//! - Protocol 30+: varlong30 encoding with 3-byte minimum
//! - Protocol 29: adds flist build/transfer time fields
//! - Protocol < 29: basic byte counts only

mod delete;
mod display;
mod transfer;

#[cfg(test)]
mod tests;

pub use delete::DeleteStats;
pub use transfer::TransferStats;
