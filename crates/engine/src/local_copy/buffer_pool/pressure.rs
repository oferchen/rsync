//! Adaptive pool resizing based on allocation pressure.
//!
//! Tracks hit/miss rates using atomic counters and periodically adjusts the
//! pool's soft capacity. A "hit" occurs when a buffer is obtained from the
//! central pool or thread-local cache. A "miss" occurs when a fresh allocation
//! is required because no pooled buffer was available.
//!
//! # Resize Policy
//!
//! - **Grow**: When the miss rate exceeds [`MISS_RATE_GROW_THRESHOLD`] (20%),
//!   the pool capacity is doubled (up to [`MAX_CAPACITY`]).
//! - **Shrink**: When utilization drops below [`UTILIZATION_SHRINK_THRESHOLD`]
//!   (30% of capacity occupied), the pool capacity is halved (down to
//!   [`MIN_CAPACITY`]).
//!
//! # Amortized Checks
//!
//! Stats are evaluated every [`CHECK_INTERVAL`] operations (64 by default).
//! Between checks, only two `Relaxed` atomic increments are performed per
//! acquire - negligible overhead on the hot path.

use std::sync::atomic::{AtomicU64, Ordering};

/// Number of acquire operations between resize evaluations.
///
/// Chosen to amortize the cost of capacity checks while still reacting
/// within a few hundred operations. Must be a power of two for efficient
/// modular arithmetic via bitwise AND.
const CHECK_INTERVAL: u64 = 64;

/// Miss rate threshold above which the pool grows (20%).
///
/// If more than 20% of acquires in the last interval required a fresh
/// allocation, the pool is undersized.
const MISS_RATE_GROW_THRESHOLD: f64 = 0.20;

/// Utilization threshold below which the pool shrinks (30%).
///
/// If fewer than 30% of pool slots are occupied after a check interval,
/// the pool is oversized and wasting memory.
const UTILIZATION_SHRINK_THRESHOLD: f64 = 0.30;

/// Minimum pool capacity after shrinking.
///
/// The pool never shrinks below this to avoid degenerate behavior with
/// very small pools.
const MIN_CAPACITY: usize = 2;

/// Maximum pool capacity after growing.
///
/// Prevents unbounded growth. At 128 KB per buffer (default), 256 buffers
/// = 32 MB of pooled memory.
const MAX_CAPACITY: usize = 256;

/// Growth factor applied when the miss rate exceeds the threshold.
const GROW_FACTOR: usize = 2;

/// Shrink factor applied when utilization drops below the threshold.
const SHRINK_DIVISOR: usize = 2;

/// Allocation pressure tracker for adaptive pool resizing.
///
/// Uses atomic counters to track pool hits (buffer reused from pool) and
/// misses (fresh allocation required). The counters are reset after each
/// evaluation to measure pressure within the current interval only.
///
/// All operations use `Relaxed` ordering because exact precision is not
/// required - the resize policy is a heuristic that tolerates small
/// counting errors under concurrent access.
#[derive(Debug)]
pub(super) struct PressureTracker {
    /// Number of successful pool acquisitions (buffer obtained from pool).
    hits: AtomicU64,
    /// Number of pool misses (fresh allocation required).
    misses: AtomicU64,
    /// Total acquire operations since last evaluation.
    ops: AtomicU64,
}

impl PressureTracker {
    /// Creates a new pressure tracker with all counters at zero.
    pub(super) fn new() -> Self {
        Self {
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
            ops: AtomicU64::new(0),
        }
    }

    /// Records a pool hit (buffer obtained without fresh allocation).
    pub(super) fn record_hit(&self) {
        self.hits.fetch_add(1, Ordering::Relaxed);
        self.ops.fetch_add(1, Ordering::Relaxed);
    }

    /// Records a pool miss (fresh allocation required).
    pub(super) fn record_miss(&self) {
        self.misses.fetch_add(1, Ordering::Relaxed);
        self.ops.fetch_add(1, Ordering::Relaxed);
    }

    /// Returns `true` if enough operations have elapsed to warrant a resize
    /// evaluation.
    ///
    /// Uses bitwise AND for efficient modular check (CHECK_INTERVAL is a
    /// power of two).
    pub(super) fn should_check(&self) -> bool {
        let ops = self.ops.load(Ordering::Relaxed);
        ops > 0 && (ops & (CHECK_INTERVAL - 1)) == 0
    }

