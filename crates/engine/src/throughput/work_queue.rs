//! Governor-weighted admission for the concurrent-delta work queue (G3).
//!
//! The concurrent-delta [`work_queue`](crate::concurrent_delta::work_queue) has
//! historically bounded in-flight work at a static `2 * thread_count` items (see
//! [`bounded`](crate::concurrent_delta::work_queue::bounded)). That bound is a
//! blunt instrument: it counts *items*, so a queue of 4 GiB files reserves the
//! same admission slot as a queue of 4 KiB files, and it never moves with the
//! pipeline's actual constraint.
//!
//! This module replaces that static bound - **only when the governor is
//! engaged** - with the drum-buffer-rope [`Rope`]: the semaphore ceiling is
//! sized from the drum's measured service time, *weighted by expected buffer
//! footprint*, memory-bounded, and clamped to `[min, max]`. When the governor is
//! [`GovernorMode::Off`] the selector falls back to the byte-for-byte original:
//! a fixed-capacity `bounded_with_capacity` queue whose crossbeam channel is
//! the sole admission gate. The default is therefore unchanged; the weighted
//! ceiling is opt-in behind `OC_RSYNC_GOVERNOR=on`.
//!
//! # What "weighted" means here
//!
//! The rope's [`permit_weight_bytes`](super::permit_weight_bytes) charges each
//! admission permit the resident-buffer footprint the transfer would allocate
//! for a file of that size, so a memory budget admits proportionally fewer
//! large-file permits than small-file permits. Admission accounting stays exact
//! either way - one permit acquired per [`send`](WorkQueueSender::send), one
//! released per drained item (see `PermitGuard`) - so at quiescence permits
//! acquired equal permits released regardless of the ceiling.
//!
//! # Ordering is untouched
//!
//! The rope only resizes the admission ceiling; it never reorders, revokes, or
//! reclassifies in-flight work. The single-producer / multi-consumer contract
//! and the [`ReorderBuffer`](crate::concurrent_delta::ReorderBuffer) drain are
//! exactly as before, so NDX wire ordering is unaffected on both paths.

use std::time::Duration;

use crate::concurrent_delta::work_queue::{self, MAX_CAPACITY, WorkQueueReceiver, WorkQueueSender};

use super::governor::{GovernorHandle, GovernorMode};
use super::rope::{Rope, RopeConfig};

/// Liveness floor for the governed admission ceiling.
///
/// Matches the `.max(2)` floor of the static capacity heuristics
/// ([`adaptive_capacity`](crate::concurrent_delta::work_queue::adaptive_queue_depth)),
/// so the rope can pull the ceiling all the way down to a slow drum's pace
/// without ever collapsing admission below two in-flight items - one being
/// processed while one waits keeps the single consumer from stalling.
pub const GOVERNED_MIN_DEPTH: usize = 2;

/// Headroom multiplier applied to the static baseline to derive the governed
/// ceiling's upper bound.
///
/// Bounds worst-case in-flight admission - and the reorder window sized to match
/// it - to twice today's static `2 * thread_count` baseline. A drum fast enough
/// to demand more buffering is capped here so a burst can never grow resident
/// memory past a predictable multiple of the pre-governor bound; the rope's own
/// memory budget tightens this further for large-file transfers.
pub const GOVERNED_MAX_FACTOR: usize = 2;

/// The `[initial, min, max]` admission-ceiling triple derived from a static
/// baseline capacity.
///
/// `initial == baseline` starts the governed queue at exactly today's static
/// depth, so until the governor commits a drum the pipeline behaves as it does
/// now; the rope then moves the ceiling within `[min, max]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CeilingBounds {
    initial: usize,
    min: usize,
    max: usize,
}

