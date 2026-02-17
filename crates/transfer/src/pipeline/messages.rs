//! Channel messages for the decoupled receiver architecture.
//!
//! Defines the streaming protocol between the network ingest thread and the
//! disk commit thread. Each file follows the sequence:
//! `Begin -> N x Chunk -> Commit` (or `Abort` on error).
//!
//! The `Shutdown` message terminates the disk thread after all files are
//! processed.

use std::path::PathBuf;

/// Messages from the network thread to the disk commit thread.
///
/// Follows a per-file protocol: `Begin -> Chunk* -> Commit | Abort`.
/// The `Shutdown` variant terminates the disk thread.
pub enum FileMessage {
    /// Start writing a new file.
    Begin(BeginMessage),
    /// A chunk of file data to write.
    Chunk(Vec<u8>),
    /// Finalize the current file (flush, fsync, rename).
    Commit,
    /// Abort the current file due to an error.
    Abort {
        /// Human-readable reason for the abort.
        reason: String,
    },
    /// Shut down the disk commit thread.
    Shutdown,
}

/// Metadata for starting a new file write on the disk thread.
pub struct BeginMessage {
    /// Destination path for the file.
    pub file_path: PathBuf,
    /// Target file size (used for adaptive buffer sizing).
    pub target_size: u64,
    /// Index into the file list (for metadata application).
    pub file_entry_index: usize,
    /// Whether to use sparse file writing.
    pub use_sparse: bool,
    /// Whether to attempt direct write (skip temp+rename for new files).
    pub direct_write: bool,
}

/// Result of committing a file to disk, sent back from the disk thread.
pub struct CommitResult {
    /// Number of bytes written to the file.
    pub bytes_written: u64,
    /// Index into the file list (correlates with `BeginMessage::file_entry_index`).
    pub file_entry_index: usize,
    /// Non-fatal metadata error, if any (path, description).
    pub metadata_error: Option<(PathBuf, String)>,
}
