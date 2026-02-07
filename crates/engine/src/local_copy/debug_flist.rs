//! DEBUG_FLIST tracing for file list operations.
//!
//! This module provides structured tracing for file list send/receive operations
//! that match upstream rsync's flist.c debug output format. All tracing is
//! conditionally compiled behind the `tracing` feature flag and produces no-op
//! inline functions when disabled.
//!
//! # Examples
//!
//! ```rust,ignore
//! use engine::local_copy::debug_flist::{FlistTracer, trace_send_file_list_entry};
//!
//! let mut tracer = FlistTracer::new();
//! tracer.start_send();
//!
//! trace_send_file_list_entry("file.txt", 1024, 1704067200, 0o644);
//! tracer.record_entry("file.txt", 1024);
//!
//! tracer.finish_send();
//! ```

use std::time::{Duration, Instant};

/// Target name for tracing events, matching rsync's debug category.
const FLIST_TARGET: &str = "rsync::flist";

// ============================================================================
// Tracing functions (feature-gated)
// ============================================================================

/// Traces the start of a file list send operation.
///
/// Emits a tracing span that tracks the duration of sending the file list.
/// In upstream rsync, this corresponds to the entry point of `send_file_list()`.
///
/// # Arguments
///
/// * `expected_count` - Estimated number of files to be sent (may be 0 if unknown)
#[cfg(feature = "tracing")]
#[inline]
pub fn trace_send_file_list_start(expected_count: usize) {
    tracing::info!(
        target: FLIST_TARGET,
        expected_count = expected_count,
        "send_file_list: starting"
    );
}

/// No-op when tracing is disabled.
#[cfg(not(feature = "tracing"))]
#[inline]
pub fn trace_send_file_list_start(_expected_count: usize) {}

/// Traces a single file list entry being sent.
///
/// Logs detailed metadata for each file being transmitted, matching upstream
/// rsync's per-file debug output format from flist.c.
///
/// # Arguments
///
/// * `name` - Relative path of the file
/// * `size` - File size in bytes
/// * `mtime` - Modification time as Unix timestamp
/// * `mode` - File mode bits (Unix permissions + file type)
#[cfg(feature = "tracing")]
#[inline]
pub fn trace_send_file_list_entry(name: &str, size: u64, mtime: i64, mode: u32) {
    tracing::debug!(
        target: FLIST_TARGET,
        name = %name,
        size = size,
        mtime = mtime,
        mode = format!("{:06o}", mode),
        "send_file_list: entry"
    );
}

/// No-op when tracing is disabled.
#[cfg(not(feature = "tracing"))]
#[inline]
pub fn trace_send_file_list_entry(_name: &str, _size: u64, _mtime: i64, _mode: u32) {}

/// Traces the completion of a file list send operation.
///
/// Emits summary statistics for the entire send operation, matching the
/// output format of upstream rsync's flist.c completion logging.
///
/// # Arguments
///
/// * `count` - Total number of files sent
/// * `total_size` - Aggregate size of all files in bytes
/// * `elapsed` - Time taken to build and send the list
#[cfg(feature = "tracing")]
#[inline]
pub fn trace_send_file_list_end(count: usize, total_size: u64, elapsed: Duration) {
    tracing::info!(
        target: FLIST_TARGET,
        count = count,
        total_size = total_size,
        elapsed_ms = elapsed.as_millis(),
        "send_file_list: complete"
    );
}

/// No-op when tracing is disabled.
#[cfg(not(feature = "tracing"))]
#[inline]
pub fn trace_send_file_list_end(_count: usize, _total_size: u64, _elapsed: Duration) {}

/// Traces the start of a file list receive operation.
///
/// Emits a tracing span for tracking receive duration. In upstream rsync,
/// this corresponds to the entry point of `receive_file_list()`.
#[cfg(feature = "tracing")]
#[inline]
pub fn trace_recv_file_list_start() {
    tracing::info!(
        target: FLIST_TARGET,
        "recv_file_list: starting"
    );
}

/// No-op when tracing is disabled.
#[cfg(not(feature = "tracing"))]
#[inline]
pub fn trace_recv_file_list_start() {}

