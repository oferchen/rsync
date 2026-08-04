//! The flist rope: a bounded, byte-weighted look-ahead window that paces the
//! file-list producer to the pipeline's drum (DBR Gap G4).
//!
//! # The problem
//!
//! Upstream rsync never materialises the whole file list up front. In
//! incremental-recursion mode the sender extends the list lazily, one segment at
//! a time, only when the receiver signals it needs more
//! (upstream: `flist.c:send_extra_file_list(f, at_least)`, flist.c:2124). That
//! on-demand pacing is what keeps sender memory flat on a ten-million-file tree.
//!
//! oc's producer can, by contrast, run arbitrarily far ahead of the consumer,
//! retaining one `FileEntry` per enumerated path. At scale that is the 1M-file
//! linear-retention problem: peak resident memory grows with the *whole* tree
//! rather than with the work actually in flight.
//!
//! # The rope
//!
//! In a drum-buffer-rope pipeline the *rope* ties the producer's admission to the
//! drum's pace, so the producer can never run further ahead than the buffer in
//! front of the constraint can absorb. This module realises that rope for the
//! file-list producer as a **byte-weighted look-ahead window**: before enumerating
//! the next entry the producer [`admit`](FlistWindow::admit)s that entry's
//! *retained footprint in bytes*, and the consumer [`release`](FlistWindow::release)s
//! the same weight once the entry has been drained downstream. When the bytes in
//! flight would exceed the window budget the producer *parks* on a condition
//! variable (it does not spin) and resumes the instant the consumer drains enough
//! to make room. The bound is expressed in **bytes, not entry count**, so a
//! million tiny entries and a handful of long-path entries are accounted on the
//! same resident-memory scale.
//!
//! # Sizing law
//!
//! The window budget is the resident-memory analogue of the delta
//! [`Rope`](super::rope::Rope)'s admission ceiling. Over a protection window `W`
//! the drum drains `rate * W` bytes of downstream work; keeping a `safety`
//! multiple of that in flight ahead of the drum lets a single stalled producer
//! window pass without starving it:
//!
//! ```text
//! budget = clamp(safety * drum_rate_bytes_per_sec * W_secs, [min, max])
//! ```
//!
//! Unlike the delta rope this budget is *bytes of retained flist*, not a permit
//! count, because the resource G4 bounds is producer-ahead memory rather than
//! concurrent delta slots.
//!
//! # Degradation ladder
//!
//! [`FlistRope::disabled`] is a pure pass-through: [`admit`](FlistRope::admit) and
//! [`release`](FlistRope::release) touch no lock and never block, so the producer
//! enumerates in exactly today's order at exactly today's pace - byte-identical,
//! wire-immutable, NDX order unchanged. That is the `OC_RSYNC_GOVERNOR=off`
//! behaviour. [`FlistRope::from_governor`] returns a disabled rope for an off
//! governor and a governor-sized [`bounded`](FlistRope::bounded) rope otherwise,
//! its budget driven from the drum through the same [`GovernorHandle`] seam the
//! delta rope uses.

use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::Duration;

use super::governor::{GovernorHandle, GovernorMode, POLL_INTERVAL};
use super::sample::Constraint;

/// Fixed per-entry retained footprint, in bytes, excluding the path text.
///
/// A buffered `FileEntry` (upstream: `flist.c` `file_struct`, oc:
/// `protocol::flist::entry`) retains its fixed scalar fields - size, mtime,
/// uid/gid, mode, device/inode for hardlink tracking, presence bits - plus the
/// allocation bookkeeping for its heap-owned path. This constant is a
/// deliberately conservative estimate of that fixed cost so the window bounds
/// *resident* memory rather than under-counting it; the variable path length is
/// added on top by [`entry_weight_bytes`]. It is an accounting weight, not a
/// `size_of`, so it stays stable if the struct layout shifts by a few bytes.
pub const FLIST_ENTRY_BASE_BYTES: u64 = 128;

/// Default lower bound on the look-ahead budget, in bytes.
///
/// Even a slow or not-yet-identified drum must let the producer stay a little way
/// ahead of the consumer, or the two would lock-step and serialise. 1 MiB keeps a
/// few thousand average entries in flight - enough to hide per-entry scheduling
/// jitter - while still bounding retention far below an unbounded whole-tree scan.
pub const DEFAULT_MIN_BUDGET_BYTES: u64 = 1024 * 1024;

