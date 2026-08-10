//! Timing and byte-count statistics collected during the generator transfer.

use std::time::{Duration, Instant};

/// Timing and byte-count statistics collected during the transfer.
///
/// Tracks timestamps for file list build and transfer phases, plus total
/// bytes read from the network. Used to compute `flist_buildtime` and
/// `flist_xfertime` statistics sent to the client (protocol >= 29).
///
/// # Upstream Reference
///
/// - `main.c:356-384` - `handle_stats()` sends build/xfer times
/// - `flist.c:2192` - `stats.flist_buildtime` timing
#[derive(Debug)]
pub(crate) struct TransferTiming {
    /// When file list building started (for flist_buildtime statistic).
    pub(crate) flist_build_start: Option<Instant>,
    /// When file list building ended (for flist_buildtime statistic).
    pub(crate) flist_build_end: Option<Instant>,
    /// When file list transfer started (for flist_xfertime statistic).
    pub(crate) flist_xfer_start: Option<Instant>,
    /// When file list transfer ended (for flist_xfertime statistic).
    pub(crate) flist_xfer_end: Option<Instant>,
    /// Elapsed time from `send_file_list` entry to the first byte hitting the
    /// wire. Diagnostic counter for sender-side INC_RECURSE (#2089) - tracks
    /// how long the receiver waits before observing any file list data.
    ///
    /// upstream: flist.c send_file_list / send_dir_name first-byte timing
    pub(crate) flist_first_byte_latency: Option<Duration>,
    /// Total bytes read from network during transfer (for total_read statistic).
    pub(crate) total_bytes_read: u64,
}

impl TransferTiming {
    /// Creates a new timing tracker with no recorded timestamps.
    pub(crate) fn new() -> Self {
        Self {
            flist_build_start: None,
            flist_build_end: None,
            flist_xfer_start: None,
            flist_xfer_end: None,
            flist_first_byte_latency: None,
            total_bytes_read: 0,
        }
    }
}
