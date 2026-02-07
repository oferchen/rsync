//! DEBUG_FILTER tracing for filter/exclude operations.
//!
//! This module provides structured tracing for filter rule evaluation operations
//! that match upstream rsync's exclude.c debug output format. All tracing is
//! conditionally compiled behind the `tracing` feature flag and produces no-op
//! inline functions when disabled.
//!
//! # Examples
//!
//! ```rust,ignore
//! use filters::debug_filter::{FilterTracer, trace_filter_rule_added};
//!
//! let mut tracer = FilterTracer::new();
//!
//! trace_filter_rule_added("*.tmp", false, false);
//! tracer.record_rule_added();
//!
//! trace_filter_evaluate("test.tmp", "*.tmp", false, true);
//! tracer.record_evaluation(false);
//!
//! tracer.summary();
//! ```

/// Target name for tracing events, matching rsync's debug category.
#[cfg(feature = "tracing")]
const FILTER_TARGET: &str = "rsync::filter";

// ============================================================================
// Tracing functions (feature-gated)
// ============================================================================

/// Traces a filter rule being added to the filter set.
///
/// Emits a tracing event when a new include/exclude rule is registered.
/// In upstream rsync, this corresponds to debug output when rules are parsed.
///
/// # Arguments
///
/// * `pattern` - The filter pattern (e.g., "*.tmp", "/var/log/")
/// * `is_include` - Whether this is an include rule (true) or exclude rule (false)
/// * `is_dir_only` - Whether this rule applies only to directories
#[cfg(feature = "tracing")]
#[inline]
pub fn trace_filter_rule_added(pattern: &str, is_include: bool, is_dir_only: bool) {
    tracing::debug!(
        target: FILTER_TARGET,
        pattern = %pattern,
        is_include = is_include,
        is_dir_only = is_dir_only,
        "filter_rule_added"
    );
}

/// No-op when tracing is disabled.
#[cfg(not(feature = "tracing"))]
#[inline]
pub fn trace_filter_rule_added(_pattern: &str, _is_include: bool, _is_dir_only: bool) {}

/// Traces evaluation of a path against a specific filter rule.
///
/// Logs the matching process for each rule evaluated against a path,
/// showing which patterns were tested and whether they matched.
///
/// # Arguments
///
/// * `path` - The path being evaluated (e.g., "src/test.tmp")
/// * `rule_pattern` - The pattern being tested against the path
/// * `is_include` - Whether this is an include rule
/// * `matched` - Whether the rule matched the path
#[cfg(feature = "tracing")]
#[inline]
pub fn trace_filter_evaluate(path: &str, rule_pattern: &str, is_include: bool, matched: bool) {
    tracing::trace!(
        target: FILTER_TARGET,
        path = %path,
        rule_pattern = %rule_pattern,
        is_include = is_include,
        matched = matched,
        "filter_evaluate"
    );
}

/// No-op when tracing is disabled.
#[cfg(not(feature = "tracing"))]
#[inline]
pub fn trace_filter_evaluate(_path: &str, _rule_pattern: &str, _is_include: bool, _matched: bool) {}

/// Traces the final decision for a path after all rules have been evaluated.
///
/// Emits the final include/exclude decision for a path, including which rule
/// (if any) made the decision.
///
/// # Arguments
///
/// * `path` - The path that was evaluated
/// * `included` - Whether the path was included (true) or excluded (false)
/// * `matching_rule` - The pattern of the rule that made the decision, if any
#[cfg(feature = "tracing")]
#[inline]
pub fn trace_filter_decision(path: &str, included: bool, matching_rule: Option<&str>) {
    tracing::info!(
        target: FILTER_TARGET,
        path = %path,
        included = included,
        matching_rule = ?matching_rule,
        "filter_decision"
    );
}

/// No-op when tracing is disabled.
#[cfg(not(feature = "tracing"))]
#[inline]
pub fn trace_filter_decision(_path: &str, _included: bool, _matching_rule: Option<&str>) {}

/// Traces loading of a per-directory merge file.
///
/// Logs when a directory-specific filter file (like .gitignore) is loaded
/// and how many rules were parsed from it.
///
/// # Arguments
///
/// * `dir` - The directory containing the merge file
/// * `rules_file` - The name of the rules file being loaded
/// * `rule_count` - Number of rules successfully loaded from the file
#[cfg(feature = "tracing")]
#[inline]
pub fn trace_dir_merge_load(dir: &str, rules_file: &str, rule_count: usize) {
    tracing::debug!(
        target: FILTER_TARGET,
        dir = %dir,
        rules_file = %rules_file,
        rule_count = rule_count,
        "dir_merge_load"
    );
}