/// Default upper bound on the look-ahead budget, in bytes.
///
/// 64 MiB caps producer-ahead flist memory regardless of tree size: at the
/// average entry footprint this is on the order of a few hundred thousand
/// buffered entries, so a ten-million-file tree can never pin its whole list
/// resident. Operators who want a tighter or looser cap override it through
/// [`FlistRopeConfig::with_max_budget`].
pub const DEFAULT_MAX_BUDGET_BYTES: u64 = 64 * 1024 * 1024;

/// Default multiple of the drum's per-window demand to keep buffered ahead.
///
/// Two windows of slack lets one stalled producer window pass without starving
/// the drum, matching the delta rope's [`DEFAULT_SAFETY_FACTOR`](super::rope::DEFAULT_SAFETY_FACTOR).
pub const DEFAULT_SAFETY_FACTOR: f64 = 2.0;

/// Returns the retained-memory weight, in bytes, of one buffered file-list entry
/// whose path is `path_len` bytes long.
///
/// This is [`FLIST_ENTRY_BASE_BYTES`] plus the path text, computed with a
/// saturating add so a pathologically long path can never wrap the accounting.
/// The weight is a function of what the *flist* retains - the entry struct and
/// its path - and is deliberately independent of the file's *content* size: a
/// zero-byte file and a terabyte file with equal-length paths pin the same
/// resident memory in the list. Never returns zero, so it is always a safe,
/// forward-progressing admission weight.
#[must_use]
pub fn entry_weight_bytes(path_len: usize) -> u64 {
    FLIST_ENTRY_BASE_BYTES.saturating_add(path_len as u64)
}

/// Mutable window state guarded by the mutex.
struct WindowInner {
    /// Byte budget: the most retained flist the producer may hold ahead.
    budget: u64,
    /// Bytes currently admitted and not yet released.
    in_flight: u64,
    /// High-water mark of `in_flight`, for RSS-bound assertions and telemetry.
    peak_in_flight: u64,
    /// Total [`admit`](FlistWindow::admit) calls.
    admits: u64,
    /// Subset of `admits` that had to park for room.
    blocks: u64,
}

impl WindowInner {
    /// Whether admitting `weight` more bytes would overrun the budget, computed
    /// saturatingly so a huge weight cannot wrap.
    fn would_exceed(&self, weight: u64) -> bool {
        self.in_flight.saturating_add(weight) > self.budget
    }
}

/// A byte-weighted bounded look-ahead window.
///
/// The producer calls [`admit`](Self::admit) before buffering an entry and the
/// consumer calls [`release`](Self::release) once it has drained. Admission
/// blocks on a [`Condvar`] when the window is full and never busy-waits; a
/// release wakes parked producers. The window is `Send + Sync` and shared across
/// the producer and consumer threads via `Arc`.
///
/// # Liveness
///
/// A single entry whose weight exceeds the entire budget - a pathologically long
/// path under a tiny budget - is admitted as soon as the window is otherwise
/// empty rather than blocking forever. Draining can never make room for it
/// beyond an empty window, so refusing it would deadlock the producer; admitting
/// it briefly overshoots the budget by one entry, which is the only safe choice
/// and is bounded by that single entry.
pub struct FlistWindow {
    inner: Mutex<WindowInner>,
    drained: Condvar,
}

impl FlistWindow {
    /// Creates a window with the given byte `budget` (clamped up to at least one
    /// byte so a zero budget cannot forbid all progress).
    #[must_use]
    pub fn new(budget: u64) -> Self {
        Self {
            inner: Mutex::new(WindowInner {
                budget: budget.max(1),
                in_flight: 0,
                peak_in_flight: 0,
                admits: 0,
                blocks: 0,
            }),
            drained: Condvar::new(),
        }
    }

