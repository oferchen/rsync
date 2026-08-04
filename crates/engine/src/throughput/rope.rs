//! The rope: admission sizing that paces the pipeline to the drum.
//!
//! In a drum-buffer-rope pipeline the *rope* ties upstream admission to the
//! drum's pace so the producer can never run further ahead than the buffer in
//! front of the constraint can absorb. Here the rope is realized as the ceiling
//! of the [`AdaptiveSemaphore`] that gates the concurrent-delta
//! [`work_queue`](crate::concurrent_delta::work_queue): the semaphore already
//! releases one permit per drained item (see `PermitGuard`), so the only thing
//! left to close the loop is *how high* to set the ceiling. [`Rope`] computes
//! that ceiling from the drum's measured service time and resizes the semaphore.
//!
//! # Sizing law
//!
//! The buffer in front of the drum must hold enough in-flight work to keep the
//! drum fed across one control window even if the producer stalls. Over a
//! protection window `W` the drum consumes `rate * W` bytes; at a per-item
//! footprint of `unit` bytes that is `rate * W / unit` items. Multiplying by a
//! safety factor gives the target admission ceiling:
//!
//! ```text
//! target = ceil(safety * rate_bytes_per_sec * W_secs / unit_bytes)
//! ```
//!
//! The result is then **memory-bounded** - a permit for a 1 GiB file reserves
//! far more resident memory than one for a 4 KiB file, so the ceiling is capped
//! at `mem_budget / weight(unit)` where `weight` mirrors the file-size buffer
//! tiers - and finally clamped to the configured `[min, max]`. Every returned
//! ceiling is therefore in `[min, max]` by construction.
//!
//! This is the DBR counterpart to the block-rate
//! [`AdaptiveQueueController`](crate::concurrent_delta::work_queue::AdaptiveQueueController):
//! that one reacts to *admission contention*, this one sizes from *drum service
//! time*. Both only move the ceiling within `[min, max]` and never reorder or
//! revoke in-flight work, so both preserve the single-producer / multi-consumer
//! ordering contract.

use std::sync::Arc;
use std::time::Duration;

use crate::concurrent_delta::work_queue::{AdaptiveSemaphore, MAX_CAPACITY};

use super::governor::{GovernorHandle, POLL_INTERVAL};
use super::sample::Constraint;

/// Default multiple of the drum's per-window demand to keep buffered.
///
/// A factor of two keeps roughly two control windows of work in flight ahead of
/// the drum, so a single stalled producer window cannot starve it before the
/// next resize. Larger factors buffer more slack (and memory) for burstier
/// producers; two is the conservative default that matches the historical
/// `2 * thread_count` static bound's intent of "one spare item per worker".
pub const DEFAULT_SAFETY_FACTOR: f64 = 2.0;

/// Default resident-memory budget for in-flight admission, in bytes.
///
/// 256 MiB caps the worst-case footprint of everything the rope admits at once.
/// It bounds the ceiling for large-file transfers (where each permit reserves a
/// megabyte-class buffer) while leaving small-file transfers - whose per-permit
/// weight is a few kilobytes - free to reach the configured `max`. Operators who
/// need a tighter or looser bound override it via [`RopeConfig::mem_budget`].
pub const DEFAULT_MEM_BUDGET_BYTES: u64 = 256 * 1024 * 1024;

// Per-permit memory weight tiers.
//
// Mirrors `transfer::adaptive_buffer::adaptive_buffer_size`: the engine crate
// does not depend on `transfer`, so the tier boundaries are duplicated here with
// this reference so the two stay in lockstep. A permit's memory weight is the
// buffer the transfer would allocate for a file of that size.

/// Files smaller than this (64 KiB) reserve the small buffer.
const SMALL_FILE_THRESHOLD: u64 = 64 * 1024;
/// Files smaller than this (1 MiB) reserve the medium buffer.
const MEDIUM_FILE_THRESHOLD: u64 = 1024 * 1024;
/// Files smaller than this (10 MiB) reserve the large buffer.
const HUGE_FILE_THRESHOLD: u64 = 10 * 1024 * 1024;