    /// Evaluates the current pressure and returns a resize recommendation.
    ///
    /// Resets all counters after evaluation so the next interval starts fresh.
    ///
    /// # Arguments
    ///
    /// * `current_capacity` - The pool's current soft capacity.
    /// * `current_available` - Number of buffers currently in the central pool.
    pub(super) fn evaluate(
        &self,
        current_capacity: usize,
        current_available: usize,
    ) -> ResizeAction {
        let hits = self.hits.swap(0, Ordering::Relaxed);
        let misses = self.misses.swap(0, Ordering::Relaxed);
        let total = self.ops.swap(0, Ordering::Relaxed);

        if total == 0 {
            return ResizeAction::Hold;
        }

        let miss_rate = misses as f64 / total as f64;

        // High miss rate: pool is too small.
        if miss_rate > MISS_RATE_GROW_THRESHOLD {
            let new_capacity = (current_capacity * GROW_FACTOR).min(MAX_CAPACITY);
            if new_capacity > current_capacity {
                return ResizeAction::Grow(new_capacity);
            }
            return ResizeAction::Hold;
        }

        // Low utilization: pool is too large.
        let utilization = if current_capacity > 0 {
            current_available as f64 / current_capacity as f64
        } else {
            0.0
        };

        // Only shrink when the pool is mostly idle AND miss rate is low.
        // The low miss rate check prevents shrinking when all buffers are
        // checked out (available = 0, utilization = 0) but demand is high.
        let _ = hits; // Suppress unused warning; hits contribute to total.
        if utilization < UTILIZATION_SHRINK_THRESHOLD && miss_rate < MISS_RATE_GROW_THRESHOLD / 2.0
        {
            let new_capacity = (current_capacity / SHRINK_DIVISOR).max(MIN_CAPACITY);
            if new_capacity < current_capacity {
                return ResizeAction::Shrink(new_capacity);
            }
        }

        ResizeAction::Hold
    }

    /// Returns the current hit count (for diagnostics/testing).
    #[cfg(test)]
    pub(super) fn hits(&self) -> u64 {
        self.hits.load(Ordering::Relaxed)
    }

    /// Returns the current miss count (for diagnostics/testing).
    #[cfg(test)]
    pub(super) fn misses(&self) -> u64 {
        self.misses.load(Ordering::Relaxed)
    }

    /// Returns the current operation count (for diagnostics/testing).
    #[cfg(test)]
    pub(super) fn ops(&self) -> u64 {
        self.ops.load(Ordering::Relaxed)
    }
}