    /// Admits `weight` bytes of look-ahead, blocking until they fit under the
    /// budget.
    ///
    /// Parks on the condition variable while the window is non-empty and adding
    /// `weight` would exceed the budget, re-checking after every wakeup so a
    /// spurious wakeup or a concurrent [`resize`](Self::resize) shrink can never
    /// over-admit. An entry heavier than the whole budget is admitted once the
    /// window drains empty (see the type's liveness note). Accounting is
    /// saturating, so no weight can wrap the in-flight total.
    pub fn admit(&self, weight: u64) {
        let mut inner = self.lock();
        inner.admits += 1;
        if inner.in_flight != 0 && inner.would_exceed(weight) {
            inner.blocks += 1;
            while inner.in_flight != 0 && inner.would_exceed(weight) {
                inner = self.drained.wait(inner).unwrap_or_else(|e| e.into_inner());
            }
        }
        inner.in_flight = inner.in_flight.saturating_add(weight);
        if inner.in_flight > inner.peak_in_flight {
            inner.peak_in_flight = inner.in_flight;
        }
    }

    /// Releases `weight` bytes previously admitted, waking parked producers.
    ///
    /// The in-flight total is decremented saturatingly, so an over-release (more
    /// released than admitted) leaves it at zero rather than underflowing. Every
    /// parked producer is woken via [`Condvar::notify_all`], because freeing
    /// bytes may satisfy a large waiter that a single wake would not.
    pub fn release(&self, weight: u64) {
        let mut inner = self.lock();
        inner.in_flight = inner.in_flight.saturating_sub(weight);
        drop(inner);
        self.drained.notify_all();
    }

    /// Changes the byte budget, waking parked producers when it grows.
    ///
    /// The new budget is clamped up to at least one byte. Growing may open room
    /// for several parked producers at once, so it wakes all of them; shrinking
    /// never revokes bytes already in flight - it only withholds future admission
    /// until releases bring the in-flight total back under the smaller budget.
    pub fn resize(&self, new_budget: u64) {
        let new_budget = new_budget.max(1);
        let mut inner = self.lock();
        let grew = new_budget > inner.budget;
        inner.budget = new_budget;
        drop(inner);
        if grew {
            self.drained.notify_all();
        }
    }

    /// The current byte budget.
    #[must_use]
    pub fn budget(&self) -> u64 {
        self.lock().budget
    }

    /// Bytes currently admitted and not yet released.
    #[must_use]
    pub fn in_flight(&self) -> u64 {
        self.lock().in_flight
    }

    /// The high-water mark of bytes in flight since construction.
    ///
    /// A bounded window keeps this at or below `budget` except for at most one
    /// over-budget entry admitted for liveness (see the type note), so tests can
    /// assert a resident-memory bound directly from it.
    #[must_use]
    pub fn peak_in_flight(&self) -> u64 {
        self.lock().peak_in_flight
    }

    /// Total admissions and the subset that had to park, as `(admits, blocks)`.
    #[must_use]
    pub fn stats(&self) -> (u64, u64) {
        let inner = self.lock();
        (inner.admits, inner.blocks)
    }

    /// Locks the inner state, recovering a poisoned guard.
    ///
    /// The accounting is simple integer arithmetic that cannot be left
    /// inconsistent, so a panic in another thread must not permanently wedge the
    /// window; recovering the guard keeps it usable.
    fn lock(&self) -> MutexGuard<'_, WindowInner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl std::fmt::Debug for FlistWindow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let inner = self.lock();
        f.debug_struct("FlistWindow")
            .field("budget", &inner.budget)
            .field("in_flight", &inner.in_flight)
            .field("peak_in_flight", &inner.peak_in_flight)
            .field("admits", &inner.admits)
            .field("blocks", &inner.blocks)
            .finish()
    }
}

/// Immutable sizing policy for a [`FlistRope`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FlistRopeConfig {
    /// Smallest budget the rope may set, in bytes (liveness floor, `>= 1`).
    min_budget: u64,
    /// Largest budget the rope may set, in bytes.
    max_budget: u64,
    /// Multiple of per-window drum demand to keep buffered ahead.
    safety_factor: f64,
    /// Control window the budget must keep the drum fed across.
    protection_window: Duration,
}

/// Error returned when a [`FlistRopeConfig`] is internally inconsistent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlistRopeConfigError {
    /// `min_budget` was zero; at least one byte is required for liveness.
    MinTooLow,
    /// `min_budget` exceeded `max_budget`.
    MinAboveMax {
        /// The requested minimum budget.
        min: u64,
        /// The requested maximum budget.
        max: u64,
    },
    /// `safety_factor` was not a finite, strictly-positive number.
    BadSafetyFactor,
}

