//! DEBUG_DELTASUM tracing for delta/checksum operations.
//!
//! This module provides structured tracing for delta matching and checksum
//! generation that match upstream rsync's match.c/checksum.c debug output format.
//! All tracing is conditionally compiled behind the `tracing` feature flag and
//! produces no-op inline functions when disabled.
//!
//! # Examples
//!
//! ```rust,ignore
//! use engine::local_copy::debug_deltasum::{DeltasumTracer, trace_checksum_start};
//!
//! let mut tracer = DeltasumTracer::new();
//! tracer.start_file("file.txt");
//!
//! trace_checksum_start("file.txt", 100, 4096);
//! trace_checksum_block(0, 0xabcd1234, &[0x12, 0x34, 0x56, 0x78]);
//!
//! tracer.record_hit(4096);
//! tracer.record_miss(512);
//! tracer.end_file();
//! ```

use std::time::{Duration, Instant};

/// Target name for tracing events, matching rsync's debug category.
const DELTASUM_TARGET: &str = "rsync::deltasum";

// ============================================================================
// Tracing functions (feature-gated)
// ============================================================================

/// Traces the start of checksum generation for a file.
///
/// Emits a tracing event when beginning to compute checksums for basis file
/// blocks. In upstream rsync, this corresponds to checksum generation in
/// checksum.c.
///
/// # Arguments
///
/// * `file_name` - Relative path of the file
/// * `block_count` - Number of blocks to checksum
/// * `block_size` - Size of each block in bytes
#[cfg(feature = "tracing")]
#[inline]
pub fn trace_checksum_start(file_name: &str, block_count: usize, block_size: u32) {
    tracing::debug!(
        target: DELTASUM_TARGET,
        file_name = %file_name,
        block_count = block_count,
        block_size = block_size,
        "checksum: starting"
    );
}

/// No-op when tracing is disabled.
#[cfg(not(feature = "tracing"))]
#[inline]
pub fn trace_checksum_start(_file_name: &str, _block_count: usize, _block_size: u32) {}

/// Traces a single checksum block computation.
///
/// Logs the weak (rolling) and strong checksums for a single block in the
/// basis file.
///
/// # Arguments
///
/// * `block_index` - Index of the block being checksummed
/// * `weak` - 32-bit rolling checksum (Adler-32 or similar)
/// * `strong` - Strong cryptographic checksum (typically MD5/MD4)
#[cfg(feature = "tracing")]
#[inline]
pub fn trace_checksum_block(block_index: usize, weak: u32, strong: &[u8]) {
    tracing::trace!(
        target: DELTASUM_TARGET,
        block_index = block_index,
        weak = format!("{:08x}", weak),
        strong = format!("{:02x}", strong.iter().fold(String::new(), |mut acc, b| {
            acc.push_str(&format!("{:02x}", b));
            acc
        })),
        "checksum: block"
    );
}

/// No-op when tracing is disabled.
#[cfg(not(feature = "tracing"))]
#[inline]
pub fn trace_checksum_block(_block_index: usize, _weak: u32, _strong: &[u8]) {}

/// Traces the completion of checksum generation.
///
/// Emits summary statistics for the checksum generation phase.
///
/// # Arguments
///
/// * `file_name` - Relative path of the file
/// * `block_count` - Total number of blocks checksummed
/// * `elapsed` - Time taken to generate checksums
#[cfg(feature = "tracing")]
#[inline]
pub fn trace_checksum_end(file_name: &str, block_count: usize, elapsed: Duration) {
    tracing::debug!(
        target: DELTASUM_TARGET,
        file_name = %file_name,
        block_count = block_count,
        elapsed_ms = elapsed.as_millis(),
        "checksum: complete"
    );
}

/// No-op when tracing is disabled.
#[cfg(not(feature = "tracing"))]
#[inline]
pub fn trace_checksum_end(_file_name: &str, _block_count: usize, _elapsed: Duration) {}

/// Traces the start of delta matching for a file.
///
/// Emits a tracing event when beginning to match target file data against
/// basis file checksums. In upstream rsync, this corresponds to the matching
/// logic in match.c.
///
/// # Arguments
///
/// * `file_name` - Relative path of the file
/// * `basis_size` - Size of the basis file in bytes
/// * `target_size` - Size of the target file in bytes
#[cfg(feature = "tracing")]
#[inline]
pub fn trace_match_start(file_name: &str, basis_size: u64, target_size: u64) {
    tracing::debug!(
        target: DELTASUM_TARGET,
        file_name = %file_name,
        basis_size = basis_size,
        target_size = target_size,
        "match: starting"
    );
}

