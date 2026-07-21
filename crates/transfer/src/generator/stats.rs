//! Statistics types and end-of-transfer helpers for the generator role.
//!
//! Contains the per-transfer `GeneratorStats` returned to callers, the internal
//! `TransferLoopResult` carried between the transfer loop and the goodbye
//! handshake, and [`is_early_close_error`] which classifies peer-disconnect
//! `io::Error` kinds tolerated during dry-run and phase boundaries.

use std::time::Duration;

use protocol::codec::{MonotonicNdxWriter, NdxCodecEnum};
use protocol::flist::FileEntry;
use protocol::stats::{CreatedStats, DeleteStats};

/// Per-type file-list tallies accumulated as the sender writes each entry to
/// the wire, mirroring upstream's `send_file_entry()` counting.
///
/// Directories, symlinks, devices, and specials are counted per type; the
/// regular-file count is the remainder and is never stored here (the summary
/// derives it). `total_size` sums `F_LENGTH` for regular files and symlinks
/// only, exactly as upstream guards the accumulation. Counting at send time -
/// rather than summing the flat `file_list` at the end - is required because
/// INC_RECURSE drains sent segments, leaving only the final sub-list in
/// `file_list` when the transfer completes.
///
/// # Upstream Reference
///
/// - `flist.c:421-438` - `send_file_entry()` bumps `stats.num_dirs` /
///   `num_symlinks` / `num_devices` / `num_specials` per entry.
/// - `flist.c:690-691` - `stats.total_size += F_LENGTH(file)` guarded by
///   `S_ISREG(mode) || S_ISLNK(mode)`.
/// - `main.c:387-411` - `output_itemized_counts()` derives `reg` as the total
///   minus the four typed categories.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct FlistSendStats {
    /// Directories sent (upstream `stats.num_dirs`).
    pub(crate) num_dirs: u64,
    /// Symbolic links sent (upstream `stats.num_symlinks`).
    pub(crate) num_symlinks: u64,
    /// Device nodes, block and character, sent (upstream `stats.num_devices`).
    pub(crate) num_devices: u64,
    /// FIFOs and sockets sent (upstream `stats.num_specials`).
    pub(crate) num_specials: u64,
    /// Summed `F_LENGTH` for regular files and symlinks (upstream
    /// `stats.total_size`); directories, devices, and specials contribute 0.
    pub(crate) total_size: u64,
}

impl FlistSendStats {
    /// Classifies one file-list entry as it is written to the wire and updates
    /// the running per-type counts and `total_size`, mirroring the counting in
    /// upstream `send_file_entry()`.
    ///
    /// # Upstream Reference
    ///
    /// - `flist.c:421-438` - per-type tally.
    /// - `flist.c:690-691` - `total_size` for regular files and symlinks only.
    pub(crate) fn record(&mut self, entry: &FileEntry) {
        if entry.is_dir() {
            self.num_dirs += 1;
        } else if entry.is_symlink() {
            self.num_symlinks += 1;
            self.total_size = self.total_size.saturating_add(entry.size());
        } else if entry.is_device() {
            self.num_devices += 1;
        } else if entry.is_special() {
            self.num_specials += 1;
        } else {
            // Regular file (upstream: S_ISREG - the only remaining type that
            // contributes to total_size).
            self.total_size = self.total_size.saturating_add(entry.size());
        }
    }
}

/// Result from the transfer loop phase of the generator.
///
/// Contains statistics and codec state from processing file transfer requests.
/// The codec state is preserved so the goodbye handshake can continue with
/// the same delta-encoded NDX sequence.
///
/// # Upstream Reference
///
/// - `sender.c:send_files()` - produces these statistics during the main loop
#[derive(Debug, Clone)]
pub(crate) struct TransferLoopResult {
    /// Number of files actually transferred.
    pub(crate) files_transferred: usize,
    /// Summed length of every transferred file (upstream: `total_transferred_size`).
    ///
    /// Mirrors `sender.c:343` `stats.total_transferred_size += F_LENGTH(file)`,
    /// accumulated at the same point as `files_transferred`.
    pub(crate) transferred_file_size: u64,
    /// Total bytes sent during transfer.
    pub(crate) bytes_sent: u64,
    /// Bytes covered by block matches across all files (upstream: matched_data).
    pub(crate) matched_data: u64,
    /// Bytes sent as literal data across all files (upstream: literal_data).
    pub(crate) literal_data: u64,
    /// Per-type tally of entries the receiver reported as created via
    /// `ITEM_IS_NEW` iflags on the wire (upstream: `stats.created_*` in
    /// `sender.c:295-308`). Reconstructed locally, never sent over the wire.
    pub(crate) created_stats: CreatedStats,
    /// NDX read codec state carried over for the goodbye handshake.
    pub(crate) ndx_read_codec: NdxCodecEnum,
    /// NDX write codec state carried over for the goodbye handshake.
    /// Uses `MonotonicNdxWriter` to assert strictly increasing file indices.
    pub(crate) ndx_write_codec: MonotonicNdxWriter,
}

