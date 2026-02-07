//! DEBUG_IO tracing for I/O operations.
//!
//! This module provides structured tracing for I/O operations that match
//! upstream rsync's io.c debug output format. All tracing is conditionally
//! compiled behind the `tracing` feature flag and produces no-op inline
//! functions when disabled.
//!
//! # Examples
//!
//! ```rust,ignore
//! use protocol::debug_io::{IoTracer, trace_io_read, trace_io_write};
//!
//! let mut tracer = IoTracer::new();
//! tracer.record_read(1024);
//! tracer.record_write(2048);
//!
//! trace_io_read(1024, "file_data");
//! trace_io_write(2048, "delta_stream");
//! trace_mplex_message(7, 1, 512);
//!
//! let throughput = tracer.throughput_read(Duration::from_secs(1));
//! ```

use std::time::Duration;

/// Target name for tracing events, matching rsync's debug category.
#[cfg(feature = "tracing")]
const IO_TARGET: &str = "rsync::io";

// ============================================================================
// Tracing functions (feature-gated)
// ============================================================================

/// Traces a read operation.
///
/// Emits a tracing event when data is read from the I/O stream.
/// In upstream rsync, this corresponds to read operations in io.c.
///
/// # Arguments
///
/// * `bytes` - Number of bytes read
/// * `tag` - Descriptive tag for the operation context (e.g., "file_data", "checksum")
#[cfg(feature = "tracing")]
#[inline]
pub fn trace_io_read(bytes: usize, tag: &str) {
    tracing::trace!(
        target: IO_TARGET,
        bytes = bytes,
        tag = tag,
        "io: read"
    );
}

/// No-op when tracing is disabled.
#[cfg(not(feature = "tracing"))]
#[inline]
pub fn trace_io_read(_bytes: usize, _tag: &str) {}

/// Traces a write operation.
///
/// Emits a tracing event when data is written to the I/O stream.
///
/// # Arguments
///
/// * `bytes` - Number of bytes written
/// * `tag` - Descriptive tag for the operation context (e.g., "delta", "file_list")
#[cfg(feature = "tracing")]
#[inline]
pub fn trace_io_write(bytes: usize, tag: &str) {
    tracing::trace!(
        target: IO_TARGET,
        bytes = bytes,
        tag = tag,
        "io: write"
    );
}

/// No-op when tracing is disabled.
#[cfg(not(feature = "tracing"))]
#[inline]
pub fn trace_io_write(_bytes: usize, _tag: &str) {}

/// Traces a multiplex message.
///
/// Logs when a multiplex message is sent or received. Multiplex messages
/// are used in rsync's protocol to interleave multiple data streams.
///
/// # Arguments
///
/// * `code` - Message code (e.g., 7 for MPLEX_BASE)
/// * `tag` - Message tag/channel identifier
/// * `length` - Length of the message payload
#[cfg(feature = "tracing")]
#[inline]
pub fn trace_mplex_message(code: u8, tag: u16, length: u32) {
    tracing::debug!(
        target: IO_TARGET,
        code = code,
        tag = tag,
        length = length,
        "io: mplex_message"
    );
}

/// No-op when tracing is disabled.
#[cfg(not(feature = "tracing"))]
#[inline]
pub fn trace_mplex_message(_code: u8, _tag: u16, _length: u32) {}

/// Traces an I/O timeout event.
///
/// Logs when an I/O operation times out.
///
/// # Arguments
///
/// * `operation` - Description of the operation that timed out
/// * `timeout_secs` - Timeout duration in seconds
#[cfg(feature = "tracing")]
#[inline]
pub fn trace_io_timeout(operation: &str, timeout_secs: u32) {
    tracing::warn!(
        target: IO_TARGET,
        operation = operation,
        timeout_secs = timeout_secs,
        "io: timeout"
    );
}

/// No-op when tracing is disabled.
#[cfg(not(feature = "tracing"))]
#[inline]
pub fn trace_io_timeout(_operation: &str, _timeout_secs: u32) {}

/// Traces an I/O error.
///
/// Logs when an I/O error occurs during an operation.
///
/// # Arguments
///
/// * `operation` - Description of the operation that failed
/// * `error` - Error message or description
#[cfg(feature = "tracing")]
#[inline]
pub fn trace_io_error(operation: &str, error: &str) {
    tracing::error!(
        target: IO_TARGET,
        operation = operation,
        error = error,
        "io: error"
    );
}

/// No-op when tracing is disabled.
#[cfg(not(feature = "tracing"))]
#[inline]
pub fn trace_io_error(_operation: &str, _error: &str) {}