/// No-op when tracing is disabled.
#[cfg(not(feature = "tracing"))]
#[inline]
pub fn trace_dir_merge_load(_dir: &str, _rules_file: &str, _rule_count: usize) {}

/// Traces summary statistics for filter operations.
///
/// Emits aggregate statistics showing how many paths were evaluated,
/// how many were included vs excluded, and the overall filtering efficiency.
///
/// # Arguments
///
/// * `total_evaluated` - Total number of paths evaluated
/// * `total_included` - Number of paths that were included
/// * `total_excluded` - Number of paths that were excluded
#[cfg(feature = "tracing")]
#[inline]
pub fn trace_filter_summary(total_evaluated: usize, total_included: usize, total_excluded: usize) {
    tracing::info!(
        target: FILTER_TARGET,
        total_evaluated = total_evaluated,
        total_included = total_included,
        total_excluded = total_excluded,
        include_ratio = if total_evaluated > 0 {
            (total_included as f64) / (total_evaluated as f64)
        } else {
            0.0
        },
        "filter_summary"
    );
}

/// No-op when tracing is disabled.
#[cfg(not(feature = "tracing"))]
#[inline]
pub fn trace_filter_summary(
    _total_evaluated: usize,
    _total_included: usize,
    _total_excluded: usize,
) {
}

// ============================================================================
// FilterTracer - stateful tracer for aggregating filter statistics
// ============================================================================

/// Aggregates statistics during filter operations.
///
/// Tracks rule counts, evaluation statistics, and merge file operations across
/// an entire filtering session. Use this when you need to accumulate stats
/// across multiple filter evaluations before emitting final summary events.
///
/// # Examples
///
/// ```no_run
/// # use filters::debug_filter::FilterTracer;
/// let mut tracer = FilterTracer::new();
///
/// tracer.record_rule_added();
/// tracer.record_rule_added();
///
/// tracer.record_evaluation(true);  // included
/// tracer.record_evaluation(false); // excluded
/// tracer.record_evaluation(true);  // included
///
/// tracer.summary();
/// assert_eq!(tracer.rules_added(), 2);
/// assert_eq!(tracer.total_evaluated(), 3);
/// assert_eq!(tracer.total_included(), 2);
/// assert_eq!(tracer.total_excluded(), 1);
/// ```
#[derive(Debug, Clone)]
pub struct FilterTracer {
    rules_added: usize,
    total_evaluated: usize,
    total_included: usize,
    total_excluded: usize,
    dir_merges: usize,
}

impl Default for FilterTracer {
    fn default() -> Self {
        Self::new()
    }
}