impl std::fmt::Display for FlistRopeConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FlistRopeConfigError::MinTooLow => {
                f.write_str("flist rope min budget must be at least 1 byte")
            }
            FlistRopeConfigError::MinAboveMax { min, max } => {
                write!(f, "flist rope min budget {min} exceeds max {max}")
            }
            FlistRopeConfigError::BadSafetyFactor => {
                f.write_str("flist rope safety factor must be finite and positive")
            }
        }
    }
}

impl std::error::Error for FlistRopeConfigError {}

impl Default for FlistRopeConfig {
    fn default() -> Self {
        Self {
            min_budget: DEFAULT_MIN_BUDGET_BYTES,
            max_budget: DEFAULT_MAX_BUDGET_BYTES,
            safety_factor: DEFAULT_SAFETY_FACTOR,
            protection_window: POLL_INTERVAL,
        }
    }
}

impl FlistRopeConfig {
    /// Builds a config for the `[min, max]` byte-budget range with the default
    /// safety factor and protection window ([`POLL_INTERVAL`]).
    ///
    /// # Errors
    ///
    /// Returns [`FlistRopeConfigError`] when the range is inconsistent
    /// (`min == 0` or `min > max`).
    pub fn new(min_budget: u64, max_budget: u64) -> Result<Self, FlistRopeConfigError> {
        Self {
            min_budget,
            max_budget,
            safety_factor: DEFAULT_SAFETY_FACTOR,
            protection_window: POLL_INTERVAL,
        }
        .validated()
    }

    fn validated(self) -> Result<Self, FlistRopeConfigError> {
        if self.min_budget < 1 {
            return Err(FlistRopeConfigError::MinTooLow);
        }
        if self.min_budget > self.max_budget {
            return Err(FlistRopeConfigError::MinAboveMax {
                min: self.min_budget,
                max: self.max_budget,
            });
        }
        if !self.safety_factor.is_finite() || self.safety_factor <= 0.0 {
            return Err(FlistRopeConfigError::BadSafetyFactor);
        }
        Ok(self)
    }

    /// Overrides the maximum budget.
    ///
    /// # Errors
    ///
    /// Returns [`FlistRopeConfigError::MinAboveMax`] if `bytes` is below the
    /// configured minimum.
    pub fn with_max_budget(mut self, bytes: u64) -> Result<Self, FlistRopeConfigError> {
        self.max_budget = bytes;
        self.validated()
    }

    /// Overrides the safety factor.
    ///
    /// # Errors
    ///
    /// Returns [`FlistRopeConfigError::BadSafetyFactor`] if `factor` is not
    /// finite and positive.
    pub fn with_safety_factor(mut self, factor: f64) -> Result<Self, FlistRopeConfigError> {
        self.safety_factor = factor;
        self.validated()
    }

    /// Overrides the protection window.
    #[must_use]
    pub fn with_protection_window(mut self, window: Duration) -> Self {
        self.protection_window = window;
        self
    }

    /// The configured minimum budget.
    #[must_use]
    pub fn min_budget(&self) -> u64 {
        self.min_budget
    }

    /// The configured maximum budget.
    #[must_use]
    pub fn max_budget(&self) -> u64 {
        self.max_budget
    }

    /// Computes the target byte budget for a drum draining at `drum_rate_bps`.
    /// The result is always in `[min_budget, max_budget]`.
    #[must_use]
    fn target_budget(&self, drum_rate_bps: f64) -> u64 {
        // Without a usable rate estimate, stay at the conservative floor.
        if !drum_rate_bps.is_finite() || drum_rate_bps <= 0.0 {
            return self.min_budget;
        }
        let window = self.protection_window.as_secs_f64();
        let demand = self.safety_factor * drum_rate_bps * window;
        // Guard the f64 -> u64 cast against NaN/overflow before clamping.
        let target = if demand.is_finite() && demand >= 1.0 {
            demand.min(u64::MAX as f64) as u64
        } else {
            self.min_budget
        };
        target.clamp(self.min_budget, self.max_budget)
    }
}

