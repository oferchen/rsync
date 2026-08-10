//! AIMD grow/shrink controller for the dynamic work-queue depth.
//!
//! The static `2 * thread_count` queue depth cannot adapt to a slow
//! destination: on high-latency filesystems (NFS, FUSE) or a saturated disk
//! the bounded queue fills, the single producer blocks on admission, and work
//! items head-of-line-block behind the slowest consumer. Conversely, a fixed
//! shallow depth starves workers when the destination is fast and the producer
//! cannot keep the queue full.
//!
//! [`AdaptiveQueueController`] closes that loop. It observes the
//! [`AdaptiveSemaphore`] admission block rate and drives an
//! [`AimdLimiter`](super::limiter::AimdLimiter) whose `target` is the source of
//! truth for the desired depth, then mirrors that target onto the semaphore via
//! [`AdaptiveSemaphore::resize`]:
//!
//! - **Grow (additive-increase / slow-start).** A low block rate means the
//!   producer is not stalling on admission - workers drain fast and may be
//!   starved for depth. The controller feeds the limiter a success signal, so
//!   the target climbs (doubling in slow-start, then `+alpha` per window),
//!   widening the queue up to `max`.
//! - **Shrink (multiplicative-decrease).** A high block rate means the producer
//!   is repeatedly blocking on admission - the destination is saturated and a
//!   deeper queue only buffers more work behind the bottleneck. The controller
//!   feeds the limiter an overload signal, halving the target down to `min`.
//!
//! # Default-on, with a deterministic escape hatch
//!
//! The adaptive path is the default: the parallel receive-delta pipeline builds
//! a [`bounded_dynamic`](super::bounded_dynamic) queue governed by this
//! controller, initialised at the same `2 * thread_count` depth the old static
//! queue used, and adapts from there. Adaptation is internal autotuning, not a
//! user-facing knob. The single environment escape hatch
//! `OC_RSYNC_ADAPTIVE_QUEUE` exists only for reproducibility and debugging:
//! setting it to a disabling value (`0`/`false`/`off`/`no`) pins the
//! deterministic static [`bounded`](super::bounded) queue with no controller.
//! [`adaptive_queue_enabled`] reports the resulting choice.
//!
//! # Invariants
//!
//! - The controller never reorders work or touches queue contents; it only
//!   moves the admission ceiling within `[min, max]`. The single-producer /
//!   multiple-consumer ordering contract is untouched.
//! - Shrinking never revokes an in-flight permit (see
//!   [`AdaptiveSemaphore::resize`]); already-admitted work always completes.
//! - The ceiling is always clamped to the `[min, max]` configured on the queue,
//!   so a controller arithmetic mistake cannot push depth out of range.

use std::sync::Arc;

use super::adaptive_semaphore::{AdaptiveSemaphore, SemStats};
use super::bounded::DynamicWorkQueue;
use super::limiter::{AimdLimiter, LimiterConfig, OverloadReason};

/// Environment escape hatch pinning the deterministic static work-queue depth.
///
/// The adaptive controller is the default. Setting this to a disabling value
/// (`0`/`false`/`off`/`no`, case-insensitive) forces the fixed static depth for
/// reproducible, controller-free behaviour; any other value (or leaving it
/// unset) keeps adaptation on.
pub const ADAPTIVE_QUEUE_ENV: &str = "OC_RSYNC_ADAPTIVE_QUEUE";

/// Block-rate threshold at or above which the controller treats the window as
/// backpressure (slow destination) and shrinks the depth.
///
/// A block rate at or above this fraction means the producer blocked on
/// admission for at least this share of its sends over the window - a clear
/// saturation signal.
const SHRINK_BLOCK_RATE: f64 = 0.5;

/// Block-rate threshold at or below which the controller treats the window as
/// worker starvation (fast destination, shallow queue) and grows the depth.
///
/// A block rate at or below this fraction means admission almost never stalled,
/// so a deeper queue can keep more work in flight without buffering behind a
/// bottleneck.
const GROW_BLOCK_RATE: f64 = 0.1;

/// Minimum number of admission attempts in a window before the controller acts.
///
/// Below this the block-rate sample is too noisy to trust, so the controller
/// holds the current depth rather than reacting to a handful of admissions.
const MIN_SAMPLES: u64 = 8;

/// Lightweight telemetry for the adaptive controller.
///
/// Captured behind the same opt-in gate as the controller itself; the default
/// static path never constructs one.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ControllerStats {
    /// Number of ticks that grew the admission ceiling.
    pub grows: u64,
    /// Number of ticks that shrank the admission ceiling.
    pub shrinks: u64,
    /// Number of ticks that left the ceiling unchanged (held or clamped).
    pub holds: u64,
    /// The admission ceiling after the most recent tick.
    pub current_depth: usize,
}

