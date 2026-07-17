//! Transfer statistics types for the receiver role.
//!
//! Contains `TransferStats` (receiver-side transfer results) and
//! `SenderStats` (statistics received from the remote sender).

use std::path::PathBuf;

use protocol::stats::{CreatedStats, DeleteStats};

/// A single file-list entry captured for `--list-only` rendering.
///
/// In list-only mode the receiver renders the file list without requesting any
/// file data. Each active flist entry is snapshotted here so the client can
/// format the upstream listing line (perms / size / date / name).
///
/// # Upstream Reference
///
/// - `generator.c:1249` - `list_file_entry()` renders one line per flist entry
#[derive(Debug, Clone)]
pub struct ListOnlyEntry {
    /// Relative path of the entry within the transferred tree.
    pub path: PathBuf,
    /// Full Unix mode bits (file type + permissions).
    pub mode: u32,
    /// Logical size in bytes.
    pub size: u64,
    /// Modification time in whole seconds since the Unix epoch.
    pub mtime: i64,
    /// Sub-second component of the modification time, in nanoseconds.
    pub mtime_nsec: u32,
    /// Access time in whole seconds since the Unix epoch.
    ///
    /// Rendered as the ATIME column when `-U`/`--atimes` is active.
    ///
    /// # Upstream Reference
    ///
    /// - `generator.c` `list_file_entry()` - `F_ATIME(f)` field
    pub atime: i64,
    /// Sub-second component of the access time, in nanoseconds.
    pub atime_nsec: u32,
    /// Creation (birth) time in whole seconds since the Unix epoch.
    ///
    /// Rendered as the CRTIME column when `--crtimes` is active.
    ///
    /// # Upstream Reference
    ///
    /// - `generator.c` `list_file_entry()` - `F_CRTIME(f)` field
    pub crtime: i64,
    /// Sub-second component of the creation time, in nanoseconds.
    ///
    /// Always `0`: the flist `FileEntry` does not carry a crtime nanosecond
    /// component (only whole-second crtime is transmitted).
    pub crtime_nsec: u32,
    /// Symlink target when the entry is a symbolic link.
    pub symlink_target: Option<PathBuf>,
    /// Whether the entry is a symbolic link.
    pub is_symlink: bool,
}

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
    /// Summed length of every transferred file (upstream: `total_transferred_size`).
    ///
    /// On a pull the local receiver computes this itself (`receiver.c:784`
    /// `stats.total_transferred_size += F_LENGTH(file)`); upstream never sends
    /// it over the wire in `handle_stats()`, so the pulling client reports it
    /// straight from this locally accumulated total.
    pub transferred_file_size: u64,
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

    /// Per-type tally of entries created at the destination (destination absent
    /// before the transfer), reconstructed locally from the `ITEM_IS_NEW`
    /// itemize flags. Never sent over the wire - upstream recomputes the
    /// "Number of created files" breakdown on the client from its own itemize
    /// pass, and this mirrors that for a remote pull.
    ///
    /// # Upstream Reference
    ///
    /// - `receiver.c:733-746` - `stats.created_*++` under `ITEM_IS_NEW`.
    pub created_stats: CreatedStats,

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

    /// File-list entries captured for `--list-only` rendering.
    ///
    /// Populated only in list-only mode; empty otherwise. The client converts
    /// these into metadata-bearing summary events so the listing can be printed.
    ///
    /// # Upstream Reference
    ///
    /// - `generator.c:1249` - `list_file_entry()` per-entry render
    pub list_only_entries: Vec<ListOnlyEntry>,
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