/// Weight of a permit for a small file (4 KiB buffer).
const SMALL_WEIGHT: u64 = 4 * 1024;
/// Weight of a permit for a medium file (64 KiB buffer).
const MEDIUM_WEIGHT: u64 = 64 * 1024;
/// Weight of a permit for a large file (256 KiB buffer).
const LARGE_WEIGHT: u64 = 256 * 1024;
/// Weight of a permit for a huge file (1 MiB buffer).
const HUGE_WEIGHT: u64 = 1024 * 1024;

/// Returns the resident-memory weight of one admission permit for a work unit of
/// `unit_bytes`, mirroring the transfer crate's file-size buffer tiers.
///
/// Never returns zero, so it is always a safe divisor when computing the
/// memory-bounded ceiling.
#[must_use]
pub const fn permit_weight_bytes(unit_bytes: u64) -> u64 {
    if unit_bytes < SMALL_FILE_THRESHOLD {
        SMALL_WEIGHT
    } else if unit_bytes < MEDIUM_FILE_THRESHOLD {
        MEDIUM_WEIGHT
    } else if unit_bytes < HUGE_FILE_THRESHOLD {
        LARGE_WEIGHT
    } else {
        HUGE_WEIGHT
    }
}

/// Error returned when a [`RopeConfig`] is internally inconsistent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RopeConfigError {
    /// `min` was zero; at least one permit is required for liveness.
    MinTooLow,
    /// `min` exceeded `max`.
    MinAboveMax {
        /// The requested minimum ceiling.
        min: usize,
        /// The requested maximum ceiling.
        max: usize,
    },
    /// `max` exceeded the semaphore's hard [`MAX_CAPACITY`].
    MaxTooHigh {
        /// The requested maximum ceiling.
        max: usize,
    },
    /// `safety_factor` was not a finite, strictly-positive number.
    BadSafetyFactor,
    /// `mem_budget` was zero, which would forbid every admission.
    ZeroMemBudget,
}

impl std::fmt::Display for RopeConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RopeConfigError::MinTooLow => f.write_str("rope min ceiling must be at least 1"),
            RopeConfigError::MinAboveMax { min, max } => {
                write!(f, "rope min ceiling {min} exceeds max {max}")
            }
            RopeConfigError::MaxTooHigh { max } => {
                write!(
                    f,
                    "rope max ceiling {max} exceeds the semaphore limit {MAX_CAPACITY}"
                )
            }
            RopeConfigError::BadSafetyFactor => {
                f.write_str("rope safety factor must be finite and positive")
            }
            RopeConfigError::ZeroMemBudget => f.write_str("rope memory budget must be non-zero"),
        }
    }
}

impl std::error::Error for RopeConfigError {}

/// Immutable sizing policy for a [`Rope`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RopeConfig {
    /// Lowest admission ceiling the rope may set (liveness floor, `>= 1`).
    min: usize,
    /// Highest admission ceiling the rope may set.
    max: usize,
    /// Multiple of per-window drum demand to keep buffered.
    safety_factor: f64,
    /// Control window the ceiling must keep the drum fed across.
    protection_window: Duration,
    /// Resident-memory cap on total in-flight admission, in bytes.
    mem_budget: u64,
}

impl RopeConfig {
    /// Builds a config for the `[min, max]` ceiling range with default safety
    /// factor, protection window ([`POLL_INTERVAL`]), and memory budget.
    ///
    /// # Errors
    ///
    /// Returns [`RopeConfigError`] when the range is inconsistent (`min == 0`,
    /// `min > max`, or `max > MAX_CAPACITY`).
    pub fn new(min: usize, max: usize) -> Result<Self, RopeConfigError> {
        Self::validated(
            min,
            max,
            DEFAULT_SAFETY_FACTOR,
            POLL_INTERVAL,
            DEFAULT_MEM_BUDGET_BYTES,
        )
    }

    fn validated(
        min: usize,
        max: usize,
        safety_factor: f64,
        protection_window: Duration,
        mem_budget: u64,
    ) -> Result<Self, RopeConfigError> {
        if min < 1 {
            return Err(RopeConfigError::MinTooLow);
        }
        if min > max {
            return Err(RopeConfigError::MinAboveMax { min, max });
        }
        if max > MAX_CAPACITY {
            return Err(RopeConfigError::MaxTooHigh { max });
        }
        if !safety_factor.is_finite() || safety_factor <= 0.0 {
            return Err(RopeConfigError::BadSafetyFactor);
        }
        if mem_budget == 0 {
            return Err(RopeConfigError::ZeroMemBudget);
        }
        Ok(Self {
            min,
            max,
            safety_factor,
            protection_window,
            mem_budget,
        })
    }

