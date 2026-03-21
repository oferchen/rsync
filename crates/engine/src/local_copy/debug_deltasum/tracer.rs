//! `DeltasumTracer` - statistics aggregator for delta matching operations.

use std::time::{Duration, Instant};

use super::trace_deltasum_summary;

/// Aggregates statistics during delta matching and checksum operations.
///
/// Tracks hit/miss/false-alarm counts, matched vs. literal byte ratios, and
/// timing information across file delta operations. Accumulates per-file stats
/// into session-wide totals before emitting a final summary trace event.
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
    pub fn start_file(&mut self, _name: &str) {
        self.current_file_hits = 0;
        self.current_file_misses = 0;
        self.current_file_false_alarms = 0;
        self.current_file_matched_bytes = 0;
        self.current_file_literal_bytes = 0;
        self.current_file_start = Some(Instant::now());
    }

    /// Records a successful block match event.
    pub fn record_hit(&mut self, length: u32) {
        self.current_file_hits = self.current_file_hits.saturating_add(1);
        self.current_file_matched_bytes = self
            .current_file_matched_bytes
            .saturating_add(u64::from(length));
    }

    /// Records a miss event (literal data required).
    pub fn record_miss(&mut self, length: u32) {
        self.current_file_misses = self.current_file_misses.saturating_add(1);
        self.current_file_literal_bytes = self
            .current_file_literal_bytes
            .saturating_add(u64::from(length));
    }

    /// Records a false alarm event (weak checksum collision).
    pub fn record_false_alarm(&mut self) {
        self.current_file_false_alarms = self.current_file_false_alarms.saturating_add(1);
    }

    /// Ends tracking for the current file and accumulates stats into totals.
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
        let total = self
            .total_matched_bytes
            .saturating_add(self.total_literal_bytes);
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
        self.current_file_start
            .map_or(Duration::ZERO, |t| t.elapsed())
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::local_copy::debug_deltasum::{
        trace_checksum_block, trace_checksum_end, trace_checksum_start, trace_match_end,
        trace_match_false_alarm, trace_match_hit, trace_match_miss, trace_match_start,
    };

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
        trace_checksum_start("test.txt", 100, 4096);
        trace_checksum_block(0, 0xabcd1234, &[0x12, 0x34, 0x56, 0x78]);
        trace_checksum_end("test.txt", 100, Duration::from_millis(50));

        trace_match_start("test.txt", 409600, 409600);
        trace_match_hit(5, 4096, 4096, 0xdeadbeef);
        trace_match_miss(8192, 512);
        trace_match_false_alarm(0x12345678, 16384);
        trace_match_end(
            "test.txt",
            10,
            5,
            2,
            409600,
            380000,
            Duration::from_millis(100),
        );

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
        let mut tracer = DeltasumTracer::new();
        tracer.start_file("traced.txt");
        tracer.record_hit(4096);
        tracer.record_miss(512);
        tracer.record_false_alarm();
        tracer.end_file();

        assert_eq!(tracer.files_processed(), 1);
        assert_eq!(tracer.hits(), 1);
        assert_eq!(tracer.misses(), 1);
        assert_eq!(tracer.false_alarms(), 1);
        assert_eq!(tracer.matched_bytes(), 4096);
        assert_eq!(tracer.literal_bytes(), 512);
    }
}