/// No-op when tracing is disabled.
#[cfg(not(feature = "tracing"))]
#[inline]
pub fn trace_match_start(_file_name: &str, _basis_size: u64, _target_size: u64) {}

/// Traces a successful block match during delta generation.
///
/// Logs when a block from the target matches a block in the basis file via
/// rolling checksum, allowing compression via reference.
///
/// # Arguments
///
/// * `block_index` - Index of the matched block in the basis file
/// * `offset` - Offset in the target file where the match occurs
/// * `length` - Length of the matched block in bytes
/// * `weak` - Weak checksum that triggered the match
#[cfg(feature = "tracing")]
#[inline]
pub fn trace_match_hit(block_index: usize, offset: u64, length: u32, weak: u32) {
    tracing::trace!(
        target: DELTASUM_TARGET,
        block_index = block_index,
        offset = offset,
        length = length,
        weak = format!("{:08x}", weak),
        "match: hit"
    );
}

/// No-op when tracing is disabled.
#[cfg(not(feature = "tracing"))]
#[inline]
pub fn trace_match_hit(_block_index: usize, _offset: u64, _length: u32, _weak: u32) {}

/// Traces a miss during delta matching.
///
/// Logs when no matching block is found, requiring literal data transmission.
///
/// # Arguments
///
/// * `offset` - Offset in the target file for the literal data
/// * `length` - Length of the literal data in bytes
#[cfg(feature = "tracing")]
#[inline]
pub fn trace_match_miss(offset: u64, length: u32) {
    tracing::trace!(
        target: DELTASUM_TARGET,
        offset = offset,
        length = length,
        "match: miss"
    );
}

/// No-op when tracing is disabled.
#[cfg(not(feature = "tracing"))]
#[inline]
pub fn trace_match_miss(_offset: u64, _length: u32) {}

/// Traces a false alarm during delta matching.
///
/// Logs when a weak checksum matches but the strong checksum verification
/// fails, indicating a collision in the rolling checksum algorithm.
///
/// # Arguments
///
/// * `weak` - Weak checksum that collided
/// * `offset` - Offset in the target file where the false alarm occurred
#[cfg(feature = "tracing")]
#[inline]
pub fn trace_match_false_alarm(weak: u32, offset: u64) {
    tracing::trace!(
        target: DELTASUM_TARGET,
        weak = format!("{:08x}", weak),
        offset = offset,
        "match: false_alarm"
    );
}

/// No-op when tracing is disabled.
#[cfg(not(feature = "tracing"))]
#[inline]
pub fn trace_match_false_alarm(_weak: u32, _offset: u64) {}

/// Traces the completion of delta matching for a file.
///
/// Emits summary statistics showing match efficiency and data transfer
/// characteristics.
///
/// # Arguments
///
/// * `file_name` - Relative path of the file
/// * `hits` - Number of successful block matches
/// * `misses` - Number of literal data regions
/// * `false_alarms` - Number of weak checksum collisions
/// * `data_bytes` - Total bytes processed from target file
/// * `matched_bytes` - Total bytes matched from basis file
/// * `elapsed` - Time taken to perform matching
#[cfg(feature = "tracing")]
#[inline]
pub fn trace_match_end(
    file_name: &str,
    hits: usize,
    misses: usize,
    false_alarms: usize,
    data_bytes: u64,
    matched_bytes: u64,
    elapsed: Duration,
) {
    tracing::debug!(
        target: DELTASUM_TARGET,
        file_name = %file_name,
        hits = hits,
        misses = misses,
        false_alarms = false_alarms,
        data_bytes = data_bytes,
        matched_bytes = matched_bytes,
        elapsed_ms = elapsed.as_millis(),
        "match: complete"
    );
}

/// No-op when tracing is disabled.
#[cfg(not(feature = "tracing"))]
#[inline]
pub fn trace_match_end(
    _file_name: &str,
    _hits: usize,
    _misses: usize,
    _false_alarms: usize,
    _data_bytes: u64,
    _matched_bytes: u64,
    _elapsed: Duration,
) {}

