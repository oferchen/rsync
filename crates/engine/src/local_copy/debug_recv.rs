//! DEBUG_RECV tracing for receiver operations.
//!
//! This module provides structured tracing for file receive/delta-apply operations
//! that match upstream rsync's receiver.c/generator.c debug output format. All tracing
//! is conditionally compiled behind the `tracing` feature flag and produces no-op
//! inline functions when disabled.
//!
//! # Examples
//!
//! ```rust,ignore
//! use engine::local_copy::debug_recv::{RecvTracer, trace_recv_file_start};
//!
//! let mut tracer = RecvTracer::new();
//! tracer.start_file("file.txt", 1024, 0);
//!
//! trace_recv_file_start("file.txt", 1024, 0);
//! trace_delta_apply_match(5, 4096, 512);
//! trace_delta_apply_literal(4608, 128);
//!
//! tracer.end_file("file.txt", 1024);
//! ```

use std::time::{Duration, Instant};

/// Target name for tracing events, matching rsync's debug category.
const RECV_TARGET: &str = "rsync::recv";

// ============================================================================
// Tracing functions (feature-gated)
// ============================================================================

/// Traces the start of a file receive operation.
///
/// Emits a tracing span that tracks the start of receiving a file's data.
/// In upstream rsync, this corresponds to the entry point of `recv_files()`.
///
/// # Arguments
///
/// * `name` - Relative path of the file being received
/// * `file_size` - Size of the file in bytes
/// * `index` - File list index for this file
#[cfg(feature = "tracing")]
#[inline]
pub fn trace_recv_file_start(name: &str, file_size: u64, index: usize) {
    tracing::info!(
        target: RECV_TARGET,
        name = %name,
        file_size = file_size,
        index = index,
        "recv_file: starting"
    );
}

/// No-op when tracing is disabled.
#[cfg(not(feature = "tracing"))]
#[inline]
pub fn trace_recv_file_start(_name: &str, _file_size: u64, _index: usize) {}

/// Traces the completion of a file receive operation.
///
/// Logs summary statistics for a completed file receive, including total bytes
/// received and time elapsed.
///
/// # Arguments
///
/// * `name` - Relative path of the file
/// * `bytes_received` - Total bytes received (including deltas and literals)
/// * `elapsed` - Time taken to receive the file
#[cfg(feature = "tracing")]
#[inline]
pub fn trace_recv_file_end(name: &str, bytes_received: u64, elapsed: Duration) {
    tracing::info!(
        target: RECV_TARGET,
        name = %name,
        bytes_received = bytes_received,
        elapsed_ms = elapsed.as_millis(),
        "recv_file: complete"
    );
}

/// No-op when tracing is disabled.
#[cfg(not(feature = "tracing"))]
#[inline]
pub fn trace_recv_file_end(_name: &str, _bytes_received: u64, _elapsed: Duration) {}

/// Traces basis file selection during receive.
///
/// Emits a trace event when a basis file is selected for delta reconstruction.
/// In upstream rsync, this corresponds to basis file selection in generator.c.
///
/// # Arguments
///
/// * `name` - Relative path of the target file
/// * `basis_path` - Path to the basis file being used
/// * `basis_size` - Size of the basis file in bytes
#[cfg(feature = "tracing")]
#[inline]
pub fn trace_basis_file_selected(name: &str, basis_path: &str, basis_size: u64) {
    tracing::debug!(
        target: RECV_TARGET,
        name = %name,
        basis_path = %basis_path,
        basis_size = basis_size,
        "basis: selected"
    );
}

/// No-op when tracing is disabled.
#[cfg(not(feature = "tracing"))]
#[inline]
pub fn trace_basis_file_selected(_name: &str, _basis_path: &str, _basis_size: u64) {}

/// Traces the start of delta application for a file.
///
/// Emits a trace event when beginning to apply deltas to reconstruct a file.
/// In upstream rsync, this corresponds to delta application in receiver.c.
///
/// # Arguments
///
/// * `name` - Relative path of the file
/// * `basis_size` - Size of the basis file in bytes
/// * `delta_size` - Size of the delta data in bytes
#[cfg(feature = "tracing")]
#[inline]
pub fn trace_delta_apply_start(name: &str, basis_size: u64, delta_size: u64) {
    tracing::debug!(
        target: RECV_TARGET,
        name = %name,
        basis_size = basis_size,
        delta_size = delta_size,
        "delta_apply: starting"
    );
}

