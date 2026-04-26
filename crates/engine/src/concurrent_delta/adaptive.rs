//! Adaptive capacity scaling policy for `ReorderBuffer`.
//!
//! Under skewed workloads a fixed reorder-buffer capacity wastes memory or
//! stalls producers. [`AdaptiveCapacityPolicy`] grows the ring under
//! sustained pressure (utilization >= 80% AND gap window > capacity/2) and
//! shrinks it back toward `min` once a rolling window of inserts averages
//! below 25% utilization. The policy never breaches `[min, max]`.

/// Default number of insert samples averaged when deciding to shrink.
pub const DEFAULT_SAMPLE_WINDOW: usize = 32;

/// Utilization threshold above which the buffer is considered "hot".
const GROW_UTILIZATION_THRESHOLD: f32 = 0.80;

/// Average utilization threshold below which the buffer is considered "idle".
const SHRINK_UTILIZATION_THRESHOLD: f32 = 0.25;

/// Adaptive capacity policy bounds and growth behaviour.
///
/// Composed into a `ReorderBuffer` via `ReorderBuffer::with_adaptive_policy`.
#[derive(Debug, Clone, Copy)]
pub struct AdaptiveCapacityPolicy {
    /// Minimum (and starting) capacity. Must be >= 1.
    pub min: usize,
    /// Maximum capacity the ring is allowed to reach.
    pub max: usize,
    /// Multiplicative grow / shrink factor. Must be > 1.0.
    pub growth_factor: f32,
    /// Number of consecutive insert samples averaged to decide shrinks.
    pub sample_window: usize,
}

impl AdaptiveCapacityPolicy {
    /// Builds a policy with the default shrink-decision window.
    ///
    /// # Panics
    ///
    /// Panics if `min == 0`, `max < min`, or `growth_factor <= 1.0`.
    #[must_use]
    pub fn new(min: usize, max: usize, growth_factor: f32) -> Self {
        Self::with_window(min, max, growth_factor, DEFAULT_SAMPLE_WINDOW)
    }

    /// Builds a policy with an explicit shrink-decision window length.
    ///
    /// # Panics
    ///
    /// Panics if `min == 0`, `max < min`, `growth_factor <= 1.0`, or
    /// `sample_window == 0`.
    #[must_use]
    pub fn with_window(min: usize, max: usize, growth_factor: f32, sample_window: usize) -> Self {
        assert!(min > 0, "adaptive policy min capacity must be non-zero");
        assert!(max >= min, "adaptive policy max must be >= min");
        assert!(
            growth_factor > 1.0 && growth_factor.is_finite(),
            "adaptive policy growth_factor must be a finite value > 1.0"
        );
        assert!(
            sample_window > 0,
            "adaptive policy sample_window must be non-zero"
        );
        Self {
            min,
            max,
            growth_factor,
            sample_window,
        }
    }

    /// Returns the next capacity when growing from `current`, clamped to `max`.
    #[must_use]
    pub(crate) fn next_grow(&self, current: usize) -> usize {
        let scaled = (current as f32 * self.growth_factor).ceil() as usize;
        scaled.max(current + 1).min(self.max)
    }

    /// Returns the next capacity when shrinking from `current`, clamped to
    /// the larger of `min` and `floor`.
    #[must_use]
    pub(crate) fn next_shrink(&self, current: usize, floor: usize) -> usize {
        let scaled = (current as f32 / self.growth_factor).floor() as usize;
        scaled.max(self.min).max(floor).min(current)
    }
}

/// Snapshot of adaptive capacity counters exposed via
/// [`ReorderBuffer::stats`](super::reorder::ReorderBuffer::stats).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReorderStats {
    /// Number of times the ring buffer grew due to the adaptive policy.
    pub grow_events: u64,
    /// Number of times the ring buffer shrank due to the adaptive policy.
    pub shrink_events: u64,
    /// Current capacity of the ring buffer.
    pub capacity: usize,
}

/// Mutable runtime state for the adaptive policy. Composed inside
/// [`ReorderBuffer`](super::reorder::ReorderBuffer).
#[derive(Debug)]
pub(crate) struct AdaptiveState {
    pub(crate) policy: AdaptiveCapacityPolicy,
    pub(crate) grow_events: u64,
    pub(crate) shrink_events: u64,
    /// Recent per-insert utilization samples in [0.0, 1.0].
    samples: Vec<f32>,
    /// Insertion index for the circular sample window.
    sample_cursor: usize,
}

impl AdaptiveState {
    pub(crate) fn new(policy: AdaptiveCapacityPolicy) -> Self {
        Self {
            policy,
            grow_events: 0,
            shrink_events: 0,
            samples: Vec::with_capacity(policy.sample_window),
            sample_cursor: 0,
        }
    }