/// Statistics from a generator (sender) transfer operation.
///
/// Returned inside [`crate::ServerStats::Generator`] after a successful send.
/// Contains file counts, byte totals, and file-list timing metrics.
///
/// # Upstream Reference
///
/// - `main.c:356-384` - `handle_stats()` sends/receives these statistics
/// - `sender.c:462` - `total_written` accumulated during `send_files()`
#[derive(Debug, Clone, Default)]
pub struct GeneratorStats {
    /// Number of files in the sent file list.
    pub files_listed: usize,
    /// Directories in the sent file list (upstream `stats.num_dirs`).
    ///
    /// Per-type tallies accumulated as each entry is written to the wire so the
    /// pushing client can reconstruct the `--stats` "Number of files"
    /// breakdown (`reg: R, dir: D, link: L, dev: V, special: S`), where `reg`
    /// is the remainder. Mirrors upstream `send_file_entry()`
    /// (flist.c:421-438); the receiver-side equivalent lives on `TransferStats`.
    pub num_dirs: u64,
    /// Symbolic links in the sent file list (upstream `stats.num_symlinks`).
    pub num_symlinks: u64,
    /// Device nodes, block and character, in the sent file list
    /// (upstream `stats.num_devices`).
    pub num_devices: u64,
    /// FIFOs and sockets in the sent file list (upstream `stats.num_specials`).
    pub num_specials: u64,
    /// Number of files actually transferred (delta or whole-file).
    pub files_transferred: usize,
    /// Summed length of every transferred file (upstream: `total_transferred_size`).
    ///
    /// On a push the local sender computes this itself (`sender.c:343`); upstream
    /// never sends it over the wire in `handle_stats()`, so the pushing client
    /// reports it straight from this locally accumulated total.
    pub transferred_file_size: u64,
    /// Total bytes sent to the receiver (delta data + literals).
    pub bytes_sent: u64,
    /// Total bytes read from the receiver (signatures, NDX requests).
    pub bytes_read: u64,
    /// Bytes covered by block matches (upstream: `stats.matched_data`).
    pub matched_data: u64,
    /// Bytes sent as literal data (upstream: `stats.literal_data`).
    pub literal_data: u64,
    /// Sum of all source file sizes in the flist (upstream: `stats.total_size`).
    pub total_size: u64,
    /// File list build time in milliseconds (upstream: `stats.flist_buildtime`).
    pub flist_buildtime_ms: u64,
    /// File list transfer time in milliseconds (upstream: `stats.flist_xfertime`).
    pub flist_xfertime_ms: u64,
    /// Elapsed time from `send_file_list` entry to the first byte written to
    /// the wire. Diagnostic counter for sender-side INC_RECURSE (#2089).
    ///
    /// `None` when `send_file_list` was never invoked or no bytes were written.
    ///
    /// upstream: flist.c send_file_list / send_dir_name first-byte timing
    pub flist_first_byte_latency: Option<Duration>,
    /// Accumulated deletion statistics from the receiver via `NDX_DEL_STATS`.
    pub delete_stats: DeleteStats,
    /// Per-type tally of entries created at the destination, reconstructed on
    /// the local sender from the `ITEM_IS_NEW` iflags read off the wire. Never
    /// sent over the wire - upstream recomputes the "Number of created files"
    /// breakdown on the client (here, the sender) from its own itemize pass.
    ///
    /// # Upstream Reference
    ///
    /// - `sender.c:295-308` - `stats.created_*++` under `iflags & ITEM_IS_NEW`.
    pub created_stats: CreatedStats,
    /// Accumulated I/O error flags from file list building and transfer.
    ///
    /// Uses [`crate::generator::io_error_flags`] constants. When `IOERR_VANISHED`
    /// is set and `IOERR_GENERAL` is not, the exit code should be 24 (partial
    /// transfer due to vanished files). Propagated to the client summary so
    /// the process exit code reflects files that disappeared mid-transfer.
    ///
    /// # Upstream Reference
    ///
    /// - `main.c:1338-1345`: `log_exit()` maps `io_error` to `RERR_VANISHED` (24).
    pub io_error: i32,
}

/// Returns `true` when the I/O error indicates an early connection close.
///
/// During dry-run and at phase boundaries, the upstream daemon may close the
/// socket before the sender finishes the goodbye handshake. These error kinds
/// all represent "peer went away" rather than a protocol error:
///
/// - `ConnectionReset` - TCP RST from peer
/// - `UnexpectedEof` - clean close mid-read
/// - `BrokenPipe` - write to closed socket
/// - `WouldBlock` - non-blocking socket with no data
/// - `ConnectionAborted` - connection terminated by peer
///
/// # Upstream Reference
///
/// - `sender.c:225-232` - tolerant error handling for dry-run
/// - `main.c:875-906` - `read_final_goodbye()` with early close tolerance
pub(crate) fn is_early_close_error(e: &std::io::Error) -> bool {
    matches!(
        e.kind(),
        std::io::ErrorKind::ConnectionReset
            | std::io::ErrorKind::UnexpectedEof
            | std::io::ErrorKind::BrokenPipe
            | std::io::ErrorKind::WouldBlock
            | std::io::ErrorKind::ConnectionAborted
    )
}