/// No-op when tracing is disabled.
#[cfg(not(feature = "tracing"))]
#[inline]
pub fn trace_delta_apply_start(_name: &str, _basis_size: u64, _delta_size: u64) {}

/// Traces a block copy event during delta application.
///
/// Logs when a block is copied from the basis file to the output,
/// allowing reconstruction via reference rather than literal transmission.
///
/// # Arguments
///
/// * `block_index` - Index of the matched block in the basis file
/// * `offset` - Offset in the output file where the block is written
/// * `length` - Length of the block in bytes
#[cfg(feature = "tracing")]
#[inline]
pub fn trace_delta_apply_match(block_index: usize, offset: u64, length: u32) {
    tracing::trace!(
        target: RECV_TARGET,
        block_index = block_index,
        offset = offset,
        length = length,
        "delta_apply: match"
    );
}

/// No-op when tracing is disabled.
#[cfg(not(feature = "tracing"))]
#[inline]
pub fn trace_delta_apply_match(_block_index: usize, _offset: u64, _length: u32) {}

/// Traces a literal data event during delta application.
///
/// Logs when literal data is written to the output because no matching block
/// was found in the basis file.
///
/// # Arguments
///
/// * `offset` - Offset in the output file for the literal data
/// * `length` - Length of the literal data in bytes
#[cfg(feature = "tracing")]
#[inline]
pub fn trace_delta_apply_literal(offset: u64, length: u32) {
    tracing::trace!(
        target: RECV_TARGET,
        offset = offset,
        length = length,
        "delta_apply: literal"
    );
}

/// No-op when tracing is disabled.
#[cfg(not(feature = "tracing"))]
#[inline]
pub fn trace_delta_apply_literal(_offset: u64, _length: u32) {}

/// Traces the completion of delta application for a file.
///
/// Emits summary statistics showing the output size and time elapsed
/// during delta reconstruction.
///
/// # Arguments
///
/// * `name` - Relative path of the file
/// * `output_size` - Size of the reconstructed file in bytes
/// * `elapsed` - Time taken to apply the delta
#[cfg(feature = "tracing")]
#[inline]
pub fn trace_delta_apply_end(name: &str, output_size: u64, elapsed: Duration) {
    tracing::debug!(
        target: RECV_TARGET,
        name = %name,
        output_size = output_size,
        elapsed_ms = elapsed.as_millis(),
        "delta_apply: complete"
    );
}

/// No-op when tracing is disabled.
#[cfg(not(feature = "tracing"))]
#[inline]
pub fn trace_delta_apply_end(_name: &str, _output_size: u64, _elapsed: Duration) {}

/// Traces checksum verification for a received file.
///
/// Logs when verifying the checksum of a received file against the expected
/// checksum, which ensures data integrity during transfer.
///
/// # Arguments
///
/// * `name` - Relative path of the file
/// * `expected` - Expected checksum value
/// * `computed` - Computed checksum value
/// * `matched` - Whether the checksums matched
#[cfg(feature = "tracing")]
#[inline]
pub fn trace_checksum_verify(name: &str, expected: &[u8], computed: &[u8], matched: bool) {
    tracing::debug!(
        target: RECV_TARGET,
        name = %name,
        expected = format!("{:02x?}", expected),
        computed = format!("{:02x?}", computed),
        matched = matched,
        "checksum: verify"
    );
}

/// No-op when tracing is disabled.
#[cfg(not(feature = "tracing"))]
#[inline]
pub fn trace_checksum_verify(_name: &str, _expected: &[u8], _computed: &[u8], _matched: bool) {}

/// Traces a summary of all receive operations.
///
/// Emits aggregate statistics for the entire receive session, including total
/// files processed, bytes received, and elapsed time.
///
/// # Arguments
///
/// * `total_files` - Total number of files received
/// * `total_bytes` - Total bytes received across all files
/// * `total_elapsed` - Total time elapsed for all receive operations
#[cfg(feature = "tracing")]
#[inline]
pub fn trace_recv_summary(total_files: usize, total_bytes: u64, total_elapsed: Duration) {
    tracing::info!(
        target: RECV_TARGET,
        total_files = total_files,
        total_bytes = total_bytes,
        elapsed_ms = total_elapsed.as_millis(),
        "recv: summary"
    );
}

/// No-op when tracing is disabled.
#[cfg(not(feature = "tracing"))]
#[inline]
pub fn trace_recv_summary(_total_files: usize, _total_bytes: u64, _total_elapsed: Duration) {}

// ============================================================================
// RecvTracer - stateful tracer for aggregating receive statistics
// ============================================================================