/// Outcome of a single controller [`tick`](AdaptiveQueueController::tick).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TickOutcome {
    /// The ceiling grew from `from` to `to`.
    Grew {
        /// Ceiling before the tick.
        from: usize,
        /// Ceiling after the tick.
        to: usize,
    },
    /// The ceiling shrank from `from` to `to`.
    Shrank {
        /// Ceiling before the tick.
        from: usize,
        /// Ceiling after the tick.
        to: usize,
    },
    /// The ceiling was left unchanged (insufficient samples, block rate in the
    /// neutral band, or the AIMD target already clamped at the boundary).
    Held {
        /// The unchanged ceiling.
        depth: usize,
    },
}

/// AIMD grow/shrink controller for a [`DynamicWorkQueue`]'s admission depth.
///
/// See the [module documentation](self) for the control law and opt-in policy.
/// The controller owns a shared handle to the queue's [`AdaptiveSemaphore`] and
/// an internal [`AimdLimiter`] whose `target` tracks the desired depth. Call
/// [`tick`](Self::tick) periodically (for example once per drained batch) to
/// fold the latest block-rate signal into the depth.
#[derive(Debug)]
pub struct AdaptiveQueueController {
    semaphore: Arc<AdaptiveSemaphore>,
    limiter: AimdLimiter,
    baseline: SemStats,
    min: usize,
    max: usize,
    stats: ControllerStats,
}

impl AdaptiveQueueController {
    /// Builds a controller for `queue`.
    ///
    /// The initial AIMD target and `[min, max]` clamp are taken from the
    /// queue's current ceiling and configured bounds, so the controller starts
    /// at today's static baseline depth and can only move within the range the
    /// queue already allows.
    #[must_use]
    pub fn new(queue: &DynamicWorkQueue) -> Self {
        let semaphore = Arc::clone(&queue.semaphore);
        let initial = semaphore.current_cap();
        let (min, max) = queue
            .sender
            .capacity_bounds()
            .expect("a DynamicWorkQueue always exposes capacity bounds");
        let limiter = LimiterConfig::new(initial)
            .min_limit(min)
            .max_limit(max)
            .build();
        let baseline = semaphore.stats();
        Self {
            semaphore,
            limiter,
            baseline,
            min,
            max,
            stats: ControllerStats {
                current_depth: initial,
                ..ControllerStats::default()
            },
        }
    }

    /// Folds the block-rate observed since the previous tick into the depth.
    ///
    /// Reads the block rate over the window since the last tick and drives the
    /// AIMD limiter: a high rate (backpressure / slow destination) records an
    /// overload so the target halves; a low rate (worker starvation) records a
    /// success so the target grows. The new target is clamped to `[min, max]`
    /// and applied to the semaphore. A resize that would move outside the range
    /// is never issued, so this call cannot fail.
    ///
    /// Returns the [`TickOutcome`] describing the depth change, if any.
    pub fn tick(&mut self) -> TickOutcome {
        let rate = self.semaphore.block_rate_since(self.baseline);
        let cur = self.semaphore.stats();
        let samples = cur.acquires.saturating_sub(self.baseline.acquires);
        self.baseline = cur;

        let before = self.semaphore.current_cap();

        if samples < MIN_SAMPLES {
            // Too few admissions to trust the rate; hold and re-baseline.
            return self.hold(before);
        }

        // Feed the AIMD limiter one control step per tick, so each observation
        // window advances the target by exactly one AIMD move.
        //
        // A ticket is acquired and immediately released, so `in_flight` returns
        // to zero and the next acquire always succeeds; only the target moves.
        if rate >= SHRINK_BLOCK_RATE {
            // Backpressure: one overload signal halves the target (subject to
            // the limiter's own debounce window).
            if let Some(ticket) = self.limiter.try_acquire() {
                ticket.record_overload(OverloadReason::QueueSaturated);
            }
        } else if rate <= GROW_BLOCK_RATE {
            // Starvation: complete one full success window so the target
            // advances a single AIMD step this tick (doubling in slow-start,
            // then `+alpha` in steady state) rather than needing `target` ticks
            // to move once.
            let window = self.limiter.target().max(1);
            for _ in 0..window {
                match self.limiter.try_acquire() {
                    Some(ticket) => ticket.record_success(),
                    None => break,
                }
            }
        } else {
            // Neutral band: neither starved nor saturated. Hold steady.
            return self.hold(before);
        }

        let target = self.limiter.target().clamp(self.min, self.max);
        if target == before {
            return self.hold(before);
        }

        // resize only fails outside [MIN_CAPACITY, MAX_CAPACITY]; target is
        // already clamped to [min, max] which the queue validated on creation,
        // so this is infallible in practice. Fall back to holding on the
        // impossible error rather than panicking on the hot path.
        if self.semaphore.resize(target).is_err() {
            return self.hold(before);
        }

        self.stats.current_depth = target;
        if target > before {
            self.stats.grows += 1;
            TickOutcome::Grew {
                from: before,
                to: target,
            }
        } else {
            self.stats.shrinks += 1;
            TickOutcome::Shrank {
                from: before,
                to: target,
            }
        }
    }

