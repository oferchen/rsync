//! Transfer and deletion statistics wire format encoding and decoding.
//!
//! Wire format varies by protocol version:
//!
//! - Protocol 30+: varlong30 encoding with 3-byte minimum
//! - Protocol 29: adds flist build/transfer time fields
//! - Protocol < 29: basic byte counts only

mod created;
mod delete;
mod transfer;

#[cfg(test)]
mod tests;

pub use created::CreatedStats;
pub use delete::DeleteStats;
pub use transfer::TransferStats;