impl CeilingBounds {
    /// Derives the governed ceiling bounds from a static `baseline` capacity.
    ///
    /// `min` is the liveness floor clamped not to exceed the baseline (so a tiny
    /// single-worker baseline stays consistent), `max` is `baseline *
    /// GOVERNED_MAX_FACTOR` clamped into `[baseline, MAX_CAPACITY]`, and
    /// `initial` is the baseline itself.
    fn from_baseline(baseline: usize) -> Self {
        let min = GOVERNED_MIN_DEPTH.min(baseline);
        let max = baseline
            .saturating_mul(GOVERNED_MAX_FACTOR)
            .clamp(baseline, MAX_CAPACITY);
        Self {
            initial: baseline,
            min,
            max,
        }
    }
}

/// A work queue whose admission ceiling is selected by governor mode.
///
/// Built by [`governed_work_queue`]. Bundles the producer/consumer halves with
/// an optional [`Rope`]:
///
/// - **Off / fallback** - `rope` is `None`; the queue is a plain
///   `bounded_with_capacity`
///   fixed bound, byte-identical to the pre-governor path.
/// - **Governed** - `rope` is `Some`; [`retune`](Self::retune) sizes the
///   admission semaphore to the governor's drum each control window.
///
/// The caller takes the [`sender`](Self::sender)/`receiver`
/// halves (via [`into_parts`](Self::into_parts)) and wires the receiver into a
/// consumer exactly as with any work queue; the reorder window should be sized
/// to [`reorder_window`](Self::reorder_window) so a grown ceiling never overruns
/// it.
pub struct GovernedWorkQueue {
    sender: WorkQueueSender,
    receiver: WorkQueueReceiver,
    rope: Option<Rope>,
    reorder_window: usize,
}

impl std::fmt::Debug for GovernedWorkQueue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GovernedWorkQueue")
            .field("governed", &self.rope.is_some())
            .field("current_capacity", &self.sender.current_capacity())
            .field("reorder_window", &self.reorder_window)
            .finish_non_exhaustive()
    }
}

impl GovernedWorkQueue {
    /// The producer half.
    #[must_use]
    pub fn sender(&self) -> &WorkQueueSender {
        &self.sender
    }

    /// Whether the queue is under governor control (`true`) or on the fixed
    /// fallback bound (`false`).
    #[must_use]
    pub fn is_governed(&self) -> bool {
        self.rope.is_some()
    }

    /// The attached rope, or `None` on the fixed path.
    #[must_use]
    pub fn rope(&self) -> Option<&Rope> {
        self.rope.as_ref()
    }

    /// The current admission ceiling: the semaphore's live ceiling on the
    /// governed path, or the fixed channel capacity otherwise.
    #[must_use]
    pub fn current_capacity(&self) -> usize {
        self.sender.current_capacity()
    }

    /// The `[min, max]` range the governed ceiling may move within, or `None` on
    /// the fixed path (whose capacity does not move).
    #[must_use]
    pub fn capacity_bounds(&self) -> Option<(usize, usize)> {
        self.sender.capacity_bounds()
    }

    /// The reorder-buffer window the consumer should be sized to.
    ///
    /// Equal to the maximum admission ceiling, so the reorder buffer can always
    /// hold every in-flight result even when the rope grows the ceiling to its
    /// upper bound.
    #[must_use]
    pub fn reorder_window(&self) -> usize {
        self.reorder_window
    }

    /// Retunes the admission ceiling from the governor's current drum and
    /// returns the applied ceiling.
    ///
    /// On the governed path this delegates to
    /// [`Rope::actuate_from`](super::rope::Rope::actuate_from): it reads the
    /// governor's committed drum and its throughput, sizes the ceiling for a
    /// work unit of `unit_bytes`, and resizes the semaphore. The ceiling is held
    /// unchanged while the drum is [`Constraint::Unknown`](super::sample::Constraint::Unknown)
    /// or has no throughput estimate yet. On the fixed path this is a no-op that
    /// returns the (immovable) fixed capacity.
    ///
    /// This is admission sizing only; it never touches in-flight work, so wire
    /// ordering is unaffected. Call it once per governor evaluation window (for
    /// example from the producer's periodic control tick).
    pub fn retune(&self, governor: &GovernorHandle, unit_bytes: u64) -> usize {
        match &self.rope {
            Some(rope) => rope.actuate_from(governor, unit_bytes),
            None => self.sender.current_capacity(),
        }
    }

