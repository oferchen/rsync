//! DEBUG_SEND tracing for sender operations.
//!
//! This module provides structured tracing for file send/delta operations
//! that match upstream rsync's sender.c debug output format. All tracing is
//! conditionally compiled behind the `tracing` feature flag and produces no-op
//! inline functions when disabled.
//!
//! # Examples
//!
//! ```rust,ignore
//! use engine::local_copy::debug_send::{SendTracer, trace_send_file_start};
//!
//! let mut tracer = SendTracer::new();
//! tracer.start_file("file.txt", 1024, 0);
//!
//! trace_send_file_start("file.txt", 1024, 0);
//! trace_delta_match(5, 4096, 512);
//! trace_delta_literal(4608, 128);
//!
//! tracer.end_file("file.txt", 640);
//! ```

#![allow(dead_code)]

use std::time::{Duration, Instant};

/// Target name for tracing events, matching rsync's debug category.
const SEND_TARGET: &str = "rsync::send";

// ============================================================================
// Tracing functions (feature-gated)
// ============================================================================

/// Traces the start of a file send operation.
///
/// Emits a tracing span that tracks the start of sending a file's data.
/// In upstream rsync, this corresponds to the entry point of `send_files()`.
///
/// # Arguments
///
/// * `name` - Relative path of the file being sent
/// * `file_size` - Size of the file in bytes
/// * `index` - File list index for this file
#[cfg(feature = "tracing")]
#[inline]
pub fn trace_send_file_start(name: &str, file_size: u64, index: usize) {
    tracing::info!(
        target: SEND_TARGET,
        name = %name,
        file_size = file_size,
        index = index,
        "send_file: starting"
    );
}

/// No-op when tracing is disabled.
#[cfg(not(feature = "tracing"))]
#[inline]
pub fn trace_send_file_start(_name: &str, _file_size: u64, _index: usize) {}

/// Traces the completion of a file send operation.
///
/// Logs summary statistics for a completed file send, including total bytes
/// transmitted and time elapsed.
///
/// # Arguments
///
/// * `name` - Relative path of the file
/// * `bytes_sent` - Total bytes sent (including deltas and literals)
/// * `elapsed` - Time taken to send the file
#[cfg(feature = "tracing")]
#[inline]
pub fn trace_send_file_end(name: &str, bytes_sent: u64, elapsed: Duration) {
    tracing::info!(
        target: SEND_TARGET,
        name = %name,
        bytes_sent = bytes_sent,
        elapsed_ms = elapsed.as_millis(),
        "send_file: complete"
    );
}

/// No-op when tracing is disabled.
#[cfg(not(feature = "tracing"))]
#[inline]
pub fn trace_send_file_end(_name: &str, _bytes_sent: u64, _elapsed: Duration) {}

/// Traces the start of delta generation for a file.
///
/// Emits a trace event when beginning to compute deltas between basis and
/// target files. In upstream rsync, this corresponds to delta computation
/// in sender.c.
///
/// # Arguments
///
/// * `name` - Relative path of the file
/// * `basis_size` - Size of the basis file in bytes
/// * `target_size` - Size of the target file in bytes
#[cfg(feature = "tracing")]
#[inline]
pub fn trace_delta_start(name: &str, basis_size: u64, target_size: u64) {
    tracing::debug!(
        target: SEND_TARGET,
        name = %name,
        basis_size = basis_size,
        target_size = target_size,
        "delta: starting"
    );
}

/// No-op when tracing is disabled.
#[cfg(not(feature = "tracing"))]
#[inline]
pub fn trace_delta_start(_name: &str, _basis_size: u64, _target_size: u64) {}

/// Traces a block match event during delta generation.
///
/// Logs when a block from the target matches a block in the basis file,
/// allowing compression via reference rather than literal transmission.
///
/// # Arguments
///
/// * `block_index` - Index of the matched block in the basis file
/// * `offset` - Offset in the target file where the match occurs
/// * `length` - Length of the matched block in bytes
#[cfg(feature = "tracing")]
#[inline]
pub fn trace_delta_match(block_index: usize, offset: u64, length: u32) {
    tracing::trace!(
        target: SEND_TARGET,
        block_index = block_index,
        offset = offset,
        length = length,
        "delta: match"
    );
}

/// No-op when tracing is disabled.
#[cfg(not(feature = "tracing"))]
#[inline]
pub fn trace_delta_match(_block_index: usize, _offset: u64, _length: u32) {}

