//! Transfer and deletion statistics wire format encoding and decoding.
//!
//! Wire format varies by protocol version:
//!
//! - Protocol 30+: `write_varlong30()` emits a varlong with a 3-byte minimum
//!   (below 30 it falls back to `write_longint()`, `io.h:46`)
//! - Protocol 29+: adds the flist build/transfer time fields
//! - Protocol < 29: byte counts only

mod created;
mod delete;
mod transfer;

#[cfg(test)]
mod tests;

pub use created::CreatedStats;
pub use delete::DeleteStats;
pub use transfer::{TransferStats, flist_buildtime_ms};
