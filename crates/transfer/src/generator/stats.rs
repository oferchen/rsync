//! Statistics types and end-of-transfer helpers for the generator role.
//!
//! Contains the per-transfer `GeneratorStats` returned to callers, the internal
//! `TransferLoopResult` carried between the transfer loop and the goodbye
//! handshake, and [`is_early_close_error`] which classifies peer-disconnect
//! `io::Error` kinds tolerated during dry-run and phase boundaries.

use std::time::Duration;

use protocol::codec::{MonotonicNdxWriter, NdxCodecEnum};
use protocol::stats::DeleteStats;

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
    /// Total bytes sent during transfer.
    pub(crate) bytes_sent: u64,
    /// Bytes covered by block matches across all files (upstream: matched_data).
    pub(crate) matched_data: u64,
    /// Bytes sent as literal data across all files (upstream: literal_data).
    pub(crate) literal_data: u64,
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
    /// Number of files actually transferred (delta or whole-file).
    pub files_transferred: usize,
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