/// Traces a literal data event during delta generation.
///
/// Logs when literal data must be transmitted because no matching block
/// was found in the basis file.
///
/// # Arguments
///
/// * `offset` - Offset in the target file for the literal data
/// * `length` - Length of the literal data in bytes
#[cfg(feature = "tracing")]
#[inline]
pub fn trace_delta_literal(offset: u64, length: u32) {
    tracing::trace!(
        target: SEND_TARGET,
        offset = offset,
        length = length,
        "delta: literal"
    );
}

/// No-op when tracing is disabled.
#[cfg(not(feature = "tracing"))]
#[inline]
pub fn trace_delta_literal(_offset: u64, _length: u32) {}

/// Traces the completion of delta generation for a file.
///
/// Emits summary statistics showing how much data was matched versus
/// transmitted as literals.
///
/// # Arguments
///
/// * `name` - Relative path of the file
/// * `matched_bytes` - Total bytes matched from basis file
/// * `literal_bytes` - Total bytes sent as literals
/// * `elapsed` - Time taken to generate the delta
#[cfg(feature = "tracing")]
#[inline]
pub fn trace_delta_end(name: &str, matched_bytes: u64, literal_bytes: u64, elapsed: Duration) {
    tracing::debug!(
        target: SEND_TARGET,
        name = %name,
        matched_bytes = matched_bytes,
        literal_bytes = literal_bytes,
        elapsed_ms = elapsed.as_millis(),
        "delta: complete"
    );
}

/// No-op when tracing is disabled.
#[cfg(not(feature = "tracing"))]
#[inline]
pub fn trace_delta_end(_name: &str, _matched_bytes: u64, _literal_bytes: u64, _elapsed: Duration) {}

/// Traces checksum generation for a file.
///
/// Logs when generating rolling checksums for basis file blocks, which
/// is necessary before delta computation can begin.
///
/// # Arguments
///
/// * `name` - Relative path of the file
/// * `block_count` - Number of blocks to checksum
/// * `block_size` - Size of each block in bytes
#[cfg(feature = "tracing")]
#[inline]
pub fn trace_checksum_generation(name: &str, block_count: usize, block_size: u32) {
    tracing::debug!(
        target: SEND_TARGET,
        name = %name,
        block_count = block_count,
        block_size = block_size,
        "checksum: generating"
    );
}

/// No-op when tracing is disabled.
#[cfg(not(feature = "tracing"))]
#[inline]
pub fn trace_checksum_generation(_name: &str, _block_count: usize, _block_size: u32) {}

/// Traces a summary of all send operations.
///
/// Emits aggregate statistics for the entire send session, including total
/// files processed, bytes sent, and elapsed time.
///
/// # Arguments
///
/// * `total_files` - Total number of files sent
/// * `total_bytes` - Total bytes sent across all files
/// * `total_elapsed` - Total time elapsed for all send operations
#[cfg(feature = "tracing")]
#[inline]
pub fn trace_send_summary(total_files: usize, total_bytes: u64, total_elapsed: Duration) {
    tracing::info!(
        target: SEND_TARGET,
        total_files = total_files,
        total_bytes = total_bytes,
        elapsed_ms = total_elapsed.as_millis(),
        "send: summary"
    );
}

/// No-op when tracing is disabled.
#[cfg(not(feature = "tracing"))]
#[inline]
pub fn trace_send_summary(_total_files: usize, _total_bytes: u64, _total_elapsed: Duration) {}

// ============================================================================
// SendTracer - stateful tracer for aggregating send statistics
// ============================================================================

