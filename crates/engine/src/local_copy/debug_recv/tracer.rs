//! Stateful receive-operation statistics aggregator.
//!
//! [`RecvTracer`] accumulates file counts, byte counts, basis selections,
//! checksum verifications, match/literal ratios, and timing across an
//! entire receive session before emitting final summary events.

use std::time::{Duration, Instant};

use super::trace_functions::{
    trace_basis_file_selected, trace_delta_apply_literal, trace_delta_apply_match,
    trace_recv_file_end, trace_recv_file_start, trace_recv_summary,
};

/// Aggregates statistics during file receive operations.
///
/// Tracks file counts, byte counts, basis selections, checksum verifications,
/// match/literal ratios, and timing information across file receive operations.
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
    pub(crate) files_received: usize,
    pub(crate) bytes_received: u64,
    pub(crate) matched_bytes: u64,
    pub(crate) literal_bytes: u64,
    pub(crate) basis_selections: usize,
    pub(crate) checksum_matches: usize,
    pub(crate) checksum_mismatches: usize,
    pub(crate) current_file_start: Option<Instant>,
    pub(crate) session_start: Option<Instant>,
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
    pub fn start_file(&mut self, name: &str, file_size: u64, index: usize) {
        if self.session_start.is_none() {
            self.session_start = Some(Instant::now());
        }
        self.current_file_start = Some(Instant::now());
        trace_recv_file_start(name, file_size, index);
    }

    /// Records a basis file selection event.
    pub fn record_basis(&mut self, name: &str, basis_path: &str, basis_size: u64) {
        self.basis_selections += 1;
        trace_basis_file_selected(name, basis_path, basis_size);
    }

    /// Records a block match event during delta application.
    pub fn record_match(&mut self, block_index: usize, offset: u64, length: u32) {
        self.matched_bytes = self.matched_bytes.saturating_add(u64::from(length));
        trace_delta_apply_match(block_index, offset, length);
    }

    /// Records a literal data event during delta application.
    pub fn record_literal(&mut self, offset: u64, length: u32) {
        self.literal_bytes = self.literal_bytes.saturating_add(u64::from(length));
        trace_delta_apply_literal(offset, length);
    }

    /// Records a checksum verification result.
    pub fn record_checksum_verify(&mut self, matched: bool) {
        if matched {
            self.checksum_matches += 1;
        } else {
            self.checksum_mismatches += 1;
        }
    }

    /// Ends tracking for the current file and emits a summary trace event.
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

    /// Returns elapsed time for the current file, or `Duration::ZERO` if idle.
    #[must_use]
    pub fn current_file_elapsed(&self) -> Duration {
        self.current_file_start
            .map_or(Duration::ZERO, |t| t.elapsed())
    }

    /// Returns total elapsed time since the session started, or `Duration::ZERO`.
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