impl FilterTracer {
    /// Creates a new filter tracer with zero counts.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            rules_added: 0,
            total_evaluated: 0,
            total_included: 0,
            total_excluded: 0,
            dir_merges: 0,
        }
    }

    /// Records that a filter rule was added to the filter set.
    pub fn record_rule_added(&mut self) {
        self.rules_added += 1;
    }

    /// Records an evaluation result, incrementing include or exclude count.
    ///
    /// # Arguments
    ///
    /// * `included` - Whether the path was included (true) or excluded (false)
    pub fn record_evaluation(&mut self, included: bool) {
        self.total_evaluated += 1;
        if included {
            self.total_included += 1;
        } else {
            self.total_excluded += 1;
        }
    }

    /// Records a directory merge operation.
    ///
    /// # Arguments
    ///
    /// * `rule_count` - Number of rules loaded from the merge file
    pub fn record_dir_merge(&mut self, rule_count: usize) {
        self.dir_merges += 1;
        self.rules_added += rule_count;
    }

    /// Emits a summary trace event with all accumulated statistics.
    pub fn summary(&self) {
        trace_filter_summary(
            self.total_evaluated,
            self.total_included,
            self.total_excluded,
        );
    }

    /// Resets all counters to zero.
    pub fn reset(&mut self) {
        self.rules_added = 0;
        self.total_evaluated = 0;
        self.total_included = 0;
        self.total_excluded = 0;
        self.dir_merges = 0;
    }

    /// Returns the number of rules added.
    #[must_use]
    pub const fn rules_added(&self) -> usize {
        self.rules_added
    }

    /// Returns the total number of paths evaluated.
    #[must_use]
    pub const fn total_evaluated(&self) -> usize {
        self.total_evaluated
    }

    /// Returns the number of paths included.
    #[must_use]
    pub const fn total_included(&self) -> usize {
        self.total_included
    }

    /// Returns the number of paths excluded.
    #[must_use]
    pub const fn total_excluded(&self) -> usize {
        self.total_excluded
    }

    /// Returns the number of directory merge operations.
    #[must_use]
    pub const fn dir_merges(&self) -> usize {
        self.dir_merges
    }

    /// Returns the ratio of included paths to total evaluated paths.
    ///
    /// Returns 0.0 if no paths have been evaluated.
    #[must_use]
    pub fn include_ratio(&self) -> f64 {
        if self.total_evaluated == 0 {
            0.0
        } else {
            (self.total_included as f64) / (self.total_evaluated as f64)
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
        let tracer = FilterTracer::new();
        assert_eq!(tracer.rules_added(), 0);
        assert_eq!(tracer.total_evaluated(), 0);
        assert_eq!(tracer.total_included(), 0);
        assert_eq!(tracer.total_excluded(), 0);
        assert_eq!(tracer.dir_merges(), 0);
    }

    #[test]
    fn test_tracer_default() {
        let tracer = FilterTracer::default();
        assert_eq!(tracer.rules_added(), 0);
        assert_eq!(tracer.total_evaluated(), 0);
        assert_eq!(tracer.total_included(), 0);
        assert_eq!(tracer.total_excluded(), 0);
    }

    #[test]
    fn test_record_rule_added() {
        let mut tracer = FilterTracer::new();
        tracer.record_rule_added();
        tracer.record_rule_added();
        tracer.record_rule_added();

        assert_eq!(tracer.rules_added(), 3);
    }

    #[test]
    fn test_record_evaluation_included() {
        let mut tracer = FilterTracer::new();
        tracer.record_evaluation(true);
        tracer.record_evaluation(true);

        assert_eq!(tracer.total_evaluated(), 2);
        assert_eq!(tracer.total_included(), 2);
        assert_eq!(tracer.total_excluded(), 0);
    }

    #[test]
    fn test_record_evaluation_excluded() {
        let mut tracer = FilterTracer::new();
        tracer.record_evaluation(false);
        tracer.record_evaluation(false);

        assert_eq!(tracer.total_evaluated(), 2);
        assert_eq!(tracer.total_included(), 0);
        assert_eq!(tracer.total_excluded(), 2);
    }

    #[test]
    fn test_record_evaluation_mixed() {
        let mut tracer = FilterTracer::new();
        tracer.record_evaluation(true); // include
        tracer.record_evaluation(false); // exclude
        tracer.record_evaluation(true); // include
        tracer.record_evaluation(false); // exclude
        tracer.record_evaluation(true); // include

        assert_eq!(tracer.total_evaluated(), 5);
        assert_eq!(tracer.total_included(), 3);
        assert_eq!(tracer.total_excluded(), 2);
    }

    #[test]
    fn test_record_dir_merge() {
        let mut tracer = FilterTracer::new();
        tracer.record_dir_merge(5);
        tracer.record_dir_merge(3);

        assert_eq!(tracer.dir_merges(), 2);
        assert_eq!(tracer.rules_added(), 8);
    }

    #[test]
    fn test_include_ratio_zero_evaluations() {
        let tracer = FilterTracer::new();
        assert_eq!(tracer.include_ratio(), 0.0);
    }

    #[test]
    fn test_include_ratio_all_included() {
        let mut tracer = FilterTracer::new();
        tracer.record_evaluation(true);
        tracer.record_evaluation(true);
        tracer.record_evaluation(true);

        assert_eq!(tracer.include_ratio(), 1.0);
    }

    #[test]
    fn test_include_ratio_all_excluded() {
        let mut tracer = FilterTracer::new();
        tracer.record_evaluation(false);
        tracer.record_evaluation(false);

        assert_eq!(tracer.include_ratio(), 0.0);
    }

    #[test]
    fn test_include_ratio_half() {
        let mut tracer = FilterTracer::new();
        tracer.record_evaluation(true);
        tracer.record_evaluation(false);

        assert!((tracer.include_ratio() - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn test_reset() {
        let mut tracer = FilterTracer::new();
        tracer.record_rule_added();
        tracer.record_evaluation(true);
        tracer.record_evaluation(false);
        tracer.record_dir_merge(5);

        tracer.reset();

        assert_eq!(tracer.rules_added(), 0);
        assert_eq!(tracer.total_evaluated(), 0);
        assert_eq!(tracer.total_included(), 0);
        assert_eq!(tracer.total_excluded(), 0);
        assert_eq!(tracer.dir_merges(), 0);
        assert_eq!(tracer.include_ratio(), 0.0);
    }

    #[test]
    fn test_summary_stats_correct() {
        let mut tracer = FilterTracer::new();
        tracer.record_rule_added();
        tracer.record_rule_added();
        tracer.record_evaluation(true);
        tracer.record_evaluation(true);
        tracer.record_evaluation(false);
        tracer.record_dir_merge(3);

        assert_eq!(tracer.rules_added(), 5); // 2 + 3 from merge
        assert_eq!(tracer.total_evaluated(), 3);
        assert_eq!(tracer.total_included(), 2);
        assert_eq!(tracer.total_excluded(), 1);
        assert_eq!(tracer.dir_merges(), 1);

        let ratio = tracer.include_ratio();
        assert!((ratio - (2.0 / 3.0)).abs() < f64::EPSILON);
    }

    #[test]
    fn test_trace_functions_do_not_panic() {
        // All trace functions should be callable without panicking
        trace_filter_rule_added("*.tmp", false, false);
        trace_filter_rule_added("/var/log/", false, true);
        trace_filter_rule_added("important/", true, true);

        trace_filter_evaluate("test.tmp", "*.tmp", false, true);
        trace_filter_evaluate("src/main.rs", "*.rs", true, true);

        trace_filter_decision("file.txt", true, Some("*.txt"));
        trace_filter_decision("excluded.tmp", false, Some("*.tmp"));
        trace_filter_decision("default.log", true, None);

        trace_dir_merge_load("/home/user", ".rsyncignore", 5);
        trace_dir_merge_load("/var/www", ".gitignore", 12);

        trace_filter_summary(100, 75, 25);
        trace_filter_summary(0, 0, 0);
    }

    #[test]
    fn test_summary_emits_trace() {
        let mut tracer = FilterTracer::new();
        tracer.record_evaluation(true);
        tracer.record_evaluation(false);

        // Should not panic
        tracer.summary();
    }

    #[test]
    fn test_large_evaluation_counts() {
        let mut tracer = FilterTracer::new();

        for i in 0..10_000 {
            tracer.record_evaluation(i % 3 != 0); // 2/3 included
        }

        assert_eq!(tracer.total_evaluated(), 10_000);
        assert_eq!(tracer.total_included(), 6_666); // 0,1,2 -> true,true,false pattern
        assert_eq!(tracer.total_excluded(), 3_334);

        let ratio = tracer.include_ratio();
        assert!((ratio - 0.6666).abs() < 0.001);
    }

    #[test]
    fn test_multiple_operations() {
        let mut tracer = FilterTracer::new();

        // First batch
        tracer.record_rule_added();
        tracer.record_evaluation(true);
        tracer.record_evaluation(false);

        // Reset and second batch
        tracer.reset();
        tracer.record_rule_added();
        tracer.record_rule_added();
        tracer.record_evaluation(true);

        assert_eq!(tracer.rules_added(), 2);
        assert_eq!(tracer.total_evaluated(), 1);
        assert_eq!(tracer.total_included(), 1);
        assert_eq!(tracer.total_excluded(), 0);
    }

    #[cfg(feature = "tracing")]
    #[test]
    fn test_tracing_feature_enabled() {
        // When tracing feature is enabled, verify the functions compile and run
        // without panicking. We can't easily verify event emission without
        // tracing-subscriber, but this at least confirms the code compiles.
        let mut tracer = FilterTracer::new();

        trace_filter_rule_added("*.log", false, false);
        tracer.record_rule_added();

        trace_filter_evaluate("test.log", "*.log", false, true);
        tracer.record_evaluation(false);

        trace_filter_decision("test.log", false, Some("*.log"));
        tracer.summary();

        assert_eq!(tracer.rules_added(), 1);
        assert_eq!(tracer.total_evaluated(), 1);
        assert_eq!(tracer.total_excluded(), 1);
    }

    #[test]
    fn test_dir_merge_with_zero_rules() {
        let mut tracer = FilterTracer::new();
        tracer.record_dir_merge(0);

        assert_eq!(tracer.dir_merges(), 1);
        assert_eq!(tracer.rules_added(), 0);
    }

    #[test]
    fn test_clone() {
        let mut tracer = FilterTracer::new();
        tracer.record_rule_added();
        tracer.record_evaluation(true);

        let cloned = tracer.clone();

        assert_eq!(cloned.rules_added(), 1);
        assert_eq!(cloned.total_evaluated(), 1);
        assert_eq!(cloned.total_included(), 1);
    }
}