/// Traces a single file list entry being received.
///
/// Logs metadata for each file entry as it arrives from the peer.
///
/// # Arguments
///
/// * `name` - Relative path of the file
/// * `size` - File size in bytes
#[cfg(feature = "tracing")]
#[inline]
pub fn trace_recv_file_list_entry(name: &str, size: u64) {
    tracing::debug!(
        target: FLIST_TARGET,
        name = %name,
        size = size,
        "recv_file_list: entry"
    );
}

/// No-op when tracing is disabled.
#[cfg(not(feature = "tracing"))]
#[inline]
pub fn trace_recv_file_list_entry(_name: &str, _size: u64) {}

/// Traces the completion of a file list receive operation.
///
/// Emits summary statistics for the entire receive operation.
///
/// # Arguments
///
/// * `count` - Total number of files received
/// * `total_size` - Aggregate size of all files in bytes
/// * `elapsed` - Time taken to receive and process the list
#[cfg(feature = "tracing")]
#[inline]
pub fn trace_recv_file_list_end(count: usize, total_size: u64, elapsed: Duration) {
    tracing::info!(
        target: FLIST_TARGET,
        count = count,
        total_size = total_size,
        elapsed_ms = elapsed.as_millis(),
        "recv_file_list: complete"
    );
}

/// No-op when tracing is disabled.
#[cfg(not(feature = "tracing"))]
#[inline]
pub fn trace_recv_file_list_end(_count: usize, _total_size: u64, _elapsed: Duration) {}

// ============================================================================
// FlistTracer - stateful tracer for aggregating file list statistics
// ============================================================================

/// Aggregates statistics during file list operations.
///
/// Tracks entry counts, total sizes, and timing information across an entire
/// file list send or receive operation. Use this when you need to accumulate
/// stats across multiple file entries before emitting final summary events.
///
/// # Examples
///
/// ```no_run
/// # use std::time::Duration;
/// # struct FlistTracer { count: usize, total_size: u64 }
/// # impl FlistTracer {
/// #     fn new() -> Self { Self { count: 0, total_size: 0 } }
/// #     fn start_send(&mut self) {}
/// #     fn record_entry(&mut self, _name: &str, size: u64) {
/// #         self.count += 1;
/// #         self.total_size += size;
/// #     }
/// #     fn finish_send(&mut self) -> Duration { Duration::ZERO }
/// #     fn count(&self) -> usize { self.count }
/// #     fn total_size(&self) -> u64 { self.total_size }
/// # }
/// let mut tracer = FlistTracer::new();
/// tracer.start_send();
///
/// tracer.record_entry("file1.txt", 1024);
/// tracer.record_entry("file2.txt", 2048);
///
/// tracer.finish_send();
/// assert_eq!(tracer.count(), 2);
/// assert_eq!(tracer.total_size(), 3072);
/// ```
#[derive(Debug, Clone)]
pub struct FlistTracer {
    count: usize,
    total_size: u64,
    start_time: Option<Instant>,
}

impl Default for FlistTracer {
    fn default() -> Self {
        Self::new()
    }
}

