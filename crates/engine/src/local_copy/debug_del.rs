//! DEBUG_DEL tracing for deletion operations.
//!
//! This module provides structured tracing for file/directory deletion operations
//! that match upstream rsync's delete.c debug output format. All tracing is
//! conditionally compiled behind the `tracing` feature flag and produces no-op
//! inline functions when disabled.
//!
//! # Examples
//!
//! ```rust,ignore
//! use engine::local_copy::debug_del::{DeleteTracer, DeletePhase, trace_delete_file};
//!
//! let mut tracer = DeleteTracer::new();
//! tracer.start_phase(DeletePhase::Before);
//!
//! trace_delete_file("obsolete.txt", false);
//! tracer.record_file_deleted();
//!
//! tracer.end_phase();
//! tracer.summary();
//! ```

use std::fmt;
use std::time::{Duration, Instant};

/// Target name for tracing events, matching rsync's debug category.
const DEL_TARGET: &str = "rsync::del";

// ============================================================================
// DeletePhase enum
// ============================================================================

/// Represents the timing phase of deletion operations.
///
/// Matches rsync's `--delete-before`, `--delete-during`, and `--delete-after`
/// timing modes for deletion operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeletePhase {
    /// Deletions occur before file transfer (`--delete-before`).
    Before,
    /// Deletions occur during file transfer (`--delete-during`).
    During,
    /// Deletions occur after file transfer (`--delete-after`).
    After,
}

impl fmt::Display for DeletePhase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DeletePhase::Before => write!(f, "delete-before"),
            DeletePhase::During => write!(f, "delete-during"),
            DeletePhase::After => write!(f, "delete-after"),
        }
    }
}

// ============================================================================
// Tracing functions (feature-gated)
// ============================================================================

/// Traces the start of a deletion phase.
///
/// Emits a tracing event that marks the beginning of a deletion phase.
/// In upstream rsync, this corresponds to entering delete_files() or similar.
///
/// # Arguments
///
/// * `phase` - The deletion phase being started
#[cfg(feature = "tracing")]
#[inline]
pub fn trace_delete_phase_start(phase: DeletePhase) {
    tracing::info!(
        target: DEL_TARGET,
        phase = %phase,
        "delete_phase: starting"
    );
}

/// No-op when tracing is disabled.
#[cfg(not(feature = "tracing"))]
#[inline]
pub fn trace_delete_phase_start(_phase: DeletePhase) {}

/// Traces the completion of a deletion phase.
///
/// Emits summary statistics for the deletion phase, including count and timing.
///
/// # Arguments
///
/// * `phase` - The deletion phase being completed
/// * `deleted_count` - Total number of files/directories deleted
/// * `elapsed` - Time taken for this phase
#[cfg(feature = "tracing")]
#[inline]
pub fn trace_delete_phase_end(phase: DeletePhase, deleted_count: usize, elapsed: Duration) {
    tracing::info!(
        target: DEL_TARGET,
        phase = %phase,
        deleted_count = deleted_count,
        elapsed_ms = elapsed.as_millis(),
        "delete_phase: complete"
    );
}

/// No-op when tracing is disabled.
#[cfg(not(feature = "tracing"))]
#[inline]
pub fn trace_delete_phase_end(_phase: DeletePhase, _deleted_count: usize, _elapsed: Duration) {}

/// Traces an individual file or directory deletion.
///
/// Logs when a single file or directory is successfully deleted during
/// a deletion phase.
///
/// # Arguments
///
/// * `path` - Relative path of the deleted file or directory
/// * `is_directory` - True if this is a directory, false if it's a file
#[cfg(feature = "tracing")]
#[inline]
pub fn trace_delete_file(path: &str, is_directory: bool) {
    tracing::debug!(
        target: DEL_TARGET,
        path = %path,
        is_directory = is_directory,
        "delete: removing"
    );
}

/// No-op when tracing is disabled.
#[cfg(not(feature = "tracing"))]
#[inline]
pub fn trace_delete_file(_path: &str, _is_directory: bool) {}

/// Traces a skipped deletion.
///
/// Logs when a file or directory deletion is skipped due to constraints
/// like `--max-delete` limits or filter rules.
///
/// # Arguments
///
/// * `path` - Relative path of the skipped item
/// * `reason` - Human-readable reason for skipping (e.g., "max-delete limit", "filter rule")
#[cfg(feature = "tracing")]
#[inline]
pub fn trace_delete_skipped(path: &str, reason: &str) {
    tracing::debug!(
        target: DEL_TARGET,
        path = %path,
        reason = reason,
        "delete: skipped"
    );
}