    /// Overrides the safety factor.
    ///
    /// # Errors
    ///
    /// Returns [`RopeConfigError::BadSafetyFactor`] if `factor` is not finite
    /// and positive.
    pub fn with_safety_factor(mut self, factor: f64) -> Result<Self, RopeConfigError> {
        if !factor.is_finite() || factor <= 0.0 {
            return Err(RopeConfigError::BadSafetyFactor);
        }
        self.safety_factor = factor;
        Ok(self)
    }

    /// Overrides the protection window.
    #[must_use]
    pub fn with_protection_window(mut self, window: Duration) -> Self {
        self.protection_window = window;
        self
    }

    /// Overrides the resident-memory budget.
    ///
    /// # Errors
    ///
    /// Returns [`RopeConfigError::ZeroMemBudget`] if `bytes` is zero.
    pub fn with_mem_budget(mut self, bytes: u64) -> Result<Self, RopeConfigError> {
        if bytes == 0 {
            return Err(RopeConfigError::ZeroMemBudget);
        }
        self.mem_budget = bytes;
        Ok(self)
    }

    /// The configured minimum ceiling.
    #[must_use]
    pub fn min(&self) -> usize {
        self.min
    }

    /// The configured maximum ceiling.
    #[must_use]
    pub fn max(&self) -> usize {
        self.max
    }

    /// The memory-bounded maximum ceiling for a work unit of `unit_bytes`.
    ///
    /// This is `min(max, mem_budget / weight(unit))`, never dropping below
    /// [`min`](Self::min) so the pipeline always stays live even under a tiny
    /// budget.
    #[must_use]
    fn mem_bounded_max(&self, unit_bytes: u64) -> usize {
        let weight = permit_weight_bytes(unit_bytes);
        // weight is always >= SMALL_WEIGHT > 0, so this division is safe.
        let by_mem = (self.mem_budget / weight).max(1);
        let by_mem = usize::try_from(by_mem).unwrap_or(usize::MAX);
        by_mem.min(self.max).max(self.min)
    }

    /// Computes the target ceiling for a drum with the given rate and work-unit
    /// footprint. The result is always in `[min, max]`.
    #[must_use]
    fn target_ceiling(&self, drum_rate_bps: f64, unit_bytes: u64) -> usize {
        let effective_max = self.mem_bounded_max(unit_bytes);
        // Without a usable rate estimate, stay at the conservative floor.
        if !drum_rate_bps.is_finite() || drum_rate_bps <= 0.0 {
            return self.min;
        }
        // A zero-size work unit still costs one slot; treat it as one byte so the
        // demand computation cannot divide by zero.
        let unit = unit_bytes.max(1) as f64;
        let window = self.protection_window.as_secs_f64();
        let demand_items = self.safety_factor * drum_rate_bps * window / unit;
        // Round up so a fractional item still reserves a whole permit; guard the
        // f64 -> usize cast against overflow and NaN.
        let target = demand_items.ceil();
        let target = if target.is_finite() && target >= 1.0 {
            target.min(usize::MAX as f64) as usize
        } else {
            self.min
        };
        target.clamp(self.min, effective_max)
    }
}

/// The rope actuator: sizes an [`AdaptiveSemaphore`]'s ceiling to the drum's
/// pace.
///
/// Holds a shared handle to the queue's admission semaphore. Call
/// [`actuate`](Self::actuate) (or [`actuate_from`](Self::actuate_from)) once per
/// governor evaluation window to move the ceiling; the semaphore's own
/// acquire/release protocol (permits returned at drain time) does the rest.
#[derive(Debug, Clone)]
pub struct Rope {
    semaphore: Arc<AdaptiveSemaphore>,
    config: RopeConfig,
}

impl Rope {
    /// Attaches a rope to `semaphore` with sizing policy `config`.
    ///
    /// The semaphore's current ceiling is left untouched until the first
    /// [`actuate`](Self::actuate); callers typically construct the semaphore via
    /// [`bounded_dynamic`](crate::concurrent_delta::work_queue::bounded_dynamic)
    /// with the same `[min, max]`.
    #[must_use]
    pub fn new(semaphore: Arc<AdaptiveSemaphore>, config: RopeConfig) -> Self {
        Self { semaphore, config }
    }