/// Aggregates statistics during file send operations.
///
/// Tracks file counts, byte counts, match/literal ratios, and timing information
/// across file send operations. Use this when you need to accumulate stats across
/// multiple files and delta operations before emitting final summary events.
///
/// # Examples
///
/// ```no_run
/// # use std::time::Duration;
/// # struct SendTracer { files_sent: usize, bytes_sent: u64, matched_bytes: u64, literal_bytes: u64 }
/// # impl SendTracer {
/// #     fn new() -> Self { Self { files_sent: 0, bytes_sent: 0, matched_bytes: 0, literal_bytes: 0 } }
/// #     fn start_file(&mut self, _name: &str, _size: u64, _index: usize) {}
/// #     fn record_match(&mut self, _block_index: usize, _offset: u64, length: u32) {
/// #         self.matched_bytes += length as u64;
/// #     }
/// #     fn record_literal(&mut self, _offset: u64, length: u32) {
/// #         self.literal_bytes += length as u64;
/// #     }
/// #     fn end_file(&mut self, _name: &str, bytes_sent: u64) {
/// #         self.files_sent += 1;
/// #         self.bytes_sent += bytes_sent;
/// #     }
/// #     fn summary(&mut self) -> Duration { Duration::ZERO }
/// #     fn files_sent(&self) -> usize { self.files_sent }
/// #     fn bytes_sent(&self) -> u64 { self.bytes_sent }
/// #     fn matched_bytes(&self) -> u64 { self.matched_bytes }
/// #     fn literal_bytes(&self) -> u64 { self.literal_bytes }
/// # }
/// let mut tracer = SendTracer::new();
/// tracer.start_file("file1.txt", 10240, 0);
/// tracer.record_match(5, 0, 4096);
/// tracer.record_literal(4096, 512);
/// tracer.end_file("file1.txt", 4608);
///
/// tracer.summary();
/// assert_eq!(tracer.files_sent(), 1);
/// assert_eq!(tracer.bytes_sent(), 4608);
/// ```
#[derive(Debug, Clone)]
pub struct SendTracer {
    files_sent: usize,
    bytes_sent: u64,
    matched_bytes: u64,
    literal_bytes: u64,
    current_file_start: Option<Instant>,
    session_start: Option<Instant>,
}

impl Default for SendTracer {
    fn default() -> Self {
        Self::new()
    }
}