/// Aggregates statistics during file receive operations.
///
/// Tracks file counts, byte counts, basis selections, checksum verifications,
/// match/literal ratios, and timing information across file receive operations.
/// Use this when you need to accumulate stats across multiple files and delta
/// operations before emitting final summary events.
///
/// # Examples
///
/// ```no_run
/// # use std::time::Duration;
/// # struct RecvTracer { files_received: usize, bytes_received: u64, checksum_matches: usize }
/// # impl RecvTracer {
/// #     fn new() -> Self { Self { files_received: 0, bytes_received: 0, checksum_matches: 0 } }
/// #     fn start_file(&mut self, _name: &str, _size: u64, _index: usize) {}
/// #     fn record_basis(&mut self, _name: &str, _basis_path: &str, _basis_size: u64) {}
/// #     fn record_match(&mut self, _block_index: usize, _offset: u64, length: u32) {}
/// #     fn record_literal(&mut self, _offset: u64, length: u32) {}
/// #     fn record_checksum_verify(&mut self, matched: bool) {
/// #         if matched { self.checksum_matches += 1; }
/// #     }
/// #     fn end_file(&mut self, _name: &str, bytes_received: u64) {
/// #         self.files_received += 1;
/// #         self.bytes_received += bytes_received;
/// #     }
/// #     fn summary(&mut self) -> Duration { Duration::ZERO }
/// #     fn files_received(&self) -> usize { self.files_received }
/// #     fn bytes_received(&self) -> u64 { self.bytes_received }
/// #     fn checksum_matches(&self) -> usize { self.checksum_matches }
/// # }
/// let mut tracer = RecvTracer::new();
/// tracer.start_file("file1.txt", 10240, 0);
/// tracer.record_basis("file1.txt", "/basis/file1.txt", 8192);
/// tracer.record_match(5, 0, 4096);
/// tracer.record_literal(4096, 512);
/// tracer.record_checksum_verify(true);
/// tracer.end_file("file1.txt", 4608);
///
/// tracer.summary();
/// assert_eq!(tracer.files_received(), 1);
/// assert_eq!(tracer.bytes_received(), 4608);
/// assert_eq!(tracer.checksum_matches(), 1);
/// ```
#[derive(Debug, Clone)]
pub struct RecvTracer {
    files_received: usize,
    bytes_received: u64,
    matched_bytes: u64,
    literal_bytes: u64,
    basis_selections: usize,
    checksum_matches: usize,
    checksum_mismatches: usize,
    current_file_start: Option<Instant>,
    session_start: Option<Instant>,
}

impl Default for RecvTracer {
    fn default() -> Self {
        Self::new()
    }
}

