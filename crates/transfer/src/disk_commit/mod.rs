//! Disk commit thread for the decoupled receiver architecture.
//!
//! Consumes `FileMessage` items from a bounded channel and performs all
//! disk I/O: opening files, writing chunks, flushing, renaming, and metadata
//! application.  Runs on a dedicated [`std::thread`] so the network thread
//! never blocks on disk.
//!
//! # Thread Protocol
//!
//! ```text
//! Network thread                      Disk thread
//! ──────────────                      ───────────
//! Begin(msg)   ──────────────────▶    open file
//! Chunk(data)  ──────────────────▶    write data
//! ...          ──────────────────▶    ...
//! Commit       ──────────────────▶    flush / rename
//!              ◀──────────────────    Ok(CommitResult)
//! ```
//!
//! A `Shutdown` message terminates the thread after draining in-progress work.

/// Configuration types for the disk commit thread.
mod config;
/// File processing: chunked writes, whole-file writes, output file opening,
/// backup creation, and post-commit metadata application.
mod process;
/// Thread spawning, main loop, and channel handle.
mod thread;
/// Buffered writer with vectored I/O and direct-write bypass.
mod writer;

#[cfg(test)]
mod tests;

pub use self::config::{BackupConfig, DiskCommitConfig, DEFAULT_CHANNEL_CAPACITY};
pub use self::thread::{DiskThreadHandle, spawn_disk_thread};
