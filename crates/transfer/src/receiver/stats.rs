//! Transfer statistics types for the receiver role.
//!
//! Contains `TransferStats` (receiver-side transfer results) and
//! `SenderStats` (statistics received from the remote sender).

use std::path::PathBuf;

use protocol::stats::DeleteStats;

/// Statistics from a receiver transfer operation.
///
/// Returned inside [`crate::ServerStats::Receiver`] after a successful receive.
/// Contains file counts, byte totals, metadata error records, and incremental-mode
/// statistics.
#[derive(Debug, Clone, Default)]
pub struct TransferStats {
    /// Number of files in the received file list.
    pub files_listed: usize,
    /// Number of files actually transferred.
    pub files_transferred: usize,
    /// Total bytes received from the sender (file data, deltas, etc.).
    pub bytes_received: u64,
    /// Total bytes sent to the sender (signatures, file indices, etc.).
    ///
    /// This tracks data sent back during the transfer, such as signature blocks
    /// for delta generation and file index requests. Mirrors upstream rsync's
    /// `stats.total_written` tracking in io.c:859.
    pub bytes_sent: u64,
    /// Total size of all source files in the file list.
    ///
    /// This is the sum of all file sizes from the received file list,
    /// used to calculate speedup ratio (total_size / bytes_transferred).
    pub total_source_bytes: u64,
    /// Metadata errors encountered (path, error message).
    pub metadata_errors: Vec<(PathBuf, String)>,
    /// Accumulated I/O error flags from the sender's file list trailer.
    ///
    /// This bitfield uses the constants from [`crate::generator::io_error_flags`] and is
    /// propagated to the client summary so the exit code reflects any I/O
    /// errors that occurred during the transfer.
    ///
    /// # Upstream Reference
    ///
    /// - `flist.c:2518`: `write_int(f, ignore_errors ? 0 : io_error);`
    pub io_error: i32,
    /// Number of `MSG_ERROR` messages received from the remote sender.
    ///
    /// When the sender encounters per-file errors it sends `MSG_ERROR` frames
    /// that the receiver tallies here. A non-zero count causes the exit code
    /// to report a partial transfer (`RERR_PARTIAL`, exit 23).
    pub error_count: u32,

    // Incremental mode statistics
    /// Total entries received from wire (incremental mode).
    pub entries_received: u64,
    /// Directories successfully created (incremental mode).
    pub directories_created: u64,
    /// Directories that failed to create (incremental mode).
    pub directories_failed: u64,
    /// Files skipped due to failed parent directory (incremental mode).
    pub files_skipped: u64,

    /// Breakdown of extraneous items deleted at the destination (`--delete`).
    pub delete_stats: DeleteStats,

    /// Whether deletion was stopped due to `--max-delete` limit.
    ///
    /// When true, the caller should report exit code 25 (`RERR_DEL_LIMIT`).
    ///
    /// # Upstream Reference
    ///
    /// - `main.c:1367` - `deletion_count >= max_delete` triggers exit 25
    pub delete_limit_exceeded: bool,

    /// Total literal (new) data bytes written during delta application.
    ///
    /// Accumulated from per-file delta token processing. Literal tokens carry
    /// data that does not match any block in the basis file.
    ///
    /// # Upstream Reference
    ///
    /// - `match.c:330` - `stats.literal_data += s->sums[j].len`
    pub literal_data: u64,

    /// Total matched (reused) data bytes during delta application.
    ///
    /// Accumulated from per-file delta token processing. Matched tokens
    /// reference blocks copied from the basis file.
    ///
    /// # Upstream Reference
    ///
    /// - `match.c:118` - `stats.matched_data += s2length`
    pub matched_data: u64,

    /// Number of files that were retransmitted due to checksum verification failure.
    ///
    /// Mirrors upstream rsync's redo mechanism where files that fail whole-file
    /// checksum after delta application are re-requested with an empty basis
    /// (whole-file transfer) in phase 2.
    ///
    /// # Upstream Reference
    ///
    /// - `receiver.c:970-974` - `send_msg_int(MSG_REDO, ndx)` queues for redo
    /// - `generator.c:2160-2199` - generator processes redo queue in phase 2
    pub redo_count: usize,
}

/// Statistics received from the remote sender after transfer completion.
///
/// The sender transmits these statistics over the wire after the transfer
/// loop finishes but before the goodbye handshake. The receiver uses them
/// to compute the speedup ratio displayed in `--stats` output.
#[derive(Debug, Clone, Default)]
pub struct SenderStats {
    /// Total bytes read by the sender during transfer.
    pub total_read: u64,
    /// Total bytes written by the sender during transfer.
    pub total_written: u64,
    /// Total size of all source files.
    pub total_size: u64,
    /// File list build time in milliseconds (protocol 29+).
    pub flist_buildtime_ms: Option<u64>,
    /// File list transfer time in milliseconds (protocol 29+).
    pub flist_xfertime_ms: Option<u64>,
}