/// Traces a summary of all delta/checksum operations.
///
/// Emits aggregate statistics for the entire session, including total files
/// processed, match efficiency, and data transfer ratios.
///
/// # Arguments
///
/// * `total_files` - Total number of files processed
/// * `total_hits` - Total number of block matches across all files
/// * `total_misses` - Total number of literal data regions across all files
/// * `total_false_alarms` - Total number of weak checksum collisions
/// * `total_matched` - Total bytes matched from basis files
/// * `total_literal` - Total bytes sent as literal data
#[cfg(feature = "tracing")]
#[inline]
pub fn trace_deltasum_summary(
    total_files: usize,
    total_hits: usize,
    total_misses: usize,
    total_false_alarms: usize,
    total_matched: u64,
    total_literal: u64,
) {
    tracing::info!(
        target: DELTASUM_TARGET,
        total_files = total_files,
        total_hits = total_hits,
        total_misses = total_misses,
        total_false_alarms = total_false_alarms,
        total_matched = total_matched,
        total_literal = total_literal,
        "deltasum: summary"
    );
}

/// No-op when tracing is disabled.
#[cfg(not(feature = "tracing"))]
#[inline]
pub fn trace_deltasum_summary(
    _total_files: usize,
    _total_hits: usize,
    _total_misses: usize,
    _total_false_alarms: usize,
    _total_matched: u64,
    _total_literal: u64,
) {}

// ============================================================================
// DeltasumTracer - stateful tracer for aggregating delta/checksum statistics
// ============================================================================

/// Aggregates statistics during delta matching and checksum operations.
///
/// Tracks hit/miss/false-alarm counts, matched vs. literal byte ratios, and
/// timing information across file delta operations. Use this when you need to
/// accumulate stats across multiple files before emitting final summary events.
///
/// # Examples
///
/// ```no_run
/// # use std::time::Duration;
/// # struct DeltasumTracer {
/// #     files_processed: usize,
/// #     total_hits: usize,
/// #     total_misses: usize,
/// #     total_false_alarms: usize,
/// #     total_matched_bytes: u64,
/// #     total_literal_bytes: u64,
/// #     current_file_hits: usize,
/// #     current_file_misses: usize,
/// #     current_file_false_alarms: usize,
/// #     current_file_matched_bytes: u64,
/// #     current_file_literal_bytes: u64,
/// # }
/// # impl DeltasumTracer {
/// #     fn new() -> Self {
/// #         Self {
/// #             files_processed: 0,
/// #             total_hits: 0,
/// #             total_misses: 0,
/// #             total_false_alarms: 0,
/// #             total_matched_bytes: 0,
/// #             total_literal_bytes: 0,
/// #             current_file_hits: 0,
/// #             current_file_misses: 0,
/// #             current_file_false_alarms: 0,
/// #             current_file_matched_bytes: 0,
/// #             current_file_literal_bytes: 0,
/// #         }
/// #     }
/// #     fn start_file(&mut self, _name: &str) {
/// #         self.current_file_hits = 0;
/// #         self.current_file_misses = 0;
/// #         self.current_file_false_alarms = 0;
/// #         self.current_file_matched_bytes = 0;
/// #         self.current_file_literal_bytes = 0;
/// #     }
/// #     fn record_hit(&mut self, length: u32) {
/// #         self.current_file_hits += 1;
/// #         self.current_file_matched_bytes += length as u64;
/// #     }
/// #     fn record_miss(&mut self, length: u32) {
/// #         self.current_file_misses += 1;
/// #         self.current_file_literal_bytes += length as u64;
/// #     }
/// #     fn record_false_alarm(&mut self) {
/// #         self.current_file_false_alarms += 1;
/// #     }
/// #     fn end_file(&mut self) {
/// #         self.files_processed += 1;
/// #         self.total_hits += self.current_file_hits;
/// #         self.total_misses += self.current_file_misses;
/// #         self.total_false_alarms += self.current_file_false_alarms;
/// #         self.total_matched_bytes += self.current_file_matched_bytes;
/// #         self.total_literal_bytes += self.current_file_literal_bytes;
/// #     }
/// #     fn summary(&mut self) {}
/// #     fn reset(&mut self) {
/// #         *self = Self::new();
/// #     }
/// #     fn hits(&self) -> usize { self.total_hits }
/// #     fn misses(&self) -> usize { self.total_misses }
/// #     fn false_alarms(&self) -> usize { self.total_false_alarms }
/// #     fn matched_bytes(&self) -> u64 { self.total_matched_bytes }
/// #     fn literal_bytes(&self) -> u64 { self.total_literal_bytes }
/// #     fn files_processed(&self) -> usize { self.files_processed }
/// #     fn match_ratio(&self) -> f64 {
/// #         let total = self.total_matched_bytes + self.total_literal_bytes;
/// #         if total == 0 {
/// #             0.0
/// #         } else {
/// #             self.total_matched_bytes as f64 / total as f64
/// #         }
/// #     }
/// # }
/// let mut tracer = DeltasumTracer::new();
/// tracer.start_file("file1.txt");
/// tracer.record_hit(4096);
/// tracer.record_miss(512);
/// tracer.record_false_alarm();
/// tracer.end_file();
///
/// tracer.summary();
/// assert_eq!(tracer.hits(), 1);
/// assert_eq!(tracer.misses(), 1);
/// assert_eq!(tracer.false_alarms(), 1);
/// assert_eq!(tracer.matched_bytes(), 4096);
/// assert_eq!(tracer.literal_bytes(), 512);
/// ```
#[derive(Debug, Clone)]
pub struct DeltasumTracer {
    files_processed: usize,
    total_hits: usize,
    total_misses: usize,
    total_false_alarms: usize,
    total_matched_bytes: u64,
    total_literal_bytes: u64,
    current_file_hits: usize,
    current_file_misses: usize,
    current_file_false_alarms: usize,
    current_file_matched_bytes: u64,
    current_file_literal_bytes: u64,
    current_file_start: Option<Instant>,
}