    /// Records a hold outcome and returns it without moving the ceiling.
    fn hold(&mut self, depth: usize) -> TickOutcome {
        self.stats.holds += 1;
        self.stats.current_depth = depth;
        TickOutcome::Held { depth }
    }

    /// Returns the controller's telemetry snapshot.
    #[must_use]
    pub fn stats(&self) -> ControllerStats {
        self.stats
    }

    /// Returns the current admission ceiling the controller manages.
    #[must_use]
    pub fn current_depth(&self) -> usize {
        self.semaphore.current_cap()
    }
}

/// Returns `true` when the adaptive work-queue controller should run.
///
/// Adaptive is the default, so this returns `true` unless
/// [`ADAPTIVE_QUEUE_ENV`] is set to a disabling value (`0`, `false`, `off`,
/// `no`, or empty, case-insensitive), which pins the deterministic static
/// depth. Every other value - and the unset default - keeps adaptation on.
#[must_use]
pub fn adaptive_queue_enabled() -> bool {
    match std::env::var(ADAPTIVE_QUEUE_ENV) {
        Ok(v) => {
            let v = v.trim();
            !v.is_empty()
                && !matches!(
                    v.to_ascii_lowercase().as_str(),
                    "0" | "false" | "off" | "no"
                )
        }
        // Unset: adaptation is the default.
        Err(_) => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::concurrent_delta::work_queue::bounded_dynamic;
    use std::sync::{Mutex, MutexGuard, OnceLock};
    use std::thread;
    use std::time::{Duration, Instant};

    /// Serialises the env-var mutation across this module's gate tests, since
    /// `std::env::set_var`/`remove_var` mutate process-global state.
    fn env_lock() -> MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    /// Restores `OC_RSYNC_ADAPTIVE_QUEUE` to its prior value on drop.
    struct EnvGuard(Option<String>);
    impl EnvGuard {
        fn set(val: &str) -> Self {
            let prev = std::env::var(ADAPTIVE_QUEUE_ENV).ok();
            unsafe { std::env::set_var(ADAPTIVE_QUEUE_ENV, val) };
            Self(prev)
        }
        fn unset() -> Self {
            let prev = std::env::var(ADAPTIVE_QUEUE_ENV).ok();
            unsafe { std::env::remove_var(ADAPTIVE_QUEUE_ENV) };
            Self(prev)
        }
    }
    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.0 {
                Some(v) => unsafe { std::env::set_var(ADAPTIVE_QUEUE_ENV, v) },
                None => unsafe { std::env::remove_var(ADAPTIVE_QUEUE_ENV) },
            }
        }
    }

    /// Pumps `n` non-blocking acquire/release cycles on the semaphore so the
    /// controller observes a window with a zero block rate (worker starvation
    /// signal). `cap` must be at least one so no acquire ever blocks.
    fn pump_no_block(sem: &AdaptiveSemaphore, n: u64) {
        for _ in 0..n {
            sem.acquire();
            sem.release();
        }
    }

    /// Drives a window dominated by blocked acquires, yielding a block rate at
    /// or above the shrink threshold (backpressure signal). Fully saturates the
    /// semaphore's current capacity, spawns enough blocked acquirers to
    /// outnumber the non-blocking saturating acquires, waits until every one has
    /// registered as blocked, then releases the held permits one at a time so
    /// the counters end deterministic.
    fn pump_all_block(sem: &Arc<AdaptiveSemaphore>) {
        // Exhaust every available permit so the spawned acquires must block.
        let held = sem.current_cap();
        for _ in 0..held {
            sem.acquire();
        }
        // Block at least MIN_SAMPLES acquirers and at least as many as the
        // non-blocking saturating acquires, so blocks/acquires >= 1/2.
        let n = (held as u64).max(MIN_SAMPLES);
        let baseline_blocks = sem.block_count();
        let mut handles = Vec::new();
        for _ in 0..n {
            let s = Arc::clone(sem);
            handles.push(thread::spawn(move || {
                s.acquire();
                s.release();
            }));
        }
        // Wait until all n acquirers are parked in the blocked state.
        let deadline = Instant::now() + Duration::from_secs(5);
        while sem.block_count() < baseline_blocks + n {
            assert!(Instant::now() < deadline, "acquirers never blocked");
            thread::yield_now();
        }
        // Release the held permits; the blocked acquirers drain one by one.
        for _ in 0..held {
            sem.release();
        }
        for h in handles {
            h.join().unwrap();
        }
    }

    #[test]
    fn adaptive_enabled_by_default_when_unset() {
        let _lock = env_lock();
        let _guard = EnvGuard::unset();
        assert!(
            adaptive_queue_enabled(),
            "adaptation must be the default when the env var is unset"
        );
    }

    #[test]
    fn disabling_spellings_pin_static_depth() {
        let _lock = env_lock();
        for val in ["", "0", "false", "off", "no", "FALSE", "Off"] {
            let _guard = EnvGuard::set(val);
            assert!(
                !adaptive_queue_enabled(),
                "value {val:?} must pin the deterministic static depth"
            );
        }
    }

    #[test]
    fn other_values_keep_adaptation_on() {
        let _lock = env_lock();
        for val in ["1", "true", "yes", "on", "auto"] {
            let _guard = EnvGuard::set(val);
            assert!(
                adaptive_queue_enabled(),
                "value {val:?} must keep adaptation on"
            );
        }
    }

    #[test]
    fn new_starts_at_queue_initial_depth() {
        let dq = bounded_dynamic(4, 2, 16).unwrap();
        let ctrl = AdaptiveQueueController::new(&dq);
        assert_eq!(ctrl.current_depth(), 4);
        assert_eq!(ctrl.stats().current_depth, 4);
        assert_eq!(ctrl.stats().grows, 0);
        assert_eq!(ctrl.stats().shrinks, 0);
    }

    #[test]
    fn grows_under_starvation() {
        // Start mid-range with headroom to grow. A zero block rate is the
        // worker-starvation signal, so the depth must climb toward max.
        let dq = bounded_dynamic(2, 1, 32).unwrap();
        let mut ctrl = AdaptiveQueueController::new(&dq);
        let start = ctrl.current_depth();

        pump_no_block(&dq.semaphore, MIN_SAMPLES);
        let outcome = ctrl.tick();

        assert!(
            matches!(outcome, TickOutcome::Grew { .. }),
            "low block rate must grow depth, got {outcome:?}"
        );
        assert!(ctrl.current_depth() > start, "depth must increase");
        assert_eq!(ctrl.stats().grows, 1);
        assert_eq!(ctrl.current_depth(), dq.semaphore.current_cap());
    }

    #[test]
    fn shrinks_under_backpressure() {
        // A high block rate is the slow-destination signal, so the depth must
        // fall via multiplicative decrease.
        let dq = bounded_dynamic(8, 1, 32).unwrap();
        let mut ctrl = AdaptiveQueueController::new(&dq);
        let start = ctrl.current_depth();

        pump_all_block(&dq.semaphore);
        let outcome = ctrl.tick();

        assert!(
            matches!(outcome, TickOutcome::Shrank { .. }),
            "high block rate must shrink depth, got {outcome:?}"
        );
        assert!(ctrl.current_depth() < start, "depth must decrease");
        assert_eq!(ctrl.stats().shrinks, 1);
    }

    #[test]
    fn holds_below_min_samples() {
        let dq = bounded_dynamic(4, 2, 16).unwrap();
        let mut ctrl = AdaptiveQueueController::new(&dq);
        // Fewer than MIN_SAMPLES admissions: too noisy to act on.
        pump_no_block(&dq.semaphore, MIN_SAMPLES - 1);
        let outcome = ctrl.tick();
        assert!(matches!(outcome, TickOutcome::Held { depth: 4 }));
        assert_eq!(ctrl.current_depth(), 4, "depth unchanged with few samples");
        assert_eq!(ctrl.stats().holds, 1);
        assert_eq!(ctrl.stats().grows, 0);
    }

    #[test]
    fn clamps_to_max_on_repeated_growth() {
        let dq = bounded_dynamic(2, 1, 6).unwrap();
        let mut ctrl = AdaptiveQueueController::new(&dq);
        // Repeatedly signal starvation; depth must converge to max and stop.
        for _ in 0..20 {
            pump_no_block(&dq.semaphore, MIN_SAMPLES);
            ctrl.tick();
        }
        assert_eq!(ctrl.current_depth(), 6, "depth clamped to configured max");
        assert!(
            ctrl.current_depth() <= 6,
            "controller never exceeds the max bound"
        );
    }

    #[test]
    fn clamps_to_min_on_repeated_backpressure() {
        let dq = bounded_dynamic(16, 3, 32).unwrap();
        let mut ctrl = AdaptiveQueueController::new(&dq);
        // Repeatedly signal saturation; depth must converge to min and stop.
        for _ in 0..20 {
            pump_all_block(&dq.semaphore);
            ctrl.tick();
        }
        assert_eq!(ctrl.current_depth(), 3, "depth clamped to configured min");
        assert!(
            ctrl.current_depth() >= 3,
            "controller never drops below the min bound"
        );
    }
}
