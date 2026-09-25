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
/// - `flist.c:2428` - `stats.flist_buildtime` timing
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

    /// Returns the `flist_buildtime` statistic for the recorded build span.
    ///
    /// Zero when no file list was built; otherwise upstream's clamped
    /// millisecond value (see `protocol::stats::flist_buildtime_ms`).
    pub(crate) fn flist_buildtime_ms(&self) -> u64 {
        match (self.flist_build_start, self.flist_build_end) {
            (Some(start), Some(end)) => {
                protocol::stats::flist_buildtime_ms(end.duration_since(start))
            }
            _ => 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A build that finishes within the same millisecond must still send a
    /// non-zero `flist_buildtime`, or an upstream client pulling with
    /// `--stats` omits its "File list generation time" line (main.c:453).
    #[test]
    fn instantaneous_build_reports_one_millisecond() {
        let now = Instant::now();
        let mut timing = TransferTiming::new();
        timing.flist_build_start = Some(now);
        timing.flist_build_end = Some(now);
        assert_eq!(timing.flist_buildtime_ms(), 1);
    }

    /// No build span recorded means no file list was sent, so nothing to clamp.
    #[test]
    fn missing_build_span_reports_zero() {
        let mut timing = TransferTiming::new();
        assert_eq!(timing.flist_buildtime_ms(), 0);
        timing.flist_build_start = Some(Instant::now());
        assert_eq!(timing.flist_buildtime_ms(), 0);
    }

    #[test]
    fn measured_build_reports_whole_milliseconds() {
        let start = Instant::now();
        let mut timing = TransferTiming::new();
        timing.flist_build_start = Some(start);
        timing.flist_build_end = Some(start + Duration::from_millis(42));
        assert_eq!(timing.flist_buildtime_ms(), 42);
    }
}
