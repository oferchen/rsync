//! Batch file binary format definitions.
//!
//! This module defines the structures and serialization for the batch file
//! format, maintaining byte-for-byte compatibility with upstream rsync.
//!
//! # Submodules
//!
//! - `flags` - Stream flags bitmap ([`BatchFlags`])
//! - `header` - Protocol negotiation header ([`BatchHeader`])
//! - `stats` - Transfer statistics ([`BatchStats`])
//! - `file_entry` - Internal file metadata tracking ([`FileEntry`])
//! - `wire` - Low-level read/write primitives (crate-internal)

mod file_entry;
mod flags;
mod header;
mod stats;
pub(crate) mod wire;

#[cfg(test)]
mod tests;

pub use file_entry::FileEntry;
pub use flags::BatchFlags;
pub use header::BatchHeader;
pub use stats::BatchStats;