/// Resize recommendation from pressure evaluation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ResizeAction {
    /// Keep current capacity.
    Hold,
    /// Grow to the specified new capacity.
    Grow(usize),
    /// Shrink to the specified new capacity.
    Shrink(usize),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_tracker_has_zero_counters() {
        let tracker = PressureTracker::new();
        assert_eq!(tracker.hits(), 0);
        assert_eq!(tracker.misses(), 0);
        assert_eq!(tracker.ops(), 0);
    }

    #[test]
    fn record_hit_increments_counters() {
        let tracker = PressureTracker::new();
        tracker.record_hit();
        assert_eq!(tracker.hits(), 1);
        assert_eq!(tracker.misses(), 0);
        assert_eq!(tracker.ops(), 1);
    }

    #[test]
    fn record_miss_increments_counters() {
        let tracker = PressureTracker::new();
        tracker.record_miss();
        assert_eq!(tracker.hits(), 0);
        assert_eq!(tracker.misses(), 1);
        assert_eq!(tracker.ops(), 1);
    }

    #[test]
    fn should_check_at_interval_boundary() {
        let tracker = PressureTracker::new();
        for i in 1..CHECK_INTERVAL {
            tracker.record_hit();
            if i < CHECK_INTERVAL {
                // Not at boundary yet (except at CHECK_INTERVAL itself).
            }
        }
        assert!(!tracker.should_check());
        tracker.record_hit();
        assert!(tracker.should_check());
    }

    #[test]
    fn should_check_false_at_zero() {
        let tracker = PressureTracker::new();
        assert!(!tracker.should_check());
    }

    #[test]
    fn evaluate_hold_with_all_hits() {
        let tracker = PressureTracker::new();
        for _ in 0..CHECK_INTERVAL {
            tracker.record_hit();
        }
        // All hits, capacity 8, 6 available (75% utilization) - hold.
        let action = tracker.evaluate(8, 6);
        assert_eq!(action, ResizeAction::Hold);
    }

    #[test]
    fn evaluate_grow_with_high_miss_rate() {
        let tracker = PressureTracker::new();
        // 30% miss rate (above 20% threshold).
        for _ in 0..70 {
            tracker.record_hit();
        }
        for _ in 0..30 {
            tracker.record_miss();
        }
        let action = tracker.evaluate(8, 4);
        assert_eq!(action, ResizeAction::Grow(16));
    }

    #[test]
    fn evaluate_grow_capped_at_max() {
        let tracker = PressureTracker::new();
        for _ in 0..50 {
            tracker.record_miss();
        }
        let action = tracker.evaluate(MAX_CAPACITY, 0);
        assert_eq!(action, ResizeAction::Hold);
    }

    #[test]
    fn evaluate_shrink_with_low_utilization() {
        let tracker = PressureTracker::new();
        // All hits, low miss rate, low utilization.
        for _ in 0..CHECK_INTERVAL {
            tracker.record_hit();
        }
        // 16 capacity, 2 available = 12.5% utilization.
        let action = tracker.evaluate(16, 2);
        assert_eq!(action, ResizeAction::Shrink(8));
    }

    #[test]
    fn evaluate_shrink_capped_at_min() {
        let tracker = PressureTracker::new();
        for _ in 0..CHECK_INTERVAL {
            tracker.record_hit();
        }
        let action = tracker.evaluate(MIN_CAPACITY, 0);
        assert_eq!(action, ResizeAction::Hold);
    }

    #[test]
    fn evaluate_no_shrink_when_miss_rate_moderate() {
        let tracker = PressureTracker::new();
        // 15% miss rate - below grow threshold but above shrink guard.
        for _ in 0..85 {
            tracker.record_hit();
        }
        for _ in 0..15 {
            tracker.record_miss();
        }
        // Low utilization but moderate misses - should hold.
        let action = tracker.evaluate(16, 2);
        assert_eq!(action, ResizeAction::Hold);
    }

    #[test]
    fn evaluate_resets_counters() {
        let tracker = PressureTracker::new();
        for _ in 0..50 {
            tracker.record_hit();
        }
        let _ = tracker.evaluate(8, 4);
        assert_eq!(tracker.hits(), 0);
        assert_eq!(tracker.misses(), 0);
        assert_eq!(tracker.ops(), 0);
    }

    #[test]
    fn evaluate_zero_ops_returns_hold() {
        let tracker = PressureTracker::new();
        let action = tracker.evaluate(8, 4);
        assert_eq!(action, ResizeAction::Hold);
    }

    #[test]
    fn evaluate_zero_capacity_no_panic() {
        let tracker = PressureTracker::new();
        for _ in 0..10 {
            tracker.record_miss();
        }
        let action = tracker.evaluate(0, 0);
        // Cannot grow from 0 (0 * 2 = 0).
        assert_eq!(action, ResizeAction::Hold);
    }

    #[test]
    fn concurrent_hit_miss_tracking() {
        use std::sync::Arc;
        use std::thread;

        let tracker = Arc::new(PressureTracker::new());
        let thread_count = 8;
        let iterations = 1000;

        let handles: Vec<_> = (0..thread_count)
            .map(|id| {
                let t = Arc::clone(&tracker);
                thread::spawn(move || {
                    for _ in 0..iterations {
                        if id % 2 == 0 {
                            t.record_hit();
                        } else {
                            t.record_miss();
                        }
                    }
                })
            })
            .collect();

        for h in handles {
            h.join().expect("thread panicked");
        }

        let total = tracker.ops();
        assert_eq!(total, thread_count as u64 * iterations);
    }

    #[test]
    fn check_interval_is_power_of_two() {
        assert!(CHECK_INTERVAL.is_power_of_two());
    }

    #[test]
    fn grow_factor_doubles_capacity() {
        let tracker = PressureTracker::new();
        for _ in 0..100 {
            tracker.record_miss();
        }
        let action = tracker.evaluate(4, 0);
        assert_eq!(action, ResizeAction::Grow(8));
    }

    #[test]
    fn shrink_halves_capacity() {
        let tracker = PressureTracker::new();
        for _ in 0..CHECK_INTERVAL {
            tracker.record_hit();
        }
        let action = tracker.evaluate(32, 4);
        assert_eq!(action, ResizeAction::Shrink(16));
    }
}
