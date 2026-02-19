//! Channel messages for the decoupled receiver architecture.
//!
//! Defines the streaming protocol between the network ingest thread and the
//! disk commit thread. Each file follows the sequence:
//! `Begin -> N x Chunk -> Commit` (or `Abort` on error).
//!
//! The `Shutdown` message terminates the disk thread after all files are
//! processed.

use std::path::PathBuf;

use protocol::flist::FileEntry;

use crate::delta_apply::ChecksumVerifier;

/// Messages from the network thread to the disk commit thread.
///
/// Follows a per-file protocol: `Begin -> Chunk* -> Commit | Abort`.
/// Small single-chunk files may use the coalesced `WholeFile` variant.
/// The `Shutdown` variant terminates the disk thread.
pub enum FileMessage {
    /// Start writing a new file.
    Begin(Box<BeginMessage>),
    /// A chunk of file data to write.
    Chunk(Vec<u8>),
    /// Finalize the current file (flush, fsync, rename).
    Commit,
    /// Coalesced message for single-chunk files: combines Begin + one Chunk +
    /// Commit into a single channel send, reducing futex overhead from 3+
    /// sends to 1. Used when the sender transmits the entire file as a single
    /// literal token (common for small files).
    WholeFile {
        /// File metadata and configuration.
        begin: Box<BeginMessage>,
        /// Complete file data.
        data: Vec<u8>,
    },
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
    /// Checksum verifier for computing per-file integrity digest on the disk
    /// thread. When `Some`, the disk thread hashes every chunk it writes and
    /// returns the final digest in [`CommitResult::computed_checksum`].
    /// When `None`, no checksum is computed (legacy path).
    pub checksum_verifier: Option<ChecksumVerifier>,
    /// File entry from the file list, used for metadata application after
    /// commit. When `Some`, the disk thread applies metadata (mtime, perms,
    /// ownership) immediately after rename — mirroring upstream
    /// `finish_transfer()` → `set_file_attrs()` in receiver.c.
    pub file_entry: Option<FileEntry>,
}

/// Computed checksum digest returned by the disk thread.
pub struct ComputedChecksum {
    /// Digest bytes (only `len` bytes are valid).
    pub bytes: [u8; ChecksumVerifier::MAX_DIGEST_LEN],
    /// Number of valid bytes in `bytes`.
    pub len: usize,
}

/// Result of committing a file to disk, sent back from the disk thread.
pub struct CommitResult {
    /// Number of bytes written to the file.
    pub bytes_written: u64,
    /// Index into the file list (correlates with `BeginMessage::file_entry_index`).
    pub file_entry_index: usize,
    /// Non-fatal metadata error, if any (path, description).
    pub metadata_error: Option<(PathBuf, String)>,
    /// Computed per-file checksum, if verification was deferred to the disk thread.
    pub computed_checksum: Option<ComputedChecksum>,
}