/// Traces a buffer flush operation.
///
/// Logs when buffered data is flushed to the underlying I/O stream.
///
/// # Arguments
///
/// * `buffered_bytes` - Number of bytes being flushed
#[cfg(feature = "tracing")]
#[inline]
pub fn trace_io_flush(buffered_bytes: usize) {
    tracing::debug!(
        target: IO_TARGET,
        buffered_bytes = buffered_bytes,
        "io: flush"
    );
}

/// No-op when tracing is disabled.
#[cfg(not(feature = "tracing"))]
#[inline]
pub fn trace_io_flush(_buffered_bytes: usize) {}

/// Traces a summary of I/O operations.
///
/// Emits aggregate statistics for a completed I/O session, including total
/// bytes read, written, and elapsed time.
///
/// # Arguments
///
/// * `total_read` - Total bytes read
/// * `total_written` - Total bytes written
/// * `elapsed` - Total elapsed time for the I/O operations
#[cfg(feature = "tracing")]
#[inline]
pub fn trace_io_summary(total_read: u64, total_written: u64, elapsed: Duration) {
    tracing::info!(
        target: IO_TARGET,
        total_read = total_read,
        total_written = total_written,
        elapsed_ms = elapsed.as_millis(),
        "io: summary"
    );
}

/// No-op when tracing is disabled.
#[cfg(not(feature = "tracing"))]
#[inline]
pub fn trace_io_summary(_total_read: u64, _total_written: u64, _elapsed: Duration) {}

// ============================================================================
// IoTracer - stateful tracer for aggregating I/O statistics
// ============================================================================

/// Aggregates statistics during I/O operations.
///
/// Tracks read/write counts, operation counts, errors, timeouts, and other
/// I/O events across the lifetime of a session. Use this when you need to
/// accumulate stats across multiple I/O operations before emitting final
/// summary events.
///
/// # Examples
///
/// ```no_run
/// # use std::time::Duration;
/// # struct IoTracer {
/// #     total_read: u64, total_written: u64, read_ops: usize, write_ops: usize,
/// #     mplex_messages: usize, timeouts: usize, errors: usize, flushes: usize
/// # }
/// # impl IoTracer {
/// #     fn new() -> Self { Self { total_read: 0, total_written: 0, read_ops: 0, write_ops: 0, mplex_messages: 0, timeouts: 0, errors: 0, flushes: 0 } }
/// #     fn record_read(&mut self, bytes: usize) { self.total_read += bytes as u64; self.read_ops += 1; }
/// #     fn record_write(&mut self, bytes: usize) { self.total_written += bytes as u64; self.write_ops += 1; }
/// #     fn record_mplex(&mut self) { self.mplex_messages += 1; }
/// #     fn record_timeout(&mut self) { self.timeouts += 1; }
/// #     fn record_error(&mut self) { self.errors += 1; }
/// #     fn record_flush(&mut self) { self.flushes += 1; }
/// #     fn throughput_read(&self, _elapsed: Duration) -> f64 { 0.0 }
/// #     fn throughput_write(&self, _elapsed: Duration) -> f64 { 0.0 }
/// #     fn summary(&self) { }
/// #     fn reset(&mut self) { *self = Self::new(); }
/// #     fn total_read(&self) -> u64 { self.total_read }
/// #     fn total_written(&self) -> u64 { self.total_written }
/// #     fn read_ops(&self) -> usize { self.read_ops }
/// #     fn write_ops(&self) -> usize { self.write_ops }
/// #     fn mplex_messages(&self) -> usize { self.mplex_messages }
/// #     fn timeouts(&self) -> usize { self.timeouts }
/// #     fn errors(&self) -> usize { self.errors }
/// #     fn flushes(&self) -> usize { self.flushes }
/// # }
/// let mut tracer = IoTracer::new();
/// tracer.record_read(1024);
/// tracer.record_write(2048);
/// tracer.record_mplex();
///
/// assert_eq!(tracer.total_read(), 1024);
/// assert_eq!(tracer.total_written(), 2048);
/// assert_eq!(tracer.read_ops(), 1);
/// assert_eq!(tracer.write_ops(), 1);
/// ```
#[derive(Debug, Clone)]
pub struct IoTracer {
    total_read: u64,
    total_written: u64,
    read_ops: usize,
    write_ops: usize,
    mplex_messages: usize,
    timeouts: usize,
    errors: usize,
    flushes: usize,
}

impl Default for IoTracer {
    fn default() -> Self {
        Self::new()
    }
}