impl SendTracer {
    /// Creates a new send tracer with zero counts.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            files_sent: 0,
            bytes_sent: 0,
            matched_bytes: 0,
            literal_bytes: 0,
            current_file_start: None,
            session_start: None,
        }
    }

    /// Starts tracking a file send operation.
    ///
    /// Records the start time for the current file and emits a trace event.
    ///
    /// # Arguments
    ///
    /// * `name` - Relative path of the file
    /// * `file_size` - Size of the file in bytes
    /// * `index` - File list index
    pub fn start_file(&mut self, name: &str, file_size: u64, index: usize) {
        if self.session_start.is_none() {
            self.session_start = Some(Instant::now());
        }
        self.current_file_start = Some(Instant::now());
        trace_send_file_start(name, file_size, index);
    }

    /// Records a block match event during delta generation.
    ///
    /// # Arguments
    ///
    /// * `block_index` - Index of the matched block
    /// * `offset` - Offset in the target file
    /// * `length` - Length of the matched block
    pub fn record_match(&mut self, block_index: usize, offset: u64, length: u32) {
        self.matched_bytes = self.matched_bytes.saturating_add(u64::from(length));
        trace_delta_match(block_index, offset, length);
    }

    /// Records a literal data event during delta generation.
    ///
    /// # Arguments
    ///
    /// * `offset` - Offset in the target file
    /// * `length` - Length of the literal data
    pub fn record_literal(&mut self, offset: u64, length: u32) {
        self.literal_bytes = self.literal_bytes.saturating_add(u64::from(length));
        trace_delta_literal(offset, length);
    }

    /// Ends tracking for the current file and emits a summary trace event.
    ///
    /// # Arguments
    ///
    /// * `name` - Relative path of the file
    /// * `bytes_sent` - Total bytes sent for this file
    pub fn end_file(&mut self, name: &str, bytes_sent: u64) {
        let elapsed = self.current_file_elapsed();
        self.files_sent += 1;
        self.bytes_sent = self.bytes_sent.saturating_add(bytes_sent);
        trace_send_file_end(name, bytes_sent, elapsed);
        self.current_file_start = None;
    }

    /// Emits a summary trace event for the entire send session.
    ///
    /// Returns the total elapsed time since the first file started.
    pub fn summary(&mut self) -> Duration {
        let elapsed = self.session_elapsed();
        trace_send_summary(self.files_sent, self.bytes_sent, elapsed);
        elapsed
    }

    /// Returns the number of files sent.
    #[must_use]
    pub const fn files_sent(&self) -> usize {
        self.files_sent
    }

    /// Returns the total bytes sent across all files.
    #[must_use]
    pub const fn bytes_sent(&self) -> u64 {
        self.bytes_sent
    }

    /// Returns the total bytes matched from basis files.
    #[must_use]
    pub const fn matched_bytes(&self) -> u64 {
        self.matched_bytes
    }

    /// Returns the total bytes sent as literals.
    #[must_use]
    pub const fn literal_bytes(&self) -> u64 {
        self.literal_bytes
    }

    /// Returns the elapsed time for the current file being sent.
    ///
    /// Returns `Duration::ZERO` if no file is currently being tracked.
    #[must_use]
    pub fn current_file_elapsed(&self) -> Duration {
        self.current_file_start
            .map_or(Duration::ZERO, |t| t.elapsed())
    }

    /// Returns the total elapsed time since the session started.
    ///
    /// Returns `Duration::ZERO` if no session has been started.
    #[must_use]
    pub fn session_elapsed(&self) -> Duration {
        self.session_start.map_or(Duration::ZERO, |t| t.elapsed())
    }

    /// Resets all counters and timing state to zero.
    pub fn reset(&mut self) {
        self.files_sent = 0;
        self.bytes_sent = 0;
        self.matched_bytes = 0;
        self.literal_bytes = 0;
        self.current_file_start = None;
        self.session_start = None;
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
        let tracer = SendTracer::new();
        assert_eq!(tracer.files_sent(), 0);
        assert_eq!(tracer.bytes_sent(), 0);
        assert_eq!(tracer.matched_bytes(), 0);
        assert_eq!(tracer.literal_bytes(), 0);
        assert_eq!(tracer.current_file_elapsed(), Duration::ZERO);
        assert_eq!(tracer.session_elapsed(), Duration::ZERO);
    }

    #[test]
    fn test_tracer_default() {
        let tracer = SendTracer::default();
        assert_eq!(tracer.files_sent(), 0);
        assert_eq!(tracer.bytes_sent(), 0);
    }

    #[test]
    fn test_start_file_initializes_timing() {
        let mut tracer = SendTracer::new();
        tracer.start_file("test.txt", 1024, 0);

        std::thread::sleep(Duration::from_millis(1));
        assert!(tracer.current_file_elapsed() > Duration::ZERO);
        assert!(tracer.session_elapsed() > Duration::ZERO);
    }

    #[test]
    fn test_record_match_accumulates() {
        let mut tracer = SendTracer::new();
        tracer.record_match(0, 0, 1024);
        tracer.record_match(1, 1024, 2048);
        tracer.record_match(2, 3072, 512);

        assert_eq!(tracer.matched_bytes(), 3584);
    }

    #[test]
    fn test_record_literal_accumulates() {
        let mut tracer = SendTracer::new();
        tracer.record_literal(0, 256);
        tracer.record_literal(256, 512);
        tracer.record_literal(768, 128);

        assert_eq!(tracer.literal_bytes(), 896);
    }

    #[test]
    fn test_end_file_increments_counts() {
        let mut tracer = SendTracer::new();
        tracer.start_file("file1.txt", 1024, 0);
        tracer.end_file("file1.txt", 512);

        assert_eq!(tracer.files_sent(), 1);
        assert_eq!(tracer.bytes_sent(), 512);
    }

    #[test]
    fn test_multiple_files() {
        let mut tracer = SendTracer::new();

        tracer.start_file("file1.txt", 2048, 0);
        tracer.record_match(0, 0, 1024);
        tracer.record_literal(1024, 256);
        tracer.end_file("file1.txt", 1280);

        tracer.start_file("file2.txt", 4096, 1);
        tracer.record_match(0, 0, 2048);
        tracer.record_literal(2048, 512);
        tracer.end_file("file2.txt", 2560);

        assert_eq!(tracer.files_sent(), 2);
        assert_eq!(tracer.bytes_sent(), 3840);
        assert_eq!(tracer.matched_bytes(), 3072);
        assert_eq!(tracer.literal_bytes(), 768);
    }

    #[test]
    fn test_reset_clears_state() {
        let mut tracer = SendTracer::new();
        tracer.start_file("test.txt", 1024, 0);
        tracer.record_match(0, 0, 512);
        tracer.record_literal(512, 256);
        tracer.end_file("test.txt", 768);

        tracer.reset();

        assert_eq!(tracer.files_sent(), 0);
        assert_eq!(tracer.bytes_sent(), 0);
        assert_eq!(tracer.matched_bytes(), 0);
        assert_eq!(tracer.literal_bytes(), 0);
        assert_eq!(tracer.current_file_elapsed(), Duration::ZERO);
        assert_eq!(tracer.session_elapsed(), Duration::ZERO);
    }

    #[test]
    fn test_summary_returns_elapsed() {
        let mut tracer = SendTracer::new();
        tracer.start_file("file.txt", 1024, 0);
        std::thread::sleep(Duration::from_millis(5));
        tracer.end_file("file.txt", 1024);

        let elapsed = tracer.summary();
        assert!(elapsed >= Duration::from_millis(5));
    }

    #[test]
    fn test_zero_size_file() {
        let mut tracer = SendTracer::new();
        tracer.start_file("empty.txt", 0, 0);
        tracer.end_file("empty.txt", 0);

        assert_eq!(tracer.files_sent(), 1);
        assert_eq!(tracer.bytes_sent(), 0);
    }

    #[test]
    fn test_saturating_add_bytes_sent() {
        let mut tracer = SendTracer::new();
        tracer.bytes_sent = u64::MAX - 100;
        tracer.start_file("huge.bin", 1024, 0);
        tracer.end_file("huge.bin", 200);

        assert_eq!(tracer.bytes_sent(), u64::MAX);
    }

    #[test]
    fn test_saturating_add_matched_bytes() {
        let mut tracer = SendTracer::new();
        tracer.matched_bytes = u64::MAX - 50;
        tracer.record_match(0, 0, 100);

        assert_eq!(tracer.matched_bytes(), u64::MAX);
    }

    #[test]
    fn test_saturating_add_literal_bytes() {
        let mut tracer = SendTracer::new();
        tracer.literal_bytes = u64::MAX - 50;
        tracer.record_literal(0, 100);

        assert_eq!(tracer.literal_bytes(), u64::MAX);
    }

    #[test]
    fn test_trace_functions_do_not_panic() {
        // All trace functions should be callable without panicking
        trace_send_file_start("test.txt", 1024, 0);
        trace_send_file_end("test.txt", 512, Duration::from_millis(100));
        trace_delta_start("test.txt", 2048, 1024);
        trace_delta_match(5, 4096, 512);
        trace_delta_literal(4608, 128);
        trace_delta_end("test.txt", 2048, 512, Duration::from_millis(50));
        trace_checksum_generation("test.txt", 100, 1024);
        trace_send_summary(10, 10240, Duration::from_secs(1));
    }

    #[test]
    fn test_end_file_without_start_file() {
        let mut tracer = SendTracer::new();
        tracer.end_file("test.txt", 1024);

        assert_eq!(tracer.files_sent(), 1);
        assert_eq!(tracer.bytes_sent(), 1024);
        assert_eq!(tracer.current_file_elapsed(), Duration::ZERO);
    }

    #[test]
    fn test_summary_without_files() {
        let mut tracer = SendTracer::new();
        let elapsed = tracer.summary();

        assert_eq!(tracer.files_sent(), 0);
        assert_eq!(tracer.bytes_sent(), 0);
        assert_eq!(elapsed, Duration::ZERO);
    }

    #[test]
    fn test_multiple_start_file_calls() {
        let mut tracer = SendTracer::new();
        tracer.start_file("file1.txt", 1024, 0);
        let first_start = tracer.current_file_start;

        std::thread::sleep(Duration::from_millis(1));
        tracer.start_file("file2.txt", 2048, 1);
        let second_start = tracer.current_file_start;

        // Should update the current file start time
        assert_ne!(first_start, second_start);
        // Session start should remain the same
        assert!(tracer.session_elapsed() > Duration::ZERO);
    }

    #[test]
    fn test_large_match_literal_counts() {
        let mut tracer = SendTracer::new();
        tracer.start_file("largefile.bin", 1_000_000, 0);

        for i in 0..1000 {
            tracer.record_match(i, (i as u64) * 1024, 1024);
        }

        for i in 0..500 {
            tracer.record_literal((i as u64) * 2048, 512);
        }

        tracer.end_file("largefile.bin", 1_280_000);

        assert_eq!(tracer.matched_bytes(), 1_024_000);
        assert_eq!(tracer.literal_bytes(), 256_000);
        assert_eq!(tracer.bytes_sent(), 1_280_000);
        assert_eq!(tracer.files_sent(), 1);
    }

    #[cfg(feature = "tracing")]
    #[test]
    fn test_tracing_feature_enabled() {
        // When tracing feature is enabled, verify the functions compile and run
        // without panicking. We can't easily verify event emission without
        // tracing-subscriber, but this at least confirms the code compiles.
        let mut tracer = SendTracer::new();
        tracer.start_file("traced.txt", 1024, 0);
        tracer.record_match(0, 0, 512);
        tracer.record_literal(512, 256);
        tracer.end_file("traced.txt", 768);

        // Verify the tracer still tracks stats correctly
        assert_eq!(tracer.files_sent(), 1);
        assert_eq!(tracer.bytes_sent(), 768);
        assert_eq!(tracer.matched_bytes(), 512);
        assert_eq!(tracer.literal_bytes(), 256);
    }
}