/// No-op when tracing is disabled.
#[cfg(not(feature = "tracing"))]
#[inline]
pub fn trace_delete_skipped(_path: &str, _reason: &str) {}

/// Traces a deletion error.
///
/// Logs when an attempt to delete a file or directory fails.
///
/// # Arguments
///
/// * `path` - Relative path of the item that failed to delete
/// * `error` - Human-readable error description
#[cfg(feature = "tracing")]
#[inline]
pub fn trace_delete_error(path: &str, error: &str) {
    tracing::warn!(
        target: DEL_TARGET,
        path = %path,
        error = error,
        "delete: error"
    );
}

/// No-op when tracing is disabled.
#[cfg(not(feature = "tracing"))]
#[inline]
pub fn trace_delete_error(_path: &str, _error: &str) {}

/// Traces a summary of all deletion operations.
///
/// Emits aggregate statistics for the entire deletion session, including
/// total deletions, skips, errors, and elapsed time.
///
/// # Arguments
///
/// * `total_deleted` - Total number of files/directories deleted
/// * `total_skipped` - Total number of deletions skipped
/// * `total_errors` - Total number of deletion errors
/// * `elapsed` - Total time elapsed for all deletion operations
#[cfg(feature = "tracing")]
#[inline]
pub fn trace_delete_summary(
    total_deleted: usize,
    total_skipped: usize,
    total_errors: usize,
    elapsed: Duration,
) {
    tracing::info!(
        target: DEL_TARGET,
        total_deleted = total_deleted,
        total_skipped = total_skipped,
        total_errors = total_errors,
        elapsed_ms = elapsed.as_millis(),
        "delete: summary"
    );
}

/// No-op when tracing is disabled.
#[cfg(not(feature = "tracing"))]
#[inline]
pub fn trace_delete_summary(
    _total_deleted: usize,
    _total_skipped: usize,
    _total_errors: usize,
    _elapsed: Duration,
) {
}

// ============================================================================
// DeleteTracer - stateful tracer for aggregating deletion statistics
// ============================================================================

/// Aggregates statistics during deletion operations.
///
/// Tracks deletion counts, skip counts, errors, and timing information across
/// deletion phases. Use this when you need to accumulate stats across multiple
/// deletions before emitting final summary events.
///
/// # Examples
///
/// ```no_run
/// # use std::time::Duration;
/// # #[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// # enum DeletePhase { Before, During, After }
/// # struct DeleteTracer { files_deleted: usize, dirs_deleted: usize }
/// # impl DeleteTracer {
/// #     fn new() -> Self { Self { files_deleted: 0, dirs_deleted: 0 } }
/// #     fn start_phase(&mut self, _phase: DeletePhase) {}
/// #     fn record_file_deleted(&mut self) { self.files_deleted += 1; }
/// #     fn record_dir_deleted(&mut self) { self.dirs_deleted += 1; }
/// #     fn record_skipped(&mut self) {}
/// #     fn record_error(&mut self) {}
/// #     fn end_phase(&mut self) -> Duration { Duration::ZERO }
/// #     fn summary(&mut self) -> Duration { Duration::ZERO }
/// #     fn total_deleted(&self) -> usize { self.files_deleted + self.dirs_deleted }
/// # }
/// let mut tracer = DeleteTracer::new();
/// tracer.start_phase(DeletePhase::Before);
///
/// tracer.record_file_deleted();
/// tracer.record_dir_deleted();
/// tracer.record_skipped();
///
/// tracer.end_phase();
/// assert_eq!(tracer.total_deleted(), 2);
/// ```
#[derive(Debug, Clone)]
pub struct DeleteTracer {
    files_deleted: usize,
    dirs_deleted: usize,
    skipped: usize,
    errors: usize,
    current_phase: Option<DeletePhase>,
    phase_start_time: Option<Instant>,
    session_start_time: Option<Instant>,
}

impl Default for DeleteTracer {
    fn default() -> Self {
        Self::new()
    }
}