    /// Records a utilization sample (count / capacity).
    pub(crate) fn record_sample(&mut self, utilization: f32) {
        let window = self.policy.sample_window;
        if self.samples.len() < window {
            self.samples.push(utilization);
        } else {
            self.samples[self.sample_cursor] = utilization;
        }
        self.sample_cursor = (self.sample_cursor + 1) % window;
    }

    /// Returns `true` when the rolling window is full and its mean is below
    /// the shrink threshold.
    pub(crate) fn should_shrink(&self) -> bool {
        if self.samples.len() < self.policy.sample_window {
            return false;
        }
        let sum: f32 = self.samples.iter().sum();
        let mean = sum / self.samples.len() as f32;
        mean < SHRINK_UTILIZATION_THRESHOLD
    }

    /// Returns `true` when this insert satisfies the grow predicate.
    pub(crate) fn should_grow(&self, count: usize, capacity: usize, gap_window: usize) -> bool {
        if capacity >= self.policy.max {
            return false;
        }
        let utilization = count as f32 / capacity as f32;
        utilization >= GROW_UTILIZATION_THRESHOLD && gap_window > capacity / 2
    }

    /// Resets the rolling window after a capacity change so the next decision
    /// reflects the new geometry.
    pub(crate) fn reset_window(&mut self) {
        self.samples.clear();
        self.sample_cursor = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_grow_respects_max() {
        let p = AdaptiveCapacityPolicy::new(4, 16, 2.0);
        assert_eq!(p.next_grow(4), 8);
        assert_eq!(p.next_grow(8), 16);
        assert_eq!(p.next_grow(16), 16);
    }

    #[test]
    fn next_grow_makes_progress_with_small_factor() {
        let p = AdaptiveCapacityPolicy::new(4, 64, 1.5);
        // 4 * 1.5 = 6
        assert_eq!(p.next_grow(4), 6);
        // current+1 floor prevents stalling on tiny ring buffers.
        let q = AdaptiveCapacityPolicy::new(2, 64, 1.1);
        assert_eq!(q.next_grow(2), 3);
    }

    #[test]
    fn next_shrink_respects_min_and_floor() {
        let p = AdaptiveCapacityPolicy::new(4, 64, 2.0);
        assert_eq!(p.next_shrink(64, 0), 32);
        assert_eq!(p.next_shrink(8, 0), 4);
        assert_eq!(p.next_shrink(4, 0), 4);
        // Floor keeps buffered items addressable.
        assert_eq!(p.next_shrink(32, 20), 20);
    }

    #[test]
    fn should_grow_requires_high_utilization_and_wide_gap() {
        let state = AdaptiveState::new(AdaptiveCapacityPolicy::new(4, 32, 2.0));
        // 8/10 utilization with gap window 6 (>5) -> grow.
        assert!(state.should_grow(8, 10, 6));
        // 8/10 utilization but gap window 5 (not > 5) -> no grow.
        assert!(!state.should_grow(8, 10, 5));
        // Low utilization -> no grow.
        assert!(!state.should_grow(2, 10, 8));
    }

    #[test]
    fn should_grow_caps_at_max() {
        let state = AdaptiveState::new(AdaptiveCapacityPolicy::new(4, 8, 2.0));
        assert!(!state.should_grow(8, 8, 8));
    }

    #[test]
    fn should_shrink_needs_full_window_and_low_mean() {
        let mut state = AdaptiveState::new(AdaptiveCapacityPolicy::with_window(4, 32, 2.0, 4));
        // Window not yet full.
        for _ in 0..3 {
            state.record_sample(0.0);
        }
        assert!(!state.should_shrink());
        // Window full and below threshold.
        state.record_sample(0.0);
        assert!(state.should_shrink());
        // Push utilization back up via wraparound.
        for _ in 0..4 {
            state.record_sample(0.9);
        }
        assert!(!state.should_shrink());
    }

    #[test]
    #[should_panic(expected = "min capacity must be non-zero")]
    fn min_zero_panics() {
        let _ = AdaptiveCapacityPolicy::new(0, 4, 2.0);
    }

    #[test]
    #[should_panic(expected = "max must be >= min")]
    fn max_less_than_min_panics() {
        let _ = AdaptiveCapacityPolicy::new(8, 4, 2.0);
    }

    #[test]
    #[should_panic(expected = "growth_factor must be a finite value > 1.0")]
    fn growth_factor_one_panics() {
        let _ = AdaptiveCapacityPolicy::new(2, 8, 1.0);
    }

    #[test]
    #[should_panic(expected = "sample_window must be non-zero")]
    fn zero_window_panics() {
        let _ = AdaptiveCapacityPolicy::with_window(2, 8, 2.0, 0);
    }
}