impl RecvTracer {
    /// Creates a new receive tracer with zero counts.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            files_received: 0,
            bytes_received: 0,
            matched_bytes: 0,
            literal_bytes: 0,
            basis_selections: 0,
            checksum_matches: 0,
            checksum_mismatches: 0,
            current_file_start: None,
            session_start: None,
        }
    }

    /// Starts tracking a file receive operation.
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
        trace_recv_file_start(name, file_size, index);
    }

    /// Records a basis file selection event.
    ///
    /// # Arguments
    ///
    /// * `name` - Relative path of the target file
    /// * `basis_path` - Path to the basis file
    /// * `basis_size` - Size of the basis file in bytes
    pub fn record_basis(&mut self, name: &str, basis_path: &str, basis_size: u64) {
        self.basis_selections += 1;
        trace_basis_file_selected(name, basis_path, basis_size);
    }

    /// Records a block match event during delta application.
    ///
    /// # Arguments
    ///
    /// * `block_index` - Index of the matched block
    /// * `offset` - Offset in the output file
    /// * `length` - Length of the matched block
    pub fn record_match(&mut self, block_index: usize, offset: u64, length: u32) {
        self.matched_bytes = self.matched_bytes.saturating_add(u64::from(length));
        trace_delta_apply_match(block_index, offset, length);
    }

    /// Records a literal data event during delta application.
    ///
    /// # Arguments
    ///
    /// * `offset` - Offset in the output file
    /// * `length` - Length of the literal data
    pub fn record_literal(&mut self, offset: u64, length: u32) {
        self.literal_bytes = self.literal_bytes.saturating_add(u64::from(length));
        trace_delta_apply_literal(offset, length);
    }

    /// Records a checksum verification event.
    ///
    /// # Arguments
    ///
    /// * `matched` - Whether the checksum matched
    pub fn record_checksum_verify(&mut self, matched: bool) {
        if matched {
            self.checksum_matches += 1;
        } else {
            self.checksum_mismatches += 1;
        }
    }

    /// Ends tracking for the current file and emits a summary trace event.
    ///
    /// # Arguments
    ///
    /// * `name` - Relative path of the file
    /// * `bytes_received` - Total bytes received for this file
    pub fn end_file(&mut self, name: &str, bytes_received: u64) {
        let elapsed = self.current_file_elapsed();
        self.files_received += 1;
        self.bytes_received = self.bytes_received.saturating_add(bytes_received);
        trace_recv_file_end(name, bytes_received, elapsed);
        self.current_file_start = None;
    }

    /// Emits a summary trace event for the entire receive session.
    ///
    /// Returns the total elapsed time since the first file started.
    pub fn summary(&mut self) -> Duration {
        let elapsed = self.session_elapsed();
        trace_recv_summary(self.files_received, self.bytes_received, elapsed);
        elapsed
    }

    /// Returns the number of files received.
    #[must_use]
    pub const fn files_received(&self) -> usize {
        self.files_received
    }

    /// Returns the total bytes received across all files.
    #[must_use]
    pub const fn bytes_received(&self) -> u64 {
        self.bytes_received
    }

    /// Returns the total bytes matched from basis files.
    #[must_use]
    pub const fn matched_bytes(&self) -> u64 {
        self.matched_bytes
    }

    /// Returns the total bytes received as literals.
    #[must_use]
    pub const fn literal_bytes(&self) -> u64 {
        self.literal_bytes
    }

    /// Returns the number of basis file selections.
    #[must_use]
    pub const fn basis_selections(&self) -> usize {
        self.basis_selections
    }

    /// Returns the number of checksum matches.
    #[must_use]
    pub const fn checksum_matches(&self) -> usize {
        self.checksum_matches
    }

    /// Returns the number of checksum mismatches.
    #[must_use]
    pub const fn checksum_mismatches(&self) -> usize {
        self.checksum_mismatches
    }

    /// Returns the elapsed time for the current file being received.
    ///
    /// Returns `Duration::ZERO` if no file is currently being tracked.
    #[must_use]
    pub fn current_file_elapsed(&self) -> Duration {
        self.current_file_start.map_or(Duration::ZERO, |t| t.elapsed())
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
        self.files_received = 0;
        self.bytes_received = 0;
        self.matched_bytes = 0;
        self.literal_bytes = 0;
        self.basis_selections = 0;
        self.checksum_matches = 0;
        self.checksum_mismatches = 0;
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
        let tracer = RecvTracer::new();
        assert_eq!(tracer.files_received(), 0);
        assert_eq!(tracer.bytes_received(), 0);
        assert_eq!(tracer.matched_bytes(), 0);
        assert_eq!(tracer.literal_bytes(), 0);
        assert_eq!(tracer.basis_selections(), 0);
        assert_eq!(tracer.checksum_matches(), 0);
        assert_eq!(tracer.checksum_mismatches(), 0);
        assert_eq!(tracer.current_file_elapsed(), Duration::ZERO);
        assert_eq!(tracer.session_elapsed(), Duration::ZERO);
    }

    #[test]
    fn test_tracer_default() {
        let tracer = RecvTracer::default();
        assert_eq!(tracer.files_received(), 0);
        assert_eq!(tracer.bytes_received(), 0);
    }

    #[test]
    fn test_start_file_initializes_timing() {
        let mut tracer = RecvTracer::new();
        tracer.start_file("test.txt", 1024, 0);

        std::thread::sleep(Duration::from_millis(1));
        assert!(tracer.current_file_elapsed() > Duration::ZERO);
        assert!(tracer.session_elapsed() > Duration::ZERO);
    }

    #[test]
    fn test_record_match_accumulates() {
        let mut tracer = RecvTracer::new();
        tracer.record_match(0, 0, 1024);
        tracer.record_match(1, 1024, 2048);
        tracer.record_match(2, 3072, 512);

        assert_eq!(tracer.matched_bytes(), 3584);
    }

    #[test]
    fn test_record_literal_accumulates() {
        let mut tracer = RecvTracer::new();
        tracer.record_literal(0, 256);
        tracer.record_literal(256, 512);
        tracer.record_literal(768, 128);

        assert_eq!(tracer.literal_bytes(), 896);
    }

    #[test]
    fn test_record_basis_increments() {
        let mut tracer = RecvTracer::new();
        tracer.record_basis("file1.txt", "/basis/file1.txt", 1024);
        tracer.record_basis("file2.txt", "/basis/file2.txt", 2048);

        assert_eq!(tracer.basis_selections(), 2);
    }

    #[test]
    fn test_record_checksum_verify_matches() {
        let mut tracer = RecvTracer::new();
        tracer.record_checksum_verify(true);
        tracer.record_checksum_verify(true);

        assert_eq!(tracer.checksum_matches(), 2);
        assert_eq!(tracer.checksum_mismatches(), 0);
    }

    #[test]
    fn test_record_checksum_verify_mismatches() {
        let mut tracer = RecvTracer::new();
        tracer.record_checksum_verify(false);
        tracer.record_checksum_verify(false);

        assert_eq!(tracer.checksum_matches(), 0);
        assert_eq!(tracer.checksum_mismatches(), 2);
    }

    #[test]
    fn test_record_checksum_verify_mixed() {
        let mut tracer = RecvTracer::new();
        tracer.record_checksum_verify(true);
        tracer.record_checksum_verify(false);
        tracer.record_checksum_verify(true);

        assert_eq!(tracer.checksum_matches(), 2);
        assert_eq!(tracer.checksum_mismatches(), 1);
    }

    #[test]
    fn test_end_file_increments_counts() {
        let mut tracer = RecvTracer::new();
        tracer.start_file("file1.txt", 1024, 0);
        tracer.end_file("file1.txt", 512);

        assert_eq!(tracer.files_received(), 1);
        assert_eq!(tracer.bytes_received(), 512);
    }

    #[test]
    fn test_multiple_files() {
        let mut tracer = RecvTracer::new();

        tracer.start_file("file1.txt", 2048, 0);
        tracer.record_basis("file1.txt", "/basis/file1.txt", 1024);
        tracer.record_match(0, 0, 1024);
        tracer.record_literal(1024, 256);
        tracer.record_checksum_verify(true);
        tracer.end_file("file1.txt", 1280);

        tracer.start_file("file2.txt", 4096, 1);
        tracer.record_basis("file2.txt", "/basis/file2.txt", 2048);
        tracer.record_match(0, 0, 2048);
        tracer.record_literal(2048, 512);
        tracer.record_checksum_verify(true);
        tracer.end_file("file2.txt", 2560);

        assert_eq!(tracer.files_received(), 2);
        assert_eq!(tracer.bytes_received(), 3840);
        assert_eq!(tracer.matched_bytes(), 3072);
        assert_eq!(tracer.literal_bytes(), 768);
        assert_eq!(tracer.basis_selections(), 2);
        assert_eq!(tracer.checksum_matches(), 2);
    }

    #[test]
    fn test_reset_clears_state() {
        let mut tracer = RecvTracer::new();
        tracer.start_file("test.txt", 1024, 0);
        tracer.record_basis("test.txt", "/basis/test.txt", 512);
        tracer.record_match(0, 0, 512);
        tracer.record_literal(512, 256);
        tracer.record_checksum_verify(true);
        tracer.end_file("test.txt", 768);

        tracer.reset();

        assert_eq!(tracer.files_received(), 0);
        assert_eq!(tracer.bytes_received(), 0);
        assert_eq!(tracer.matched_bytes(), 0);
        assert_eq!(tracer.literal_bytes(), 0);
        assert_eq!(tracer.basis_selections(), 0);
        assert_eq!(tracer.checksum_matches(), 0);
        assert_eq!(tracer.checksum_mismatches(), 0);
        assert_eq!(tracer.current_file_elapsed(), Duration::ZERO);
        assert_eq!(tracer.session_elapsed(), Duration::ZERO);
    }

    #[test]
    fn test_summary_returns_elapsed() {
        let mut tracer = RecvTracer::new();
        tracer.start_file("file.txt", 1024, 0);
        std::thread::sleep(Duration::from_millis(5));
        tracer.end_file("file.txt", 1024);

        let elapsed = tracer.summary();
        assert!(elapsed >= Duration::from_millis(5));
    }

    #[test]
    fn test_zero_size_file() {
        let mut tracer = RecvTracer::new();
        tracer.start_file("empty.txt", 0, 0);
        tracer.end_file("empty.txt", 0);

        assert_eq!(tracer.files_received(), 1);
        assert_eq!(tracer.bytes_received(), 0);
    }

    #[test]
    fn test_saturating_add_bytes_received() {
        let mut tracer = RecvTracer::new();
        tracer.bytes_received = u64::MAX - 100;
        tracer.start_file("huge.bin", 1024, 0);
        tracer.end_file("huge.bin", 200);

        assert_eq!(tracer.bytes_received(), u64::MAX);
    }

    #[test]
    fn test_saturating_add_matched_bytes() {
        let mut tracer = RecvTracer::new();
        tracer.matched_bytes = u64::MAX - 50;
        tracer.record_match(0, 0, 100);

        assert_eq!(tracer.matched_bytes(), u64::MAX);
    }

    #[test]
    fn test_saturating_add_literal_bytes() {
        let mut tracer = RecvTracer::new();
        tracer.literal_bytes = u64::MAX - 50;
        tracer.record_literal(0, 100);

        assert_eq!(tracer.literal_bytes(), u64::MAX);
    }

    #[test]
    fn test_trace_functions_do_not_panic() {
        // All trace functions should be callable without panicking
        trace_recv_file_start("test.txt", 1024, 0);
        trace_recv_file_end("test.txt", 512, Duration::from_millis(100));
        trace_basis_file_selected("test.txt", "/basis/test.txt", 1024);
        trace_delta_apply_start("test.txt", 2048, 1024);
        trace_delta_apply_match(5, 4096, 512);
        trace_delta_apply_literal(4608, 128);
        trace_delta_apply_end("test.txt", 2048, Duration::from_millis(50));
        trace_checksum_verify("test.txt", &[0x12, 0x34], &[0x12, 0x34], true);
        trace_recv_summary(10, 10240, Duration::from_secs(1));
    }

    #[test]
    fn test_end_file_without_start_file() {
        let mut tracer = RecvTracer::new();
        tracer.end_file("test.txt", 1024);

        assert_eq!(tracer.files_received(), 1);
        assert_eq!(tracer.bytes_received(), 1024);
        assert_eq!(tracer.current_file_elapsed(), Duration::ZERO);
    }

    #[test]
    fn test_summary_without_files() {
        let mut tracer = RecvTracer::new();
        let elapsed = tracer.summary();

        assert_eq!(tracer.files_received(), 0);
        assert_eq!(tracer.bytes_received(), 0);
        assert_eq!(elapsed, Duration::ZERO);
    }

    #[test]
    fn test_multiple_start_file_calls() {
        let mut tracer = RecvTracer::new();
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
        let mut tracer = RecvTracer::new();
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
        assert_eq!(tracer.bytes_received(), 1_280_000);
        assert_eq!(tracer.files_received(), 1);
    }

    #[test]
    fn test_empty_transfer() {
        let mut tracer = RecvTracer::new();
        let elapsed = tracer.summary();

        assert_eq!(tracer.files_received(), 0);
        assert_eq!(tracer.bytes_received(), 0);
        assert_eq!(tracer.matched_bytes(), 0);
        assert_eq!(tracer.literal_bytes(), 0);
        assert_eq!(elapsed, Duration::ZERO);
    }

    #[test]
    fn test_file_without_basis() {
        let mut tracer = RecvTracer::new();
        tracer.start_file("newfile.txt", 1024, 0);
        tracer.record_literal(0, 1024);
        tracer.record_checksum_verify(true);
        tracer.end_file("newfile.txt", 1024);

        assert_eq!(tracer.files_received(), 1);
        assert_eq!(tracer.bytes_received(), 1024);
        assert_eq!(tracer.matched_bytes(), 0);
        assert_eq!(tracer.literal_bytes(), 1024);
        assert_eq!(tracer.basis_selections(), 0);
    }

    #[cfg(feature = "tracing")]
    #[test]
    fn test_tracing_feature_enabled() {
        // When tracing feature is enabled, verify the functions compile and run
        // without panicking. We can't easily verify event emission without
        // tracing-subscriber, but this at least confirms the code compiles.
        let mut tracer = RecvTracer::new();
        tracer.start_file("traced.txt", 1024, 0);
        tracer.record_basis("traced.txt", "/basis/traced.txt", 512);
        tracer.record_match(0, 0, 512);
        tracer.record_literal(512, 256);
        tracer.record_checksum_verify(true);
        tracer.end_file("traced.txt", 768);

        // Verify the tracer still tracks stats correctly
        assert_eq!(tracer.files_received(), 1);
        assert_eq!(tracer.bytes_received(), 768);
        assert_eq!(tracer.matched_bytes(), 512);
        assert_eq!(tracer.literal_bytes(), 256);
        assert_eq!(tracer.checksum_matches(), 1);
    }
}