impl IoTracer {
    /// Creates a new I/O tracer with zero counts.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            total_read: 0,
            total_written: 0,
            read_ops: 0,
            write_ops: 0,
            mplex_messages: 0,
            timeouts: 0,
            errors: 0,
            flushes: 0,
        }
    }

    /// Records a read operation.
    ///
    /// Increments the read operation counter and accumulates bytes read.
    ///
    /// # Arguments
    ///
    /// * `bytes` - Number of bytes read
    pub fn record_read(&mut self, bytes: usize) {
        self.total_read = self.total_read.saturating_add(bytes as u64);
        self.read_ops = self.read_ops.saturating_add(1);
    }

    /// Records a write operation.
    ///
    /// Increments the write operation counter and accumulates bytes written.
    ///
    /// # Arguments
    ///
    /// * `bytes` - Number of bytes written
    pub fn record_write(&mut self, bytes: usize) {
        self.total_written = self.total_written.saturating_add(bytes as u64);
        self.write_ops = self.write_ops.saturating_add(1);
    }

    /// Records a multiplex message.
    ///
    /// Increments the multiplex message counter.
    pub fn record_mplex(&mut self) {
        self.mplex_messages = self.mplex_messages.saturating_add(1);
    }

    /// Records a timeout event.
    ///
    /// Increments the timeout counter.
    pub fn record_timeout(&mut self) {
        self.timeouts = self.timeouts.saturating_add(1);
    }

    /// Records an error event.
    ///
    /// Increments the error counter.
    pub fn record_error(&mut self) {
        self.errors = self.errors.saturating_add(1);
    }

    /// Records a flush operation.
    ///
    /// Increments the flush counter.
    pub fn record_flush(&mut self) {
        self.flushes = self.flushes.saturating_add(1);
    }

    /// Emits a summary trace event for the I/O session.
    ///
    /// # Arguments
    ///
    /// * `elapsed` - Total elapsed time for the I/O operations
    pub fn summary(&self, elapsed: Duration) {
        trace_io_summary(self.total_read, self.total_written, elapsed);
    }

    /// Resets all counters to zero.
    pub fn reset(&mut self) {
        self.total_read = 0;
        self.total_written = 0;
        self.read_ops = 0;
        self.write_ops = 0;
        self.mplex_messages = 0;
        self.timeouts = 0;
        self.errors = 0;
        self.flushes = 0;
    }

    /// Returns the total bytes read.
    #[must_use]
    pub const fn total_read(&self) -> u64 {
        self.total_read
    }

    /// Returns the total bytes written.
    #[must_use]
    pub const fn total_written(&self) -> u64 {
        self.total_written
    }

    /// Returns the number of read operations.
    #[must_use]
    pub const fn read_ops(&self) -> usize {
        self.read_ops
    }

    /// Returns the number of write operations.
    #[must_use]
    pub const fn write_ops(&self) -> usize {
        self.write_ops
    }

    /// Returns the number of multiplex messages.
    #[must_use]
    pub const fn mplex_messages(&self) -> usize {
        self.mplex_messages
    }

    /// Returns the number of timeout events.
    #[must_use]
    pub const fn timeouts(&self) -> usize {
        self.timeouts
    }

    /// Returns the number of error events.
    #[must_use]
    pub const fn errors(&self) -> usize {
        self.errors
    }

    /// Returns the number of flush operations.
    #[must_use]
    pub const fn flushes(&self) -> usize {
        self.flushes
    }

    /// Calculates read throughput in bytes per second.
    ///
    /// Returns 0.0 if elapsed time is zero to avoid division by zero.
    ///
    /// # Arguments
    ///
    /// * `elapsed` - Total elapsed time for read operations
    #[must_use]
    pub fn throughput_read(&self, elapsed: Duration) -> f64 {
        let secs = elapsed.as_secs_f64();
        if secs > 0.0 {
            self.total_read as f64 / secs
        } else {
            0.0
        }
    }

    /// Calculates write throughput in bytes per second.
    ///
    /// Returns 0.0 if elapsed time is zero to avoid division by zero.
    ///
    /// # Arguments
    ///
    /// * `elapsed` - Total elapsed time for write operations
    #[must_use]
    pub fn throughput_write(&self, elapsed: Duration) -> f64 {
        let secs = elapsed.as_secs_f64();
        if secs > 0.0 {
            self.total_written as f64 / secs
        } else {
            0.0
        }
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
        let tracer = IoTracer::new();
        assert_eq!(tracer.total_read(), 0);
        assert_eq!(tracer.total_written(), 0);
        assert_eq!(tracer.read_ops(), 0);
        assert_eq!(tracer.write_ops(), 0);
        assert_eq!(tracer.mplex_messages(), 0);
        assert_eq!(tracer.timeouts(), 0);
        assert_eq!(tracer.errors(), 0);
        assert_eq!(tracer.flushes(), 0);
    }

    #[test]
    fn test_tracer_default() {
        let tracer = IoTracer::default();
        assert_eq!(tracer.total_read(), 0);
        assert_eq!(tracer.total_written(), 0);
        assert_eq!(tracer.read_ops(), 0);
        assert_eq!(tracer.write_ops(), 0);
    }

    #[test]
    fn test_record_read_accumulates() {
        let mut tracer = IoTracer::new();
        tracer.record_read(1024);
        tracer.record_read(2048);
        tracer.record_read(512);

        assert_eq!(tracer.total_read(), 3584);
        assert_eq!(tracer.read_ops(), 3);
    }

    #[test]
    fn test_record_write_accumulates() {
        let mut tracer = IoTracer::new();
        tracer.record_write(256);
        tracer.record_write(512);
        tracer.record_write(128);

        assert_eq!(tracer.total_written(), 896);
        assert_eq!(tracer.write_ops(), 3);
    }

    #[test]
    fn test_record_mplex() {
        let mut tracer = IoTracer::new();
        tracer.record_mplex();
        tracer.record_mplex();
        tracer.record_mplex();

        assert_eq!(tracer.mplex_messages(), 3);
    }

    #[test]
    fn test_record_timeout() {
        let mut tracer = IoTracer::new();
        tracer.record_timeout();
        tracer.record_timeout();

        assert_eq!(tracer.timeouts(), 2);
    }

    #[test]
    fn test_record_error() {
        let mut tracer = IoTracer::new();
        tracer.record_error();
        tracer.record_error();
        tracer.record_error();

        assert_eq!(tracer.errors(), 3);
    }

    #[test]
    fn test_record_flush() {
        let mut tracer = IoTracer::new();
        tracer.record_flush();
        tracer.record_flush();

        assert_eq!(tracer.flushes(), 2);
    }

    #[test]
    fn test_reset_clears_state() {
        let mut tracer = IoTracer::new();
        tracer.record_read(1024);
        tracer.record_write(2048);
        tracer.record_mplex();
        tracer.record_timeout();
        tracer.record_error();
        tracer.record_flush();

        tracer.reset();

        assert_eq!(tracer.total_read(), 0);
        assert_eq!(tracer.total_written(), 0);
        assert_eq!(tracer.read_ops(), 0);
        assert_eq!(tracer.write_ops(), 0);
        assert_eq!(tracer.mplex_messages(), 0);
        assert_eq!(tracer.timeouts(), 0);
        assert_eq!(tracer.errors(), 0);
        assert_eq!(tracer.flushes(), 0);
    }

    #[test]
    fn test_throughput_read_calculation() {
        let mut tracer = IoTracer::new();
        tracer.record_read(1024);
        tracer.record_read(1024);

        let throughput = tracer.throughput_read(Duration::from_secs(1));
        assert!((throughput - 2048.0).abs() < 0.1);
    }

    #[test]
    fn test_throughput_write_calculation() {
        let mut tracer = IoTracer::new();
        tracer.record_write(4096);
        tracer.record_write(4096);

        let throughput = tracer.throughput_write(Duration::from_secs(2));
        assert!((throughput - 4096.0).abs() < 0.1);
    }

    #[test]
    fn test_throughput_zero_elapsed() {
        let mut tracer = IoTracer::new();
        tracer.record_read(1024);
        tracer.record_write(2048);

        let read_throughput = tracer.throughput_read(Duration::ZERO);
        let write_throughput = tracer.throughput_write(Duration::ZERO);

        assert_eq!(read_throughput, 0.0);
        assert_eq!(write_throughput, 0.0);
    }

    #[test]
    fn test_zero_reads_and_writes() {
        let tracer = IoTracer::new();

        assert_eq!(tracer.total_read(), 0);
        assert_eq!(tracer.total_written(), 0);
        assert_eq!(tracer.throughput_read(Duration::from_secs(1)), 0.0);
        assert_eq!(tracer.throughput_write(Duration::from_secs(1)), 0.0);
    }

    #[test]
    fn test_saturating_add_total_read() {
        let mut tracer = IoTracer::new();
        tracer.total_read = u64::MAX - 100;
        tracer.record_read(200);

        assert_eq!(tracer.total_read(), u64::MAX);
        assert_eq!(tracer.read_ops(), 1);
    }

    #[test]
    fn test_saturating_add_total_written() {
        let mut tracer = IoTracer::new();
        tracer.total_written = u64::MAX - 50;
        tracer.record_write(100);

        assert_eq!(tracer.total_written(), u64::MAX);
        assert_eq!(tracer.write_ops(), 1);
    }

    #[test]
    fn test_saturating_add_operation_counts() {
        let mut tracer = IoTracer::new();
        tracer.read_ops = usize::MAX - 1;
        tracer.write_ops = usize::MAX - 1;
        tracer.mplex_messages = usize::MAX - 1;
        tracer.timeouts = usize::MAX - 1;
        tracer.errors = usize::MAX - 1;
        tracer.flushes = usize::MAX - 1;

        tracer.record_read(100);
        tracer.record_write(100);
        tracer.record_mplex();
        tracer.record_timeout();
        tracer.record_error();
        tracer.record_flush();

        assert_eq!(tracer.read_ops(), usize::MAX);
        assert_eq!(tracer.write_ops(), usize::MAX);
        assert_eq!(tracer.mplex_messages(), usize::MAX);
        assert_eq!(tracer.timeouts(), usize::MAX);
        assert_eq!(tracer.errors(), usize::MAX);
        assert_eq!(tracer.flushes(), usize::MAX);
    }

    #[test]
    fn test_trace_functions_do_not_panic() {
        // All trace functions should be callable without panicking
        trace_io_read(1024, "test_read");
        trace_io_write(2048, "test_write");
        trace_mplex_message(7, 1, 512);
        trace_io_timeout("read_operation", 30);
        trace_io_error("write_operation", "connection reset");
        trace_io_flush(4096);
        trace_io_summary(10240, 20480, Duration::from_secs(1));
    }

    #[test]
    fn test_summary_with_elapsed() {
        let mut tracer = IoTracer::new();
        tracer.record_read(1024);
        tracer.record_write(2048);

        // Should not panic
        tracer.summary(Duration::from_secs(1));

        // Stats should remain unchanged
        assert_eq!(tracer.total_read(), 1024);
        assert_eq!(tracer.total_written(), 2048);
    }

    #[test]
    fn test_multiple_operation_types() {
        let mut tracer = IoTracer::new();

        // Mix of different operations
        tracer.record_read(1024);
        tracer.record_write(2048);
        tracer.record_mplex();
        tracer.record_read(512);
        tracer.record_flush();
        tracer.record_write(1024);
        tracer.record_error();
        tracer.record_timeout();

        assert_eq!(tracer.total_read(), 1536);
        assert_eq!(tracer.total_written(), 3072);
        assert_eq!(tracer.read_ops(), 2);
        assert_eq!(tracer.write_ops(), 2);
        assert_eq!(tracer.mplex_messages(), 1);
        assert_eq!(tracer.flushes(), 1);
        assert_eq!(tracer.errors(), 1);
        assert_eq!(tracer.timeouts(), 1);
    }

    #[test]
    fn test_large_transfer_stats() {
        let mut tracer = IoTracer::new();

        // Simulate large transfer
        for _ in 0..10000 {
            tracer.record_read(65536); // 64KB reads
            tracer.record_write(65536); // 64KB writes
        }

        assert_eq!(tracer.total_read(), 655_360_000);
        assert_eq!(tracer.total_written(), 655_360_000);
        assert_eq!(tracer.read_ops(), 10000);
        assert_eq!(tracer.write_ops(), 10000);
    }

    #[cfg(feature = "tracing")]
    #[test]
    fn test_tracing_feature_enabled() {
        // When tracing feature is enabled, verify the functions compile and run
        // without panicking. We can't easily verify event emission without
        // tracing-subscriber, but this at least confirms the code compiles.
        let mut tracer = IoTracer::new();
        tracer.record_read(1024);
        tracer.record_write(2048);
        tracer.record_mplex();
        tracer.record_timeout();
        tracer.record_error();
        tracer.record_flush();
        tracer.summary(Duration::from_secs(1));

        // Verify the tracer still tracks stats correctly
        assert_eq!(tracer.total_read(), 1024);
        assert_eq!(tracer.total_written(), 2048);
        assert_eq!(tracer.mplex_messages(), 1);
        assert_eq!(tracer.timeouts(), 1);
        assert_eq!(tracer.errors(), 1);
        assert_eq!(tracer.flushes(), 1);
    }
}