impl FlistTracer {
    /// Creates a new file list tracer with zero counts.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            count: 0,
            total_size: 0,
            start_time: None,
        }
    }

    /// Starts tracking a send operation, recording the start time.
    pub fn start_send(&mut self) {
        self.start_time = Some(Instant::now());
        trace_send_file_list_start(0);
    }

    /// Starts tracking a receive operation, recording the start time.
    pub fn start_recv(&mut self) {
        self.start_time = Some(Instant::now());
        trace_recv_file_list_start();
    }

    /// Records a file entry, incrementing count and accumulating size.
    ///
    /// # Arguments
    ///
    /// * `name` - File name (used only when tracing is enabled)
    /// * `size` - File size in bytes
    pub fn record_entry(&mut self, _name: &str, size: u64) {
        self.count += 1;
        self.total_size = self.total_size.saturating_add(size);
    }

    /// Records a send entry with full metadata and emits a trace event.
    ///
    /// # Arguments
    ///
    /// * `name` - Relative path of the file
    /// * `size` - File size in bytes
    /// * `mtime` - Modification time as Unix timestamp
    /// * `mode` - File mode bits
    pub fn record_send_entry(&mut self, name: &str, size: u64, mtime: i64, mode: u32) {
        trace_send_file_list_entry(name, size, mtime, mode);
        self.record_entry(name, size);
    }

    /// Records a receive entry and emits a trace event.
    ///
    /// # Arguments
    ///
    /// * `name` - Relative path of the file
    /// * `size` - File size in bytes
    pub fn record_recv_entry(&mut self, name: &str, size: u64) {
        trace_recv_file_list_entry(name, size);
        self.record_entry(name, size);
    }

    /// Finishes tracking a send operation and emits summary trace event.
    ///
    /// Returns the elapsed time since `start_send()` was called, or
    /// `Duration::ZERO` if timing was not initialized.
    pub fn finish_send(&mut self) -> Duration {
        let elapsed = self.elapsed();
        trace_send_file_list_end(self.count, self.total_size, elapsed);
        elapsed
    }

    /// Finishes tracking a receive operation and emits summary trace event.
    ///
    /// Returns the elapsed time since `start_recv()` was called, or
    /// `Duration::ZERO` if timing was not initialized.
    pub fn finish_recv(&mut self) -> Duration {
        let elapsed = self.elapsed();
        trace_recv_file_list_end(self.count, self.total_size, elapsed);
        elapsed
    }

    /// Returns the number of file entries recorded.
    #[must_use]
    pub const fn count(&self) -> usize {
        self.count
    }

    /// Returns the aggregate size of all recorded entries.
    #[must_use]
    pub const fn total_size(&self) -> u64 {
        self.total_size
    }

    /// Returns the elapsed time since tracking started.
    ///
    /// Returns `Duration::ZERO` if `start_send()` or `start_recv()` was not called.
    #[must_use]
    pub fn elapsed(&self) -> Duration {
        self.start_time.map_or(Duration::ZERO, |t| t.elapsed())
    }

    /// Resets all counters and timing state to zero.
    pub fn reset(&mut self) {
        self.count = 0;
        self.total_size = 0;
        self.start_time = None;
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tracer_new() {
        let tracer = FlistTracer::new();
        assert_eq!(tracer.count(), 0);
        assert_eq!(tracer.total_size(), 0);
        assert_eq!(tracer.elapsed(), Duration::ZERO);
    }

    #[test]
    fn test_tracer_default() {
        let tracer = FlistTracer::default();
        assert_eq!(tracer.count(), 0);
        assert_eq!(tracer.total_size(), 0);
    }

    #[test]
    fn test_record_entry_accumulates() {
        let mut tracer = FlistTracer::new();
        tracer.record_entry("file1.txt", 1024);
        tracer.record_entry("file2.txt", 2048);
        tracer.record_entry("file3.txt", 512);

        assert_eq!(tracer.count(), 3);
        assert_eq!(tracer.total_size(), 3584);
    }

    #[test]
    fn test_record_entry_saturating_add() {
        let mut tracer = FlistTracer::new();
        tracer.total_size = u64::MAX - 100;
        tracer.record_entry("huge.bin", 200);

        assert_eq!(tracer.total_size(), u64::MAX);
        assert_eq!(tracer.count(), 1);
    }

    #[test]
    fn test_start_send_initializes_timing() {
        let mut tracer = FlistTracer::new();
        tracer.start_send();

        // Elapsed should be non-zero after some time
        std::thread::sleep(Duration::from_millis(1));
        assert!(tracer.elapsed() > Duration::ZERO);
    }

    #[test]
    fn test_start_recv_initializes_timing() {
        let mut tracer = FlistTracer::new();
        tracer.start_recv();

        std::thread::sleep(Duration::from_millis(1));
        assert!(tracer.elapsed() > Duration::ZERO);
    }

    #[test]
    fn test_finish_send_returns_elapsed() {
        let mut tracer = FlistTracer::new();
        tracer.start_send();
        std::thread::sleep(Duration::from_millis(5));

        let elapsed = tracer.finish_send();
        assert!(elapsed >= Duration::from_millis(5));
    }

    #[test]
    fn test_finish_recv_returns_elapsed() {
        let mut tracer = FlistTracer::new();
        tracer.start_recv();
        std::thread::sleep(Duration::from_millis(5));

        let elapsed = tracer.finish_recv();
        assert!(elapsed >= Duration::from_millis(5));
    }

    #[test]
    fn test_reset_clears_state() {
        let mut tracer = FlistTracer::new();
        tracer.start_send();
        tracer.record_entry("file.txt", 1024);

        tracer.reset();

        assert_eq!(tracer.count(), 0);
        assert_eq!(tracer.total_size(), 0);
        assert_eq!(tracer.elapsed(), Duration::ZERO);
    }

    #[test]
    fn test_record_send_entry_increments() {
        let mut tracer = FlistTracer::new();
        tracer.record_send_entry("file.txt", 2048, 1704067200, 0o644);

        assert_eq!(tracer.count(), 1);
        assert_eq!(tracer.total_size(), 2048);
    }

    #[test]
    fn test_record_recv_entry_increments() {
        let mut tracer = FlistTracer::new();
        tracer.record_recv_entry("received.txt", 4096);

        assert_eq!(tracer.count(), 1);
        assert_eq!(tracer.total_size(), 4096);
    }

    #[test]
    fn test_empty_file_list() {
        let mut tracer = FlistTracer::new();
        tracer.start_send();
        let elapsed = tracer.finish_send();

        assert_eq!(tracer.count(), 0);
        assert_eq!(tracer.total_size(), 0);
        assert!(elapsed >= Duration::ZERO);
    }

    #[test]
    fn test_large_entry_counts() {
        let mut tracer = FlistTracer::new();
        tracer.start_send();

        for i in 0..10_000 {
            let name = format!("file_{}.txt", i);
            tracer.record_entry(&name, 1024);
        }

        tracer.finish_send();
        assert_eq!(tracer.count(), 10_000);
        assert_eq!(tracer.total_size(), 10_240_000);
    }

    #[test]
    fn test_trace_functions_do_not_panic() {
        // All trace functions should be callable without panicking
        trace_send_file_list_start(42);
        trace_send_file_list_entry("test.txt", 1024, 1704067200, 0o644);
        trace_send_file_list_end(42, 43008, Duration::from_millis(100));

        trace_recv_file_list_start();
        trace_recv_file_list_entry("received.txt", 2048);
        trace_recv_file_list_end(10, 20480, Duration::from_millis(50));
    }

    #[test]
    fn test_multiple_operations() {
        let mut tracer = FlistTracer::new();

        // First send
        tracer.start_send();
        tracer.record_entry("file1.txt", 1000);
        tracer.finish_send();

        // Reset and do receive
        tracer.reset();
        tracer.start_recv();
        tracer.record_entry("file2.txt", 2000);
        tracer.finish_recv();

        assert_eq!(tracer.count(), 1);
        assert_eq!(tracer.total_size(), 2000);
    }

    #[cfg(feature = "tracing")]
    #[test]
    fn test_tracing_feature_enabled() {
        // When tracing feature is enabled, verify the functions compile and run
        // without panicking. We can't easily verify event emission without
        // tracing-subscriber, but this at least confirms the code compiles.
        let mut tracer = FlistTracer::new();
        tracer.start_send();
        tracer.record_send_entry("traced.txt", 512, 1704067200, 0o755);
        tracer.finish_send();

        // Verify the tracer still tracks stats correctly
        assert_eq!(tracer.count(), 1);
        assert_eq!(tracer.total_size(), 512);
    }

    #[test]
    fn test_zero_size_entries() {
        let mut tracer = FlistTracer::new();
        tracer.record_entry("empty.txt", 0);
        tracer.record_entry("also_empty.txt", 0);

        assert_eq!(tracer.count(), 2);
        assert_eq!(tracer.total_size(), 0);
    }

    #[test]
    fn test_mode_formatting() {
        // Verify mode formatting doesn't panic
        trace_send_file_list_entry("regular.txt", 1024, 1704067200, 0o100644);
        trace_send_file_list_entry("executable.sh", 2048, 1704067200, 0o100755);
        trace_send_file_list_entry("directory", 0, 1704067200, 0o040755);
    }
}
