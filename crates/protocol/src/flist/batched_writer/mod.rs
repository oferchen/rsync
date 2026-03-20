//! Batched file list writer for reduced network round-trips.
//!
//! This module provides a [`BatchedFileListWriter`] that accumulates multiple
//! file entries before flushing them to the underlying writer in a single
//! operation. This reduces network round-trips when sending file lists over
//! the network.
//!
//! # Batching Strategy
//!
//! The writer flushes the batch when any of these conditions are met:
//! - The batch contains `max_entries` entries (default: 64)
//! - The batch size exceeds `max_bytes` bytes (default: 64KB)
//! - The flush timeout expires (default: 100ms)
//! - An explicit flush is requested
//! - The writer is dropped (auto-flush)
//!
//! # Usage
//!
//! ```no_run
//! use protocol::flist::{BatchedFileListWriter, FileEntry};
//! use protocol::ProtocolVersion;
//!
//! let protocol = ProtocolVersion::try_from(32u8).unwrap();
//! let mut writer = std::io::sink();
//!
//! let mut batched = BatchedFileListWriter::new(protocol);
//!
//! // Add entries - they're accumulated in a batch
//! let entry = FileEntry::new_file("test.txt".into(), 100, 0o644);
//! batched.add_entry(&mut writer, &entry)?;
//!
//! // Entries are written when batch is full or on explicit flush
//! batched.flush(&mut writer)?;
//! # Ok::<(), std::io::Error>(())
//! ```

mod config;
mod stats;
#[cfg(test)]
mod tests;
mod writer;

pub use config::BatchConfig;
pub use stats::BatchStats;
pub use writer::BatchedFileListWriter;