impl Default for DeltasumTracer {
    fn default() -> Self {
        Self::new()
    }
}

impl DeltasumTracer {
    /// Creates a new deltasum tracer with zero counts.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            files_processed: 0,
            total_hits: 0,
            total_misses: 0,
            total_false_alarms: 0,
            total_matched_bytes: 0,
            total_literal_bytes: 0,
            current_file_hits: 0,
            current_file_misses: 0,
            current_file_false_alarms: 0,
            current_file_matched_bytes: 0,
            current_file_literal_bytes: 0,
            current_file_start: None,
        }
    }

    /// Starts tracking a file's delta/checksum operation.
    ///
    /// Resets per-file counters and records the start time.
    ///
    /// # Arguments
    ///
    /// * `name` - Relative path of the file
    pub fn start_file(&mut self, _name: &str) {
        self.current_file_hits = 0;
        self.current_file_misses = 0;
        self.current_file_false_alarms = 0;
        self.current_file_matched_bytes = 0;
        self.current_file_literal_bytes = 0;
        self.current_file_start = Some(Instant::now());
    }

    /// Records a successful block match event.
    ///
    /// # Arguments
    ///
    /// * `length` - Length of the matched block in bytes
    pub fn record_hit(&mut self, length: u32) {
        self.current_file_hits = self.current_file_hits.saturating_add(1);
        self.current_file_matched_bytes =
            self.current_file_matched_bytes.saturating_add(u64::from(length));
    }

    /// Records a miss event (literal data required).
    ///
    /// # Arguments
    ///
    /// * `length` - Length of the literal data in bytes
    pub fn record_miss(&mut self, length: u32) {
        self.current_file_misses = self.current_file_misses.saturating_add(1);
        self.current_file_literal_bytes =
            self.current_file_literal_bytes.saturating_add(u64::from(length));
    }

    /// Records a false alarm event (weak checksum collision).
    pub fn record_false_alarm(&mut self) {
        self.current_file_false_alarms = self.current_file_false_alarms.saturating_add(1);
    }

    /// Ends tracking for the current file and accumulates stats.
    pub fn end_file(&mut self) {
        self.files_processed = self.files_processed.saturating_add(1);
        self.total_hits = self.total_hits.saturating_add(self.current_file_hits);
        self.total_misses = self.total_misses.saturating_add(self.current_file_misses);
        self.total_false_alarms = self
            .total_false_alarms
            .saturating_add(self.current_file_false_alarms);
        self.total_matched_bytes = self
            .total_matched_bytes
            .saturating_add(self.current_file_matched_bytes);
        self.total_literal_bytes = self
            .total_literal_bytes
            .saturating_add(self.current_file_literal_bytes);
        self.current_file_start = None;
    }

    /// Emits a summary trace event for the entire session.
    pub fn summary(&mut self) {
        trace_deltasum_summary(
            self.files_processed,
            self.total_hits,
            self.total_misses,
            self.total_false_alarms,
            self.total_matched_bytes,
            self.total_literal_bytes,
        );
    }

    /// Returns the total number of successful block matches.
    #[must_use]
    pub const fn hits(&self) -> usize {
        self.total_hits
    }

    /// Returns the total number of literal data regions.
    #[must_use]
    pub const fn misses(&self) -> usize {
        self.total_misses
    }

    /// Returns the total number of weak checksum collisions.
    #[must_use]
    pub const fn false_alarms(&self) -> usize {
        self.total_false_alarms
    }

    /// Returns the total bytes matched from basis files.
    #[must_use]
    pub const fn matched_bytes(&self) -> u64 {
        self.total_matched_bytes
    }

    /// Returns the total bytes sent as literal data.
    #[must_use]
    pub const fn literal_bytes(&self) -> u64 {
        self.total_literal_bytes
    }

    /// Returns the total number of files processed.
    #[must_use]
    pub const fn files_processed(&self) -> usize {
        self.files_processed
    }

    /// Returns the match ratio (matched bytes / total bytes).
    ///
    /// Returns 0.0 if no data has been processed.
    #[must_use]
    pub fn match_ratio(&self) -> f64 {
        let total = self.total_matched_bytes.saturating_add(self.total_literal_bytes);
        if total == 0 {
            0.0
        } else {
            self.total_matched_bytes as f64 / total as f64
        }
    }

    /// Returns the elapsed time for the current file.
    ///
    /// Returns `Duration::ZERO` if no file is currently being tracked.
    #[must_use]
    pub fn current_file_elapsed(&self) -> Duration {
        self.current_file_start.map_or(Duration::ZERO, |t| t.elapsed())
    }

    /// Resets all counters and timing state to zero.
    pub fn reset(&mut self) {
        self.files_processed = 0;
        self.total_hits = 0;
        self.total_misses = 0;
        self.total_false_alarms = 0;
        self.total_matched_bytes = 0;
        self.total_literal_bytes = 0;
        self.current_file_hits = 0;
        self.current_file_misses = 0;
        self.current_file_false_alarms = 0;
        self.current_file_matched_bytes = 0;
        self.current_file_literal_bytes = 0;
        self.current_file_start = None;
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
        let tracer = DeltasumTracer::new();
        assert_eq!(tracer.hits(), 0);
        assert_eq!(tracer.misses(), 0);
        assert_eq!(tracer.false_alarms(), 0);
        assert_eq!(tracer.matched_bytes(), 0);
        assert_eq!(tracer.literal_bytes(), 0);
        assert_eq!(tracer.files_processed(), 0);
        assert_eq!(tracer.match_ratio(), 0.0);
        assert_eq!(tracer.current_file_elapsed(), Duration::ZERO);
    }

    #[test]
    fn test_tracer_default() {
        let tracer = DeltasumTracer::default();
        assert_eq!(tracer.hits(), 0);
        assert_eq!(tracer.misses(), 0);
        assert_eq!(tracer.false_alarms(), 0);
    }

    #[test]
    fn test_record_hit_accumulates() {
        let mut tracer = DeltasumTracer::new();
        tracer.start_file("test.txt");
        tracer.record_hit(4096);
        tracer.record_hit(2048);
        tracer.record_hit(1024);
        tracer.end_file();

        assert_eq!(tracer.hits(), 3);
        assert_eq!(tracer.matched_bytes(), 7168);
    }

    #[test]
    fn test_record_miss_accumulates() {
        let mut tracer = DeltasumTracer::new();
        tracer.start_file("test.txt");
        tracer.record_miss(512);
        tracer.record_miss(256);
        tracer.record_miss(128);
        tracer.end_file();

        assert_eq!(tracer.misses(), 3);
        assert_eq!(tracer.literal_bytes(), 896);
    }

    #[test]
    fn test_record_false_alarm_accumulates() {
        let mut tracer = DeltasumTracer::new();
        tracer.start_file("test.txt");
        tracer.record_false_alarm();
        tracer.record_false_alarm();
        tracer.end_file();

        assert_eq!(tracer.false_alarms(), 2);
    }

    #[test]
    fn test_multiple_files() {
        let mut tracer = DeltasumTracer::new();

        tracer.start_file("file1.txt");
        tracer.record_hit(4096);
        tracer.record_miss(512);
        tracer.record_false_alarm();
        tracer.end_file();

        tracer.start_file("file2.txt");
        tracer.record_hit(8192);
        tracer.record_miss(1024);
        tracer.end_file();

        assert_eq!(tracer.files_processed(), 2);
        assert_eq!(tracer.hits(), 2);
        assert_eq!(tracer.misses(), 2);
        assert_eq!(tracer.false_alarms(), 1);
        assert_eq!(tracer.matched_bytes(), 12288);
        assert_eq!(tracer.literal_bytes(), 1536);
    }

    #[test]
    fn test_reset_clears_state() {
        let mut tracer = DeltasumTracer::new();
        tracer.start_file("test.txt");
        tracer.record_hit(4096);
        tracer.record_miss(512);
        tracer.record_false_alarm();
        tracer.end_file();

        tracer.reset();

        assert_eq!(tracer.files_processed(), 0);
        assert_eq!(tracer.hits(), 0);
        assert_eq!(tracer.misses(), 0);
        assert_eq!(tracer.false_alarms(), 0);
        assert_eq!(tracer.matched_bytes(), 0);
        assert_eq!(tracer.literal_bytes(), 0);
        assert_eq!(tracer.match_ratio(), 0.0);
        assert_eq!(tracer.current_file_elapsed(), Duration::ZERO);
    }

    #[test]
    fn test_match_ratio() {
        let mut tracer = DeltasumTracer::new();
        tracer.start_file("test.txt");
        tracer.record_hit(4096);
        tracer.record_miss(1024);
        tracer.end_file();

        let ratio = tracer.match_ratio();
        assert!((ratio - 0.8).abs() < 0.001);
    }

    #[test]
    fn test_match_ratio_all_hits() {
        let mut tracer = DeltasumTracer::new();
        tracer.start_file("test.txt");
        tracer.record_hit(8192);
        tracer.end_file();

        assert_eq!(tracer.match_ratio(), 1.0);
    }

    #[test]
    fn test_match_ratio_all_misses() {
        let mut tracer = DeltasumTracer::new();
        tracer.start_file("test.txt");
        tracer.record_miss(8192);
        tracer.end_file();

        assert_eq!(tracer.match_ratio(), 0.0);
    }

    #[test]
    fn test_match_ratio_zero_bytes() {
        let tracer = DeltasumTracer::new();
        assert_eq!(tracer.match_ratio(), 0.0);
    }

    #[test]
    fn test_saturating_add_hits() {
        let mut tracer = DeltasumTracer::new();
        tracer.total_hits = usize::MAX - 1;
        tracer.start_file("test.txt");
        tracer.record_hit(100);
        tracer.record_hit(100);
        tracer.record_hit(100);
        tracer.end_file();

        assert_eq!(tracer.hits(), usize::MAX);
    }

    #[test]
    fn test_saturating_add_misses() {
        let mut tracer = DeltasumTracer::new();
        tracer.total_misses = usize::MAX - 1;
        tracer.start_file("test.txt");
        tracer.record_miss(100);
        tracer.record_miss(100);
        tracer.record_miss(100);
        tracer.end_file();

        assert_eq!(tracer.misses(), usize::MAX);
    }

    #[test]
    fn test_saturating_add_false_alarms() {
        let mut tracer = DeltasumTracer::new();
        tracer.total_false_alarms = usize::MAX - 1;
        tracer.start_file("test.txt");
        tracer.record_false_alarm();
        tracer.record_false_alarm();
        tracer.record_false_alarm();
        tracer.end_file();

        assert_eq!(tracer.false_alarms(), usize::MAX);
    }

    #[test]
    fn test_saturating_add_matched_bytes() {
        let mut tracer = DeltasumTracer::new();
        tracer.start_file("test.txt");
        tracer.current_file_matched_bytes = u64::MAX - 50;
        tracer.record_hit(100);

        assert_eq!(tracer.current_file_matched_bytes, u64::MAX);
    }

    #[test]
    fn test_saturating_add_literal_bytes() {
        let mut tracer = DeltasumTracer::new();
        tracer.start_file("test.txt");
        tracer.current_file_literal_bytes = u64::MAX - 50;
        tracer.record_miss(100);

        assert_eq!(tracer.current_file_literal_bytes, u64::MAX);
    }

    #[test]
    fn test_trace_functions_do_not_panic() {
        // All trace functions should be callable without panicking
        trace_checksum_start("test.txt", 100, 4096);
        trace_checksum_block(0, 0xabcd1234, &[0x12, 0x34, 0x56, 0x78]);
        trace_checksum_end("test.txt", 100, Duration::from_millis(50));

        trace_match_start("test.txt", 409600, 409600);
        trace_match_hit(5, 4096, 4096, 0xdeadbeef);
        trace_match_miss(8192, 512);
        trace_match_false_alarm(0x12345678, 16384);
        trace_match_end("test.txt", 10, 5, 2, 409600, 380000, Duration::from_millis(100));

        trace_deltasum_summary(5, 50, 25, 10, 2000000, 500000);
    }

    #[test]
    fn test_start_file_initializes_timing() {
        let mut tracer = DeltasumTracer::new();
        tracer.start_file("test.txt");

        std::thread::sleep(Duration::from_millis(1));
        assert!(tracer.current_file_elapsed() > Duration::ZERO);
    }

    #[test]
    fn test_end_file_clears_timing() {
        let mut tracer = DeltasumTracer::new();
        tracer.start_file("test.txt");
        tracer.end_file();

        assert_eq!(tracer.current_file_elapsed(), Duration::ZERO);
    }

    #[test]
    fn test_start_file_resets_per_file_counters() {
        let mut tracer = DeltasumTracer::new();
        tracer.start_file("file1.txt");
        tracer.record_hit(4096);
        tracer.record_miss(512);
        tracer.end_file();

        tracer.start_file("file2.txt");
        assert_eq!(tracer.current_file_hits, 0);
        assert_eq!(tracer.current_file_misses, 0);
        assert_eq!(tracer.current_file_false_alarms, 0);
        assert_eq!(tracer.current_file_matched_bytes, 0);
        assert_eq!(tracer.current_file_literal_bytes, 0);
    }

    #[test]
    fn test_summary_does_not_panic() {
        let mut tracer = DeltasumTracer::new();
        tracer.start_file("test.txt");
        tracer.record_hit(4096);
        tracer.end_file();

        tracer.summary();
        // Just verify it doesn't panic
    }

    #[test]
    fn test_zero_length_file() {
        let mut tracer = DeltasumTracer::new();
        tracer.start_file("empty.txt");
        tracer.end_file();

        assert_eq!(tracer.files_processed(), 1);
        assert_eq!(tracer.hits(), 0);
        assert_eq!(tracer.misses(), 0);
        assert_eq!(tracer.matched_bytes(), 0);
        assert_eq!(tracer.literal_bytes(), 0);
    }

    #[test]
    fn test_saturating_add_files_processed() {
        let mut tracer = DeltasumTracer::new();
        tracer.files_processed = usize::MAX - 1;
        tracer.start_file("file1.txt");
        tracer.end_file();
        tracer.start_file("file2.txt");
        tracer.end_file();
        tracer.start_file("file3.txt");
        tracer.end_file();

        assert_eq!(tracer.files_processed(), usize::MAX);
    }

    #[cfg(feature = "tracing")]
    #[test]
    fn test_tracing_feature_enabled() {
        // When tracing feature is enabled, verify the functions compile and run
        // without panicking. We can't easily verify event emission without
        // tracing-subscriber, but this at least confirms the code compiles.
        let mut tracer = DeltasumTracer::new();
        tracer.start_file("traced.txt");
        tracer.record_hit(4096);
        tracer.record_miss(512);
        tracer.record_false_alarm();
        tracer.end_file();

        // Verify the tracer still tracks stats correctly
        assert_eq!(tracer.files_processed(), 1);
        assert_eq!(tracer.hits(), 1);
        assert_eq!(tracer.misses(), 1);
        assert_eq!(tracer.false_alarms(), 1);
        assert_eq!(tracer.matched_bytes(), 4096);
        assert_eq!(tracer.literal_bytes(), 512);
    }
}