/// The flist rope actuator: a byte-weighted look-ahead window plus its sizing
/// policy, or a pure pass-through when the governor is off.
///
/// Clone-cheap: a `Bounded` rope shares its window through an `Arc`, so the
/// producer and consumer hold clones of the same rope and see the same window.
#[derive(Debug, Clone)]
pub enum FlistRope {
    /// The governor-off path: admission is a no-op, so the producer keeps today's
    /// exact enumeration order and pace, byte-identical to the pre-governor code.
    Disabled,
    /// The governor-engaged path: admission is gated by the shared byte window.
    Bounded {
        /// The shared look-ahead window.
        window: Arc<FlistWindow>,
        /// The sizing policy driving [`actuate`](FlistRope::actuate).
        config: FlistRopeConfig,
    },
}

impl FlistRope {
    /// A pass-through rope: [`admit`](Self::admit) and [`release`](Self::release)
    /// do nothing and never block.
    #[must_use]
    pub fn disabled() -> Self {
        FlistRope::Disabled
    }

    /// A bounded rope whose window starts at the config's minimum budget.
    #[must_use]
    pub fn bounded(config: FlistRopeConfig) -> Self {
        FlistRope::Bounded {
            window: Arc::new(FlistWindow::new(config.min_budget)),
            config,
        }
    }

    /// Builds a rope from a governor handle: [`disabled`](Self::disabled) when the
    /// governor is [`GovernorMode::Off`], otherwise a [`bounded`](Self::bounded)
    /// rope sized by `config`.
    ///
    /// This is the degradation-ladder entry point: an off governor yields the
    /// byte-identical pass-through, so wiring the rope into the producer costs
    /// nothing on the default path.
    #[must_use]
    pub fn from_governor(governor: &GovernorHandle, config: FlistRopeConfig) -> Self {
        match governor.mode() {
            GovernorMode::Off => FlistRope::Disabled,
            GovernorMode::Observe => FlistRope::bounded(config),
        }
    }

    /// Whether this rope actually bounds the producer.
    #[must_use]
    pub fn is_bounded(&self) -> bool {
        matches!(self, FlistRope::Bounded { .. })
    }

    /// Admits `weight` bytes of look-ahead before the producer buffers an entry.
    ///
    /// A no-op on a [`disabled`](Self::disabled) rope; otherwise blocks on the
    /// window until the bytes fit (see [`FlistWindow::admit`]).
    pub fn admit(&self, weight: u64) {
        if let FlistRope::Bounded { window, .. } = self {
            window.admit(weight);
        }
    }

    /// Releases `weight` bytes after the consumer drains an entry.
    ///
    /// A no-op on a [`disabled`](Self::disabled) rope; otherwise frees window room
    /// and wakes parked producers (see [`FlistWindow::release`]).
    pub fn release(&self, weight: u64) {
        if let FlistRope::Bounded { window, .. } = self {
            window.release(weight);
        }
    }

    /// Resizes the window to the target budget for a drum draining at
    /// `drum_rate_bps` and returns the applied budget.
    ///
    /// A no-op returning `0` on a disabled rope. On a bounded rope the target is
    /// clamped into `[min_budget, max_budget]` and applied to the window.
    pub fn actuate(&self, drum_rate_bps: f64) -> u64 {
        match self {
            FlistRope::Disabled => 0,
            FlistRope::Bounded { window, config } => {
                let target = config.target_budget(drum_rate_bps);
                window.resize(target);
                target
            }
        }
    }

    /// Drives the window budget from a live [`GovernorHandle`]: sizes from the
    /// drum's throughput, or holds the current budget when no drum is identified
    /// yet.
    ///
    /// Returns the applied budget (the current budget when held, `0` when
    /// disabled).
    pub fn actuate_from(&self, governor: &GovernorHandle) -> u64 {
        match self {
            FlistRope::Disabled => 0,
            FlistRope::Bounded { window, .. } => {
                let drum = governor.drum();
                if drum == Constraint::Unknown {
                    return window.budget();
                }
                match governor.throughput(drum) {
                    Some(rate) => self.actuate(rate),
                    None => window.budget(),
                }
            }
        }
    }

    /// The window's current byte budget, or `0` when disabled.
    #[must_use]
    pub fn budget(&self) -> u64 {
        match self {
            FlistRope::Disabled => 0,
            FlistRope::Bounded { window, .. } => window.budget(),
        }
    }