    /// The sizing policy in force.
    #[must_use]
    pub fn config(&self) -> &RopeConfig {
        &self.config
    }

    /// The target ceiling for a drum of `drum_rate_bps` and `unit_bytes` per
    /// item, without applying it. Always in `[min, max]`.
    #[must_use]
    pub fn target_ceiling(&self, drum_rate_bps: f64, unit_bytes: u64) -> usize {
        self.config.target_ceiling(drum_rate_bps, unit_bytes)
    }

    /// Resizes the semaphore to the target ceiling for the given drum signal and
    /// returns the applied ceiling.
    ///
    /// The target is clamped into `[min, max]`, which the semaphore validated at
    /// construction, so the resize is infallible; on the impossible out-of-range
    /// error the ceiling is left unchanged and the current value returned.
    pub fn actuate(&self, drum_rate_bps: f64, unit_bytes: u64) -> usize {
        let target = self.target_ceiling(drum_rate_bps, unit_bytes);
        match self.semaphore.resize(target) {
            Ok(()) => target,
            Err(_) => self.semaphore.current_cap(),
        }
    }

    /// Drives the ceiling from a live [`GovernorHandle`]: reads the current drum
    /// and its throughput and resizes accordingly.
    ///
    /// Holds the current ceiling unchanged (returning it) when the governor has
    /// not yet identified a drum ([`Constraint::Unknown`]) or has no throughput
    /// estimate for it - there is nothing to size against yet.
    pub fn actuate_from(&self, governor: &GovernorHandle, unit_bytes: u64) -> usize {
        let drum = governor.drum();
        if drum == Constraint::Unknown {
            return self.semaphore.current_cap();
        }
        match governor.throughput(drum) {
            Some(rate) => self.actuate(rate, unit_bytes),
            None => self.semaphore.current_cap(),
        }
    }