    /// Splits the queue into its producer and consumer halves for wiring into a
    /// pipeline, discarding the rope handle.
    ///
    /// Use [`retune`](Self::retune) before calling this if you need to size the
    /// ceiling, or clone the [`rope`](Self::rope) out first to keep driving it
    /// after the split.
    #[must_use]
    pub fn into_parts(self) -> (WorkQueueSender, WorkQueueReceiver) {
        (self.sender, self.receiver)
    }

    /// Splits the queue into its halves and the optional rope, so the caller can
    /// keep driving the ceiling after wiring the halves into a pipeline.
    #[must_use]
    pub fn into_parts_with_rope(self) -> (WorkQueueSender, WorkQueueReceiver, Option<Rope>) {
        (self.sender, self.receiver, self.rope)
    }
}

/// Builds a work queue whose admission ceiling is chosen by governor `mode`.
///
/// `baseline` is the static `2 * thread_count`-style capacity the fixed path
/// uses verbatim. When `mode` is [`GovernorMode::Off`] the result is exactly a
/// `bounded_with_capacity`
/// queue - the crossbeam channel is the sole gate and no rope is attached - so
/// the default build is byte-identical to the pre-governor pipeline.
///
/// When `mode` is [`GovernorMode::Observe`] the result is a
/// [`bounded_dynamic`](crate::concurrent_delta::work_queue::bounded_dynamic)
/// queue starting at `baseline` and free to move within
/// `[GOVERNED_MIN_DEPTH, baseline * GOVERNED_MAX_FACTOR]`, with a [`Rope`] whose
/// [`retune`](GovernedWorkQueue::retune) sizes the ceiling from the drum. Sizing
/// weights each permit by expected buffer footprint and memory-bounds the total,
/// per [`RopeConfig`]'s defaults.
///
/// If the dynamic queue or rope config is somehow rejected (an internally
/// impossible bounds error), the build degrades cleanly to the fixed path rather
/// than failing, so a caller never has to handle an error here.
///
/// # Panics
///
/// Panics if `baseline` is zero, matching
/// `bounded_with_capacity`.
#[must_use]
pub fn governed_work_queue(baseline: usize, mode: GovernorMode) -> GovernedWorkQueue {
    assert!(
        baseline > 0,
        "work queue baseline capacity must be non-zero"
    );
    match mode {
        GovernorMode::Off => fixed(baseline),
        GovernorMode::Observe => try_governed(baseline).unwrap_or_else(|| fixed(baseline)),
    }
}

/// Builds the fixed-bound fallback: byte-identical to the pre-governor queue.
fn fixed(baseline: usize) -> GovernedWorkQueue {
    let (sender, receiver) = work_queue::bounded_with_capacity(baseline);
    GovernedWorkQueue {
        sender,
        receiver,
        rope: None,
        reorder_window: baseline,
    }
}

/// Attempts to build the governed dynamic queue with an attached rope.
///
/// Returns `None` on the practically impossible bounds/config error so the
/// caller can degrade to the fixed path.
fn try_governed(baseline: usize) -> Option<GovernedWorkQueue> {
    let CeilingBounds { initial, min, max } = CeilingBounds::from_baseline(baseline);
    let queue = work_queue::bounded_dynamic(initial, min, max).ok()?;
    let config = RopeConfig::new(min, max).ok()?;
    let rope = Rope::new(std::sync::Arc::clone(&queue.semaphore), config);
    let work_queue::DynamicWorkQueue {
        sender, receiver, ..
    } = queue;
    Some(GovernedWorkQueue {
        sender,
        receiver,
        rope: Some(rope),
        reorder_window: max,
    })
}