impl DeleteTracer {
    /// Creates a new deletion tracer with zero counts.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            files_deleted: 0,
            dirs_deleted: 0,
            skipped: 0,
            errors: 0,
            current_phase: None,
            phase_start_time: None,
            session_start_time: None,
        }
    }

    /// Starts tracking a deletion phase, recording the start time.
    ///
    /// # Arguments
    ///
    /// * `phase` - The deletion phase being started
    pub fn start_phase(&mut self, phase: DeletePhase) {
        if self.session_start_time.is_none() {
            self.session_start_time = Some(Instant::now());
        }
        self.current_phase = Some(phase);
        self.phase_start_time = Some(Instant::now());
        trace_delete_phase_start(phase);
    }

    /// Records a file deletion, incrementing the file count.
    pub fn record_file_deleted(&mut self) {
        self.files_deleted += 1;
    }

    /// Records a directory deletion, incrementing the directory count.
    pub fn record_dir_deleted(&mut self) {
        self.dirs_deleted += 1;
    }

    /// Records a skipped deletion, incrementing the skip count.
    pub fn record_skipped(&mut self) {
        self.skipped += 1;
    }

    /// Records a deletion error, incrementing the error count.
    pub fn record_error(&mut self) {
        self.errors += 1;
    }

    /// Ends tracking for the current phase and emits a summary trace event.
    ///
    /// Returns the elapsed time since `start_phase()` was called, or
    /// `Duration::ZERO` if timing was not initialized.
    pub fn end_phase(&mut self) -> Duration {
        let elapsed = self.phase_elapsed();
        if let Some(phase) = self.current_phase {
            let phase_deleted = self.files_deleted + self.dirs_deleted;
            trace_delete_phase_end(phase, phase_deleted, elapsed);
        }
        self.phase_start_time = None;
        self.current_phase = None;
        elapsed
    }

    /// Emits a summary trace event for the entire deletion session.
    ///
    /// Returns the total elapsed time since the first phase started.
    pub fn summary(&mut self) -> Duration {
        let elapsed = self.session_elapsed();
        let total_deleted = self.files_deleted + self.dirs_deleted;
        trace_delete_summary(total_deleted, self.skipped, self.errors, elapsed);
        elapsed
    }

    /// Returns the number of files deleted.
    #[must_use]
    pub const fn files_deleted(&self) -> usize {
        self.files_deleted
    }

    /// Returns the number of directories deleted.
    #[must_use]
    pub const fn dirs_deleted(&self) -> usize {
        self.dirs_deleted
    }

    /// Returns the total number of items deleted (files + directories).
    #[must_use]
    pub const fn total_deleted(&self) -> usize {
        self.files_deleted + self.dirs_deleted
    }

    /// Returns the number of deletions skipped.
    #[must_use]
    pub const fn skipped(&self) -> usize {
        self.skipped
    }

    /// Returns the number of deletion errors.
    #[must_use]
    pub const fn errors(&self) -> usize {
        self.errors
    }

    /// Returns the current deletion phase, if any.
    #[must_use]
    pub const fn current_phase(&self) -> Option<DeletePhase> {
        self.current_phase
    }

    /// Returns the elapsed time for the current phase.
    ///
    /// Returns `Duration::ZERO` if no phase is currently being tracked.
    #[must_use]
    pub fn phase_elapsed(&self) -> Duration {
        self.phase_start_time.map_or(Duration::ZERO, |t| t.elapsed())
    }

    /// Returns the total elapsed time since the session started.
    ///
    /// Returns `Duration::ZERO` if no session has been started.
    #[must_use]
    pub fn session_elapsed(&self) -> Duration {
        self.session_start_time
            .map_or(Duration::ZERO, |t| t.elapsed())
    }

    /// Resets all counters and timing state to zero.
    pub fn reset(&mut self) {
        self.files_deleted = 0;
        self.dirs_deleted = 0;
        self.skipped = 0;
        self.errors = 0;
        self.current_phase = None;
        self.phase_start_time = None;
        self.session_start_time = None;
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_delete_phase_display_before() {
        assert_eq!(DeletePhase::Before.to_string(), "delete-before");
    }

    #[test]
    fn test_delete_phase_display_during() {
        assert_eq!(DeletePhase::During.to_string(), "delete-during");
    }

    #[test]
    fn test_delete_phase_display_after() {
        assert_eq!(DeletePhase::After.to_string(), "delete-after");
    }

    #[test]
    fn test_tracer_new() {
        let tracer = DeleteTracer::new();
        assert_eq!(tracer.files_deleted(), 0);
        assert_eq!(tracer.dirs_deleted(), 0);
        assert_eq!(tracer.total_deleted(), 0);
        assert_eq!(tracer.skipped(), 0);
        assert_eq!(tracer.errors(), 0);
        assert_eq!(tracer.current_phase(), None);
        assert_eq!(tracer.phase_elapsed(), Duration::ZERO);
        assert_eq!(tracer.session_elapsed(), Duration::ZERO);
    }

    #[test]
    fn test_tracer_default() {
        let tracer = DeleteTracer::default();
        assert_eq!(tracer.files_deleted(), 0);
        assert_eq!(tracer.dirs_deleted(), 0);
        assert_eq!(tracer.total_deleted(), 0);
    }

    #[test]
    fn test_record_file_deleted() {
        let mut tracer = DeleteTracer::new();
        tracer.record_file_deleted();
        tracer.record_file_deleted();

        assert_eq!(tracer.files_deleted(), 2);
        assert_eq!(tracer.dirs_deleted(), 0);
        assert_eq!(tracer.total_deleted(), 2);
    }

    #[test]
    fn test_record_dir_deleted() {
        let mut tracer = DeleteTracer::new();
        tracer.record_dir_deleted();
        tracer.record_dir_deleted();
        tracer.record_dir_deleted();

        assert_eq!(tracer.files_deleted(), 0);
        assert_eq!(tracer.dirs_deleted(), 3);
        assert_eq!(tracer.total_deleted(), 3);
    }

    #[test]
    fn test_record_mixed_deletions() {
        let mut tracer = DeleteTracer::new();
        tracer.record_file_deleted();
        tracer.record_dir_deleted();
        tracer.record_file_deleted();
        tracer.record_dir_deleted();

        assert_eq!(tracer.files_deleted(), 2);
        assert_eq!(tracer.dirs_deleted(), 2);
        assert_eq!(tracer.total_deleted(), 4);
    }

    #[test]
    fn test_record_skipped() {
        let mut tracer = DeleteTracer::new();
        tracer.record_skipped();
        tracer.record_skipped();

        assert_eq!(tracer.skipped(), 2);
    }

    #[test]
    fn test_record_error() {
        let mut tracer = DeleteTracer::new();
        tracer.record_error();
        tracer.record_error();
        tracer.record_error();

        assert_eq!(tracer.errors(), 3);
    }

    #[test]
    fn test_start_phase_sets_current_phase() {
        let mut tracer = DeleteTracer::new();
        tracer.start_phase(DeletePhase::Before);

        assert_eq!(tracer.current_phase(), Some(DeletePhase::Before));
    }

    #[test]
    fn test_start_phase_initializes_timing() {
        let mut tracer = DeleteTracer::new();
        tracer.start_phase(DeletePhase::During);

        std::thread::sleep(Duration::from_millis(1));
        assert!(tracer.phase_elapsed() > Duration::ZERO);
        assert!(tracer.session_elapsed() > Duration::ZERO);
    }

    #[test]
    fn test_end_phase_returns_elapsed() {
        let mut tracer = DeleteTracer::new();
        tracer.start_phase(DeletePhase::After);
        std::thread::sleep(Duration::from_millis(5));

        let elapsed = tracer.end_phase();
        assert!(elapsed >= Duration::from_millis(5));
    }

    #[test]
    fn test_end_phase_clears_phase_state() {
        let mut tracer = DeleteTracer::new();
        tracer.start_phase(DeletePhase::Before);
        tracer.end_phase();

        assert_eq!(tracer.current_phase(), None);
        assert_eq!(tracer.phase_elapsed(), Duration::ZERO);
    }

    #[test]
    fn test_multiple_phases() {
        let mut tracer = DeleteTracer::new();

        tracer.start_phase(DeletePhase::Before);
        tracer.record_file_deleted();
        tracer.end_phase();

        tracer.start_phase(DeletePhase::During);
        tracer.record_dir_deleted();
        tracer.record_file_deleted();
        tracer.end_phase();

        assert_eq!(tracer.files_deleted(), 2);
        assert_eq!(tracer.dirs_deleted(), 1);
        assert_eq!(tracer.total_deleted(), 3);
    }

    #[test]
    fn test_summary_returns_elapsed() {
        let mut tracer = DeleteTracer::new();
        tracer.start_phase(DeletePhase::Before);
        std::thread::sleep(Duration::from_millis(5));
        tracer.record_file_deleted();
        tracer.end_phase();

        let elapsed = tracer.summary();
        assert!(elapsed >= Duration::from_millis(5));
    }

    #[test]
    fn test_reset_clears_all_state() {
        let mut tracer = DeleteTracer::new();
        tracer.start_phase(DeletePhase::Before);
        tracer.record_file_deleted();
        tracer.record_dir_deleted();
        tracer.record_skipped();
        tracer.record_error();

        tracer.reset();

        assert_eq!(tracer.files_deleted(), 0);
        assert_eq!(tracer.dirs_deleted(), 0);
        assert_eq!(tracer.total_deleted(), 0);
        assert_eq!(tracer.skipped(), 0);
        assert_eq!(tracer.errors(), 0);
        assert_eq!(tracer.current_phase(), None);
        assert_eq!(tracer.phase_elapsed(), Duration::ZERO);
        assert_eq!(tracer.session_elapsed(), Duration::ZERO);
    }

    #[test]
    fn test_empty_deletion_session() {
        let mut tracer = DeleteTracer::new();
        tracer.start_phase(DeletePhase::Before);
        tracer.end_phase();

        assert_eq!(tracer.total_deleted(), 0);
        assert_eq!(tracer.skipped(), 0);
        assert_eq!(tracer.errors(), 0);
    }

    #[test]
    fn test_summary_without_phases() {
        let mut tracer = DeleteTracer::new();
        let elapsed = tracer.summary();

        assert_eq!(tracer.total_deleted(), 0);
        assert_eq!(elapsed, Duration::ZERO);
    }

    #[test]
    fn test_mixed_operations() {
        let mut tracer = DeleteTracer::new();
        tracer.start_phase(DeletePhase::Before);
        tracer.record_file_deleted();
        tracer.record_file_deleted();
        tracer.record_dir_deleted();
        tracer.record_skipped();
        tracer.record_error();
        tracer.end_phase();

        tracer.summary();

        assert_eq!(tracer.files_deleted(), 2);
        assert_eq!(tracer.dirs_deleted(), 1);
        assert_eq!(tracer.total_deleted(), 3);
        assert_eq!(tracer.skipped(), 1);
        assert_eq!(tracer.errors(), 1);
    }

    #[test]
    fn test_trace_functions_do_not_panic() {
        // All trace functions should be callable without panicking
        trace_delete_phase_start(DeletePhase::Before);
        trace_delete_phase_end(DeletePhase::Before, 42, Duration::from_millis(100));
        trace_delete_file("test.txt", false);
        trace_delete_file("testdir", true);
        trace_delete_skipped("skip.txt", "max-delete limit");
        trace_delete_error("error.txt", "permission denied");
        trace_delete_summary(10, 2, 1, Duration::from_secs(1));
    }

    #[test]
    fn test_end_phase_without_start_phase() {
        let mut tracer = DeleteTracer::new();
        let elapsed = tracer.end_phase();

        assert_eq!(elapsed, Duration::ZERO);
        assert_eq!(tracer.current_phase(), None);
    }

    #[test]
    fn test_phase_change() {
        let mut tracer = DeleteTracer::new();
        tracer.start_phase(DeletePhase::Before);
        assert_eq!(tracer.current_phase(), Some(DeletePhase::Before));

        tracer.start_phase(DeletePhase::During);
        assert_eq!(tracer.current_phase(), Some(DeletePhase::During));
    }

    #[test]
    fn test_large_deletion_counts() {
        let mut tracer = DeleteTracer::new();
        tracer.start_phase(DeletePhase::Before);

        for _ in 0..10_000 {
            tracer.record_file_deleted();
        }

        for _ in 0..5_000 {
            tracer.record_dir_deleted();
        }

        tracer.end_phase();

        assert_eq!(tracer.files_deleted(), 10_000);
        assert_eq!(tracer.dirs_deleted(), 5_000);
        assert_eq!(tracer.total_deleted(), 15_000);
    }

    #[test]
    fn test_zero_deletions() {
        let mut tracer = DeleteTracer::new();
        tracer.start_phase(DeletePhase::Before);
        tracer.end_phase();

        assert_eq!(tracer.total_deleted(), 0);
        assert_eq!(tracer.files_deleted(), 0);
        assert_eq!(tracer.dirs_deleted(), 0);
    }

    #[cfg(feature = "tracing")]
    #[test]
    fn test_tracing_feature_enabled() {
        // When tracing feature is enabled, verify the functions compile and run
        // without panicking. We can't easily verify event emission without
        // tracing-subscriber, but this at least confirms the code compiles.
        let mut tracer = DeleteTracer::new();
        tracer.start_phase(DeletePhase::Before);
        tracer.record_file_deleted();
        tracer.record_dir_deleted();
        tracer.end_phase();

        tracer.summary();

        // Verify the tracer still tracks stats correctly
        assert_eq!(tracer.files_deleted(), 1);
        assert_eq!(tracer.dirs_deleted(), 1);
        assert_eq!(tracer.total_deleted(), 2);
    }
}