    /// The semaphore's current ceiling.
    #[must_use]
    pub fn current_ceiling(&self) -> usize {
        self.semaphore.current_cap()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::concurrent_delta::work_queue::bounded_dynamic;

    fn cfg(min: usize, max: usize) -> RopeConfig {
        RopeConfig::new(min, max).expect("valid range")
    }

    fn rope(min: usize, max: usize) -> Rope {
        let dq = bounded_dynamic(min, min, max).expect("dynamic queue");
        Rope::new(dq.semaphore, cfg(min, max))
    }

    // --- config validation ---------------------------------------------------

    #[test]
    fn config_rejects_inconsistent_ranges() {
        assert_eq!(RopeConfig::new(0, 4), Err(RopeConfigError::MinTooLow));
        assert_eq!(
            RopeConfig::new(8, 4),
            Err(RopeConfigError::MinAboveMax { min: 8, max: 4 })
        );
        assert_eq!(
            RopeConfig::new(1, MAX_CAPACITY + 1),
            Err(RopeConfigError::MaxTooHigh {
                max: MAX_CAPACITY + 1
            })
        );
    }

    #[test]
    fn config_rejects_bad_safety_and_budget() {
        assert_eq!(
            cfg(1, 4).with_safety_factor(0.0),
            Err(RopeConfigError::BadSafetyFactor)
        );
        assert_eq!(
            cfg(1, 4).with_safety_factor(f64::NAN),
            Err(RopeConfigError::BadSafetyFactor)
        );
        assert_eq!(
            cfg(1, 4).with_mem_budget(0),
            Err(RopeConfigError::ZeroMemBudget)
        );
    }

    // --- permit weight -------------------------------------------------------

    #[test]
    fn permit_weight_follows_file_size_tiers() {
        assert_eq!(permit_weight_bytes(0), SMALL_WEIGHT);
        assert_eq!(permit_weight_bytes(SMALL_FILE_THRESHOLD - 1), SMALL_WEIGHT);
        assert_eq!(permit_weight_bytes(SMALL_FILE_THRESHOLD), MEDIUM_WEIGHT);
        assert_eq!(permit_weight_bytes(MEDIUM_FILE_THRESHOLD), LARGE_WEIGHT);
        assert_eq!(permit_weight_bytes(HUGE_FILE_THRESHOLD), HUGE_WEIGHT);
        assert_eq!(permit_weight_bytes(u64::MAX), HUGE_WEIGHT);
    }

    // --- ceiling sizing ------------------------------------------------------

    #[test]
    fn no_rate_estimate_sizes_to_min() {
        let r = rope(2, 64);
        assert_eq!(r.target_ceiling(0.0, 4096), 2);
        assert_eq!(r.target_ceiling(-1.0, 4096), 2);
        assert_eq!(r.target_ceiling(f64::NAN, 4096), 2);
        assert_eq!(r.target_ceiling(f64::INFINITY, 4096), 2);
    }

    #[test]
    fn faster_drum_demands_a_higher_ceiling() {
        // 4 KiB units, 0.25 s window, safety 2. At 1 MB/s: 2*1e6*0.25/4096 ~= 122
        // -> clamped to max. At 40 KB/s: 2*4e4*0.25/4096 ~= 4.88 -> ceil 5.
        let r = rope(2, 256);
        let slow = r.target_ceiling(40_000.0, 4096);
        let fast = r.target_ceiling(1_000_000.0, 4096);
        assert_eq!(slow, 5, "slow drum needs a shallow buffer, got {slow}");
        assert!(
            fast > slow,
            "faster drum must demand more, {fast} vs {slow}"
        );
    }

    #[test]
    fn ceiling_is_always_within_min_max() {
        let r = rope(3, 40);
        for &rate in &[1.0, 1e3, 1e6, 1e9, 1e15, f64::MAX] {
            for &unit in &[0u64, 1, 4096, 1 << 20, 1 << 30, u64::MAX] {
                let c = r.target_ceiling(rate, unit);
                assert!(
                    (3..=40).contains(&c),
                    "ceiling {c} out of [3,40] for rate={rate} unit={unit}"
                );
            }
        }
    }

    #[test]
    fn memory_budget_caps_large_file_ceiling() {
        // A 4 MiB memory budget with 1 MiB-per-permit huge files allows only 4
        // permits even though max is 1000 and the rate demands far more.
        let dq = bounded_dynamic(1, 1, 1000).unwrap();
        let config = cfg(1, 1000).with_mem_budget(4 * 1024 * 1024).unwrap();
        let r = Rope::new(dq.semaphore, config);
        let huge = r.target_ceiling(1e12, HUGE_FILE_THRESHOLD);
        assert_eq!(
            huge, 4,
            "memory budget must cap huge-file admission, got {huge}"
        );
        // Small files under the same budget are not memory-constrained.
        let small = r.target_ceiling(1e12, 1024);
        assert_eq!(small, 1000, "small files can reach max, got {small}");
    }

    #[test]
    fn zero_size_unit_does_not_divide_by_zero() {
        let r = rope(1, 64);
        let c = r.target_ceiling(1e6, 0);
        assert!((1..=64).contains(&c), "zero-size unit gave {c}");
    }

    // --- actuation -----------------------------------------------------------

    #[test]
    fn actuate_moves_the_semaphore_ceiling() {
        let dq = bounded_dynamic(2, 2, 256).unwrap();
        let sem = Arc::clone(&dq.semaphore);
        let r = Rope::new(dq.semaphore, cfg(2, 256));
        let applied = r.actuate(1_000_000.0, 4096);
        assert_eq!(applied, sem.current_cap());
        assert!(applied > 2, "a fast drum should grow the ceiling");
        // A stalled estimate pulls the ceiling back to the floor.
        let floored = r.actuate(0.0, 4096);
        assert_eq!(floored, 2);
        assert_eq!(sem.current_cap(), 2);
    }

    #[test]
    fn actuate_from_holds_without_a_drum() {
        use crate::throughput::{Governor, GovernorConfig, GovernorMode};
        let dq = bounded_dynamic(4, 2, 64).unwrap();
        let sem = Arc::clone(&dq.semaphore);
        let r = Rope::new(dq.semaphore, cfg(2, 64));
        let mut gov = Governor::spawn(GovernorConfig::new(GovernorMode::Observe));
        // No telemetry emitted: drum stays Unknown, ceiling held.
        assert_eq!(r.actuate_from(&gov, 4096), 4);
        assert_eq!(sem.current_cap(), 4);
        gov.shutdown();
    }
}