impl GovernedWorkQueue {
    /// Replaces the rope's sizing policy with one derived from the current
    /// `[min, max]` and the given protection window and memory budget.
    ///
    /// No-op on the fixed path. A zero `mem_budget` is rejected and leaves the
    /// policy unchanged.
    #[must_use]
    pub fn with_sizing(mut self, protection_window: Duration, mem_budget: u64) -> Self {
        if let (Some(rope), Some((min, max))) = (self.rope.as_ref(), self.sender.capacity_bounds())
        {
            if let Ok(config) = RopeConfig::new(min, max)
                .and_then(|c| c.with_mem_budget(mem_budget))
                .map(|c| c.with_protection_window(protection_window))
            {
                self.rope = Some(Rope::new(rope.semaphore_arc(), config));
            }
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::concurrent_delta::DeltaWork;
    use crate::throughput::{Governor, GovernorConfig};
    use std::path::PathBuf;

    // --- ceiling bounds derivation ------------------------------------------

    #[test]
    fn weighted_bounds_track_the_static_baseline() {
        let b = CeilingBounds::from_baseline(16);
        assert_eq!(b.initial, 16, "starts at today's static depth");
        assert_eq!(b.min, GOVERNED_MIN_DEPTH);
        assert_eq!(b.max, 32, "headroom is 2x the baseline");
    }

    #[test]
    fn weighted_bounds_stay_consistent_for_a_tiny_baseline() {
        // A single-slot baseline must not produce min > max or an out-of-range
        // initial; min clamps down to the baseline.
        let b = CeilingBounds::from_baseline(1);
        assert_eq!(b.min, 1);
        assert!(b.min <= b.max && b.initial >= b.min && b.initial <= b.max);
    }

    #[test]
    fn weighted_bounds_cap_at_max_capacity() {
        let b = CeilingBounds::from_baseline(MAX_CAPACITY - 1);
        assert_eq!(b.max, MAX_CAPACITY, "2x is clamped to the hard limit");
        assert!(b.initial <= b.max);
    }

    // --- selection by mode ---------------------------------------------------

    #[test]
    fn weighted_off_mode_is_a_fixed_bound() {
        let q = governed_work_queue(8, GovernorMode::Off);
        assert!(!q.is_governed(), "off mode must not attach a rope");
        assert!(q.rope().is_none());
        assert_eq!(q.current_capacity(), 8, "fixed at the baseline");
        assert_eq!(q.capacity_bounds(), None, "fixed capacity does not move");
        assert_eq!(q.reorder_window(), 8);
    }

    #[test]
    fn weighted_observe_mode_is_governed() {
        let q = governed_work_queue(8, GovernorMode::Observe);
        assert!(q.is_governed(), "observe mode attaches a rope");
        assert_eq!(q.current_capacity(), 8, "starts at the static baseline");
        assert_eq!(
            q.capacity_bounds(),
            Some((GOVERNED_MIN_DEPTH, 16)),
            "ceiling range is [min, 2x baseline]"
        );
        assert_eq!(q.reorder_window(), 16, "reorder window covers max ceiling");
    }

    #[test]
    #[should_panic(expected = "non-zero")]
    fn weighted_zero_baseline_panics() {
        let _ = governed_work_queue(0, GovernorMode::Off);
    }

    // --- retune / actuation --------------------------------------------------

    #[test]
    fn weighted_retune_is_a_noop_on_the_fixed_path() {
        let q = governed_work_queue(8, GovernorMode::Off);
        let mut gov = Governor::spawn(GovernorConfig::new(GovernorMode::Off));
        assert_eq!(q.retune(&gov, 4096), 8, "fixed capacity is returned as-is");
        assert_eq!(q.current_capacity(), 8);
        gov.shutdown();
    }

    #[test]
    fn weighted_retune_holds_at_baseline_without_a_drum() {
        let q = governed_work_queue(8, GovernorMode::Observe);
        let mut gov = Governor::spawn(GovernorConfig::new(GovernorMode::Observe));
        // No telemetry: drum is Unknown, so the ceiling is held at the baseline.
        assert_eq!(q.retune(&gov, 4096), 8);
        assert_eq!(q.current_capacity(), 8);
        gov.shutdown();
    }

    #[test]
    fn weighted_retune_stays_within_min_max_for_any_drum() {
        // Drive the ceiling directly through the rope over a wide range of drum
        // rates and unit sizes; every applied ceiling must land in [min, max].
        let q = governed_work_queue(8, GovernorMode::Observe);
        let rope = q.rope().expect("governed").clone();
        let (min, max) = q.capacity_bounds().expect("bounds");
        for &rate in &[1.0, 1e3, 1e6, 1e9, 1e15, f64::MAX] {
            for &unit in &[0u64, 1, 4096, 1 << 20, 1 << 30, u64::MAX] {
                let applied = rope.actuate(rate, unit);
                assert!(
                    (min..=max).contains(&applied),
                    "ceiling {applied} out of [{min},{max}] rate={rate} unit={unit}"
                );
                assert_eq!(applied, q.current_capacity());
            }
        }
    }

    #[test]
    fn weighted_permit_for_a_huge_file_admits_fewer_than_a_small_file() {
        // A tight memory budget must admit proportionally fewer huge-file
        // permits than small-file permits: the "a 4 GiB permit != a 4 KiB
        // permit" invariant.
        let q = governed_work_queue(1024, GovernorMode::Observe)
            .with_sizing(Duration::from_millis(250), 8 * 1024 * 1024);
        let rope = q.rope().expect("governed").clone();
        let huge = rope.target_ceiling(1e15, 32 * 1024 * 1024);
        let small = rope.target_ceiling(1e15, 4096);
        assert!(
            huge < small,
            "huge-file admission {huge} must be tighter than small-file {small}"
        );
    }

    #[test]
    fn weighted_zero_size_work_unit_does_not_divide_by_zero() {
        let q = governed_work_queue(8, GovernorMode::Observe);
        let rope = q.rope().expect("governed").clone();
        let (min, max) = q.capacity_bounds().expect("bounds");
        let applied = rope.actuate(1e6, 0);
        assert!(
            (min..=max).contains(&applied),
            "zero-size unit gave {applied}"
        );
    }

    // --- admission accounting: acquired == released at quiescence ------------

    #[test]
    fn weighted_permits_are_conserved_across_a_full_drain() {
        // Push a batch through the governed queue's real drain path and assert
        // the semaphore returns to zero in-flight (every acquire paired with a
        // release) with its ceiling intact.
        const COUNT: u32 = 200;
        let q = governed_work_queue(8, GovernorMode::Observe);
        let sem = q.rope().expect("governed").semaphore_arc();
        let cap_before = q.current_capacity();
        let (tx, rx) = q.into_parts();
        let producer = std::thread::spawn(move || {
            for i in 0..COUNT {
                let work = DeltaWork::whole_file(i, PathBuf::from(format!("/dst/{i}")), 4096)
                    .with_sequence(u64::from(i));
                tx.send(work).expect("send");
            }
        });
        let drained: Vec<u32> = rx.drain_parallel(|w| w.ndx().get());
        producer.join().expect("producer join");
        assert_eq!(drained.len(), COUNT as usize, "every item drained");
        assert_eq!(sem.in_flight(), 0, "no permit leaked at quiescence");
        assert_eq!(sem.current_cap(), cap_before, "ceiling intact after drain");
    }

    #[test]
    fn weighted_fixed_and_dynamic_admit_the_same_items() {
        // Differential: for the same batch, the off (fixed) and on (governed,
        // held at baseline) paths deliver the identical set of ndx values.
        const COUNT: u32 = 128;
        let run = |mode| {
            let (tx, rx) = governed_work_queue(8, mode).into_parts();
            let producer = std::thread::spawn(move || {
                for i in 0..COUNT {
                    tx.send(
                        DeltaWork::whole_file(i, PathBuf::from("/dst"), 64)
                            .with_sequence(u64::from(i)),
                    )
                    .expect("send");
                }
            });
            let mut out: Vec<u32> = rx.drain_parallel(|w| w.ndx().get());
            producer.join().expect("join");
            out.sort_unstable();
            out
        };
        assert_eq!(
            run(GovernorMode::Off),
            run(GovernorMode::Observe),
            "fixed and governed paths must admit the same items"
        );
    }
}