    /// The window's peak bytes in flight, or `0` when disabled.
    #[must_use]
    pub fn peak_in_flight(&self) -> u64 {
        match self {
            FlistRope::Disabled => 0,
            FlistRope::Bounded { window, .. } => window.peak_in_flight(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Instant;

    const RECV_TIMEOUT: Duration = Duration::from_secs(5);

    // --- weight accounting ---------------------------------------------------

    #[test]
    fn entry_weight_is_base_plus_path_and_never_zero() {
        // A zero-size file with an empty path still costs the fixed footprint.
        assert_eq!(entry_weight_bytes(0), FLIST_ENTRY_BASE_BYTES);
        assert_eq!(entry_weight_bytes(7), FLIST_ENTRY_BASE_BYTES + 7);
        // Content size is irrelevant to flist retention: only the path grows it.
        assert!(entry_weight_bytes(1) > 0);
    }

    #[test]
    fn entry_weight_is_overflow_safe_for_huge_paths() {
        // A pathologically long path must saturate, not wrap.
        assert_eq!(entry_weight_bytes(usize::MAX), u64::MAX);
    }

    #[test]
    fn window_accounting_is_overflow_safe() {
        // Admitting huge weights must saturate the in-flight total, never wrap,
        // and over-release must floor at zero rather than underflow.
        let w = FlistWindow::new(u64::MAX);
        w.admit(u64::MAX);
        w.admit(u64::MAX);
        assert_eq!(w.in_flight(), u64::MAX, "in-flight saturates, not wraps");
        w.release(1);
        w.release(u64::MAX);
        assert_eq!(w.in_flight(), 0, "over-release floors at zero");
    }

    // --- bound enforcement ---------------------------------------------------

    #[test]
    fn admit_stays_within_budget_for_fitting_entries() {
        let w = FlistWindow::new(100);
        w.admit(40);
        w.admit(40);
        assert_eq!(w.in_flight(), 80);
        // A third 40 would exceed 100; drain first, then it fits.
        w.release(40);
        w.admit(40);
        assert_eq!(w.in_flight(), 80);
        assert!(w.peak_in_flight() <= 100, "budget never exceeded");
    }

    #[test]
    fn oversized_lone_entry_is_admitted_for_liveness() {
        // A single entry heavier than the whole budget must not deadlock: it is
        // admitted once the window is empty, overshooting by exactly that entry.
        let w = FlistWindow::new(10);
        w.admit(1_000);
        assert_eq!(w.in_flight(), 1_000);
        w.release(1_000);
        assert_eq!(w.in_flight(), 0);
    }

    // --- parking (not spinning) ----------------------------------------------

    #[test]
    fn producer_parks_when_full_and_resumes_on_drain() {
        let w = Arc::new(FlistWindow::new(100));
        w.admit(100); // window is now exactly full
        let (tx, rx) = mpsc::channel();
        let worker = {
            let w = Arc::clone(&w);
            std::thread::spawn(move || {
                // Must park: 100 + 50 > 100 and the window is non-empty.
                w.admit(50);
                tx.send(()).unwrap();
            })
        };
        // The producer cannot have been admitted yet.
        assert!(
            rx.recv_timeout(Duration::from_millis(50)).is_err(),
            "producer should still be parked"
        );
        // Draining enough room wakes it.
        w.release(60);
        rx.recv_timeout(RECV_TIMEOUT)
            .expect("producer should resume after drain");
        worker.join().unwrap();
        let (admits, blocks) = w.stats();
        assert_eq!(admits, 2);
        assert_eq!(blocks, 1, "the second admit had to park exactly once");
    }

    #[test]
    fn resize_grow_unblocks_a_parked_producer() {
        let w = Arc::new(FlistWindow::new(100));
        w.admit(100);
        let (tx, rx) = mpsc::channel();
        let worker = {
            let w = Arc::clone(&w);
            std::thread::spawn(move || {
                w.admit(50);
                tx.send(()).unwrap();
            })
        };
        assert!(rx.recv_timeout(Duration::from_millis(50)).is_err());
        // Growing the budget alone - no release - must wake the waiter.
        w.resize(200);
        rx.recv_timeout(RECV_TIMEOUT)
            .expect("grow should wake the parked producer");
        worker.join().unwrap();
        assert_eq!(w.in_flight(), 150);
    }

    #[test]
    fn resize_shrink_withholds_but_does_not_revoke() {
        let w = FlistWindow::new(200);
        w.admit(150);
        w.resize(100); // below in-flight: existing bytes stay admitted
        assert_eq!(w.budget(), 100);
        assert_eq!(w.in_flight(), 150, "shrink never revokes in-flight bytes");
    }

    // --- sizing law ----------------------------------------------------------

    #[test]
    fn no_rate_estimate_sizes_to_min_budget() {
        let cfg = FlistRopeConfig::new(4096, 1 << 20).unwrap();
        assert_eq!(cfg.target_budget(0.0), 4096);
        assert_eq!(cfg.target_budget(-1.0), 4096);
        assert_eq!(cfg.target_budget(f64::NAN), 4096);
        assert_eq!(cfg.target_budget(f64::INFINITY), 4096);
    }

    #[test]
    fn faster_drum_demands_a_larger_budget_clamped_to_range() {
        // 0.25 s window, safety 2. At 100 MB/s: 2*1e8*0.25 = 5e7 -> clamped to max.
        // At 8 KB/s: 2*8e3*0.25 = 4000 -> between min and max.
        let cfg = FlistRopeConfig::new(1024, 8 * 1024 * 1024).unwrap();
        let slow = cfg.target_budget(8_000.0);
        let fast = cfg.target_budget(100_000_000.0);
        assert_eq!(slow, 4_000);
        assert_eq!(fast, 8 * 1024 * 1024, "a fast drum saturates the max budget");
        assert!(fast > slow);
    }

    #[test]
    fn target_budget_always_within_range() {
        let cfg = FlistRopeConfig::new(2048, 1 << 24).unwrap();
        for &rate in &[1.0, 1e3, 1e6, 1e12, 1e30, f64::MAX] {
            let b = cfg.target_budget(rate);
            assert!(
                (2048..=(1 << 24)).contains(&b),
                "budget {b} out of range for rate {rate}"
            );
        }
    }

    // --- config validation ---------------------------------------------------

    #[test]
    fn config_rejects_inconsistent_ranges_and_factors() {
        assert_eq!(
            FlistRopeConfig::new(0, 4096),
            Err(FlistRopeConfigError::MinTooLow)
        );
        assert_eq!(
            FlistRopeConfig::new(8, 4),
            Err(FlistRopeConfigError::MinAboveMax { min: 8, max: 4 })
        );
        assert_eq!(
            FlistRopeConfig::new(1, 4096).unwrap().with_safety_factor(0.0),
            Err(FlistRopeConfigError::BadSafetyFactor)
        );
        assert_eq!(
            FlistRopeConfig::new(1, 4096)
                .unwrap()
                .with_safety_factor(f64::NAN),
            Err(FlistRopeConfigError::BadSafetyFactor)
        );
        assert_eq!(
            FlistRopeConfig::new(4096, 1 << 20).unwrap().with_max_budget(1),
            Err(FlistRopeConfigError::MinAboveMax {
                min: 4096,
                max: 1
            })
        );
    }

    // --- rope actuation ------------------------------------------------------

    #[test]
    fn disabled_rope_is_a_pure_passthrough() {
        let r = FlistRope::disabled();
        assert!(!r.is_bounded());
        // Admission and release never block and never account.
        r.admit(1_000_000);
        r.release(1_000_000);
        assert_eq!(r.budget(), 0);
        assert_eq!(r.peak_in_flight(), 0);
        assert_eq!(r.actuate(1e9), 0);
    }

    #[test]
    fn bounded_rope_actuates_budget_from_rate() {
        let cfg = FlistRopeConfig::new(1024, 8 * 1024 * 1024).unwrap();
        let r = FlistRope::bounded(cfg);
        assert!(r.is_bounded());
        assert_eq!(r.budget(), 1024, "starts at the floor");
        let applied = r.actuate(100_000_000.0);
        assert_eq!(applied, 8 * 1024 * 1024);
        assert_eq!(r.budget(), applied);
        // A stalled drum pulls the budget back to the floor.
        assert_eq!(r.actuate(0.0), 1024);
        assert_eq!(r.budget(), 1024);
    }

    #[test]
    fn from_governor_off_is_disabled_engaged_is_bounded() {
        use crate::throughput::{Governor, GovernorConfig};
        let cfg = FlistRopeConfig::default();

        let mut off = Governor::spawn(GovernorConfig::new(GovernorMode::Off));
        assert!(!FlistRope::from_governor(&off, cfg).is_bounded());
        off.shutdown();

        let mut on = Governor::spawn(GovernorConfig::new(GovernorMode::Observe));
        let rope = FlistRope::from_governor(&on, cfg);
        assert!(rope.is_bounded());
        // No drum identified yet: the budget is held at the floor.
        assert_eq!(rope.actuate_from(&on), rope.budget());
        on.shutdown();
    }

    // --- differential: OFF enumerates identically to a no-rope baseline -------

    /// Drives a single-producer / single-consumer enumeration through `rope` and
    /// returns the exact sequence the consumer observed. With a disabled rope
    /// this is the pre-governor behaviour; with a bounded rope the *order* must be
    /// identical and only the pacing differs.
    fn run_enumeration(rope: FlistRope, path_lens: &[usize]) -> Vec<usize> {
        let (tx, rx) = mpsc::channel::<(usize, u64)>();
        let producer_rope = rope.clone();
        let sizes: Vec<usize> = path_lens.to_vec();
        let producer = std::thread::spawn(move || {
            for (ndx, &len) in sizes.iter().enumerate() {
                let weight = entry_weight_bytes(len);
                producer_rope.admit(weight);
                // Send the NDX in strict enumeration order, with its weight so the
                // consumer can release exactly what was admitted.
                tx.send((ndx, weight)).unwrap();
            }
        });
        let mut order = Vec::with_capacity(path_lens.len());
        for (ndx, weight) in rx {
            order.push(ndx);
            rope.release(weight);
        }
        producer.join().unwrap();
        order
    }

    #[test]
    fn off_and_bounded_ropes_enumerate_in_identical_order() {
        let path_lens: Vec<usize> = (0..500).map(|i| 1 + (i * 37) % 4096).collect();
        // Baseline: today's behaviour (no rope at all == disabled rope).
        let baseline = run_enumeration(FlistRope::disabled(), &path_lens);
        let expected: Vec<usize> = (0..500).collect();
        assert_eq!(baseline, expected, "disabled rope must preserve NDX order");
        // A tight bounded rope forces frequent parking but must not reorder.
        let cfg = FlistRopeConfig::new(1024, 4096).unwrap();
        let bounded = run_enumeration(FlistRope::bounded(cfg), &path_lens);
        assert_eq!(
            bounded, baseline,
            "roping changes pacing, never enumeration order"
        );
    }

    // --- RSS bound at 1M-file scale ------------------------------------------

    #[test]
    fn bounded_window_caps_retention_across_a_million_entries() {
        // Stream a million-entry-style workload through a small window with a slow
        // consumer and assert peak in-flight stays within one entry of the budget
        // - i.e. retention is bounded by the window, not by the tree size.
        const ENTRIES: usize = 1_000_000;
        const BUDGET: u64 = 256 * 1024;
        let window = Arc::new(FlistWindow::new(BUDGET));
        // Every entry has an average-ish path; max single weight bounds overshoot.
        let weight = entry_weight_bytes(200);
        let max_overshoot = BUDGET + weight;

        let (tx, rx) = mpsc::channel::<u64>();
        let producer = {
            let window = Arc::clone(&window);
            std::thread::spawn(move || {
                for _ in 0..ENTRIES {
                    window.admit(weight);
                    tx.send(weight).unwrap();
                }
            })
        };
        let mut count = 0usize;
        for w in rx {
            count += 1;
            // The producer cannot have pinned the whole tree: at any moment the
            // in-flight bytes are within one entry of the budget.
            assert!(
                window.in_flight() <= max_overshoot,
                "retention {} exceeded bound {max_overshoot}",
                window.in_flight()
            );
            window.release(w);
        }
        producer.join().unwrap();
        assert_eq!(count, ENTRIES);
        assert!(
            window.peak_in_flight() <= max_overshoot,
            "peak retention {} exceeded bound {max_overshoot} for {ENTRIES} entries",
            window.peak_in_flight()
        );
        // The window genuinely engaged: a bounded run of a million entries through
        // a 256 KiB window must have parked the producer many times.
        let (admits, blocks) = window.stats();
        assert_eq!(admits, ENTRIES as u64);
        assert!(blocks > 0, "a tight window must have parked the producer");
    }
}
