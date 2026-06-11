//! Allocator for io_uring buffer group IDs (bgid).
//!
//! io_uring provided buffer rings (PBUF_RING) are identified by a 16-bit
//! Buffer Group ID. With only 65 536 possible values, a long-running
//! process that continuously allocates new buffer rings without recycling
//! bgids will eventually exhaust the namespace and silently collide with
//! rings still active in the kernel.
//!
//! This submodule owns the bgid allocator, its process-wide counters
//! (`NEXT_BGID`, `PEAK_USED`, `BGID_EXHAUSTED_COUNT`) and the throttled
//! namespace-pressure and exhaustion warnings. The public surface is
//! re-exported through the parent [`super`] module so consumers keep using
//! `crate::io_uring::buffer_ring::BgidAllocator` etc. unchanged.
//!
//! # Daemon-aware warnings (BGW series)
//!
//! Long-lived daemon processes may exhaust the bgid namespace repeatedly
//! over their lifetime as sessions come and go. The original one-shot
//! exhaustion warning (`warn_bgid_fallback_once`) fired only on the first
//! occurrence, making it invisible to operators monitoring a daemon that
//! has been running for days.
//!
//! The current design uses throttled periodic warnings: exhaustion
//! warnings fire at most once per `BGID_EXHAUSTION_WARN_THROTTLE` window
//! (60 seconds). Each warning includes the cumulative exhaustion count,
//! current in-flight occupancy, and peak usage so operators can correlate
//! pressure with session churn.
//!
//! Per-session tracking is available via `BgidSessionStats`: callers
//! snapshot the process-wide counters at session start and compute the
//! delta at session end to attribute exhaustion events to individual
//! connections.

use std::sync::atomic::{AtomicU16, AtomicU32, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

pub use crate::io_uring_common::BgidAllocError;

/// Maximum number of distinct buffer group IDs available per process.
///
/// The io_uring kernel interface stores bgid as `u16` inside
/// `struct io_uring_buf_reg` (upstream: io_uring/kbuf.c,
/// `io_register_pbuf_ring()`), bounding the namespace to
/// `u16::MAX + 1 = 65 536` values (0..=65 535). Registering a 65 537th
/// group without first unregistering an existing one causes the kernel to
/// return `EEXIST` or silently collide, so callers must stay within this
/// bound.
pub(super) const BGID_NAMESPACE_SIZE: u32 = u16::MAX as u32 + 1;

/// Process-wide monotonic counter for automatic buffer group ID assignment.
///
/// Stored as `u32` so values above `u16::MAX` can be detected without
/// wrapping. Incremented once per [`BgidAllocator::allocate`] call (when
/// the free-list is empty) and decremented only on the boundary call that
/// crosses past the namespace limit, keeping the counter capped at
/// `BGID_NAMESPACE_SIZE` thereafter.
static NEXT_BGID: AtomicU32 = AtomicU32::new(0);

/// Process-wide high-water mark for concurrently-allocated bgids.
///
/// Updated via `fetch_max` after every successful
/// [`BgidAllocator::allocate`] call. Exposed by [`bgid_peak_used`] so
/// operators and tests can observe the worst-case occupancy of the 16-bit
/// namespace over the process lifetime. Never decreases; deallocation
/// returns ids to the free-list but the peak stays.
static PEAK_USED: AtomicU16 = AtomicU16::new(0);

/// Process-wide counter of [`BgidAllocator::allocate`] calls that
/// returned [`BgidAllocError::Exhausted`].
///
/// Read with [`bgid_exhausted_count`]. Each exhausted return increments
/// the counter by one. Monotonic and cumulative for the process lifetime;
/// callers that want a rate compute the delta between two snapshots.
static BGID_EXHAUSTED_COUNT: AtomicU64 = AtomicU64::new(0);

/// Minimum interval between successive bgid exhaustion fallback warnings.
///
/// Set to 60 seconds so a long-lived daemon that experiences repeated
/// exhaustion across many sessions gets periodic visibility without
/// flooding the log. Operators can correlate each warning with the
/// cumulative `exhausted_count` to assess whether the condition is
/// transient (session churn) or sustained (namespace leak).
const BGID_EXHAUSTION_WARN_THROTTLE: Duration = Duration::from_secs(60);

/// Initial capacity reserved for the bgid free-list.
///
/// Sized to cover the typical steady-state churn of a long-running daemon
/// recycling buffer rings without triggering `Vec` reallocations under the
/// free-list mutex. 4 096 entries match a common upper bound on
/// simultaneously-open buffer rings before the namespace warning fires
/// (50 % of u16::MAX is 32 767).
const BGID_FREE_LIST_INITIAL_CAPACITY: usize = 1 << 12;

/// Occupancy threshold (in absolute bgid count) that triggers the
/// throttled namespace-pressure warning.
///
/// Set to 50 % of the 16-bit namespace so operators get early notice that
/// the process is approaching exhaustion while there is still headroom to
/// react.
const BGID_OCCUPANCY_WARN_THRESHOLD: u16 = (BGID_NAMESPACE_SIZE / 2) as u16;

/// Minimum interval between successive namespace-pressure warnings.
const BGID_WARN_THROTTLE: Duration = Duration::from_secs(30);

/// Process-wide free-list of returned bgids available for reuse.
///
/// Populated by [`BgidAllocator::deallocate`] when a
/// [`super::BufferRing`] that was issued a bgid by
/// [`BgidAllocator::allocate`] is dropped. Drained by
/// [`BgidAllocator::allocate`] before incrementing [`NEXT_BGID`], so the
/// monotonic counter only advances when no reusable id is available.
///
/// Pre-sized to `BGID_FREE_LIST_INITIAL_CAPACITY` so the steady-state
/// churn of a long-running daemon does not trigger `Vec` reallocations
/// under the free-list mutex.
fn bgid_free_list() -> &'static Mutex<Vec<u16>> {
    static FREE_LIST: OnceLock<Mutex<Vec<u16>>> = OnceLock::new();
    FREE_LIST.get_or_init(|| Mutex::new(Vec::with_capacity(BGID_FREE_LIST_INITIAL_CAPACITY)))
}

/// Last time the namespace-pressure warning was emitted.
///
/// `None` means "never emitted". Updated under the mutex so two threads
/// observing > 50 % occupancy simultaneously do not both emit a warning.
fn bgid_warn_last() -> &'static Mutex<Option<Instant>> {
    static LAST: OnceLock<Mutex<Option<Instant>>> = OnceLock::new();
    LAST.get_or_init(|| Mutex::new(None))
}

/// Last time the bgid exhaustion fallback warning was emitted.
///
/// Separate from `bgid_warn_last` because the two warnings track
/// different conditions (namespace pressure vs full exhaustion) and fire
/// at different throttle intervals.
fn bgid_exhaustion_warn_last() -> &'static Mutex<Option<Instant>> {
    static LAST: OnceLock<Mutex<Option<Instant>>> = OnceLock::new();
    LAST.get_or_init(|| Mutex::new(None))
}

/// Emits a throttled "BGID space exhausted, falling back"
/// `tracing::warn!` at most once per `BGID_EXHAUSTION_WARN_THROTTLE`
/// window.
///
/// Called from [`super::BufferRing::new_with_allocator`] whenever
/// [`BgidAllocator::allocate`] returns [`BgidAllocError::Exhausted`].
/// Unlike the previous one-shot design, this fires periodically so
/// operators monitoring a long-lived daemon get repeated visibility
/// when the namespace stays exhausted across many sessions. Each
/// warning includes the cumulative exhaustion count, current in-flight
/// occupancy, and peak usage for correlation with session churn.
pub(super) fn warn_bgid_fallback(err: BgidAllocError) {
    let mut last = match bgid_exhaustion_warn_last().lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    let now = Instant::now();
    let fire = match *last {
        None => true,
        Some(t) => now.duration_since(t) >= BGID_EXHAUSTION_WARN_THROTTLE,
    };
    if fire {
        *last = Some(now);
        let BgidAllocError::Exhausted {
            fresh_used,
            free_list_len,
        } = err;
        tracing::warn!(
            target: "fast_io::buffer_ring",
            fresh_used,
            free_list_len,
            exhausted_count = BGID_EXHAUSTED_COUNT.load(Ordering::Relaxed),
            in_flight = current_in_flight(),
            peak_used = PEAK_USED.load(Ordering::Relaxed),
            "BGID space exhausted, falling back to non-registered receive path"
        );
    }
}

/// Emits a throttled `tracing::warn!` when bgid occupancy crosses 50 %.
///
/// The warning fires at most once per `BGID_WARN_THROTTLE` window so a
/// hot allocation path under sustained pressure does not flood the log.
fn maybe_warn_namespace_pressure(in_flight: u16) {
    if in_flight < BGID_OCCUPANCY_WARN_THRESHOLD {
        return;
    }
    let mut last = match bgid_warn_last().lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    let now = Instant::now();
    let fire = match *last {
        None => true,
        Some(t) => now.duration_since(t) >= BGID_WARN_THROTTLE,
    };
    if fire {
        *last = Some(now);
        tracing::warn!(
            target: "fast_io::buffer_ring",
            in_flight,
            namespace = BGID_NAMESPACE_SIZE,
            "io_uring bgid occupancy crossed 50% of the 16-bit namespace"
        );
    }
}

/// Allocator for io_uring buffer group IDs (bgid).
///
/// io_uring provided buffer rings (PBUF_RING) are identified by a 16-bit
/// Buffer Group ID. With only 65 536 possible values, a long-running
/// process that continuously allocates new buffer rings without recycling
/// bgids will eventually exhaust the namespace and silently collide with
/// rings still active in the kernel.
///
/// [`BgidAllocator`] provides a safe, bounded allocation path:
///
/// - [`allocate`](Self::allocate) returns a bgid - either a previously
///   freed id from the internal free-list, or the next monotonic value
///   starting at 0.
/// - [`deallocate`](Self::deallocate) returns a bgid to the free-list so
///   that future [`allocate`](Self::allocate) calls can reuse it.
/// - Once the monotonic counter reaches `BGID_NAMESPACE_SIZE` (65 536)
///   and the free-list is empty, [`allocate`](Self::allocate) returns
///   [`super::BufferRingError::BgidExhausted`] rather than wrapping and
///   silently reusing a bgid still held by an active ring.
///
/// Callers that create a bounded, fixed number of buffer rings per
/// process may set [`super::BufferRingConfig::bgid`] directly with known
/// constants and skip this allocator entirely.
pub struct BgidAllocator;

impl BgidAllocator {
    /// Allocates the next available buffer group ID.
    ///
    /// First drains the internal free-list of previously-deallocated bgids.
    /// If the free-list is empty, falls through to a process-wide monotonic
    /// `u32` counter starting at 0. When the counter would exceed
    /// `u16::MAX` (65 535) - meaning all 65 536 possible bgids have been
    /// issued and none have been returned - returns
    /// [`BgidAllocError::Exhausted`] without panicking and bumps
    /// [`bgid_exhausted_count`].
    ///
    /// # Errors
    ///
    /// Returns [`BgidAllocError::Exhausted`] when both the free-list is
    /// empty and the monotonic counter is at the namespace limit. The
    /// error carries the live `fresh_used` and `free_list_len` snapshot
    /// for operator diagnostics. Callers must drop existing
    /// [`super::BufferRing`] instances that own their bgid (so
    /// [`deallocate`](Self::deallocate) runs in the destructor) to make
    /// ids available again; otherwise the recommended downgrade is to
    /// skip the buffer-ring registration and continue serving with plain
    /// `recv`/`read` on that connection.
    pub fn allocate() -> Result<u16, BgidAllocError> {
        // Reuse a freed id when one is available. The lock is held only for
        // the pop, so contention with concurrent deallocate calls is
        // negligible in practice (one buffer ring per long-running task).
        let popped = bgid_free_list()
            .lock()
            .expect("bgid free-list poisoned")
            .pop();
        if let Some(id) = popped {
            record_allocation();
            return Ok(id);
        }

        // Relaxed ordering is sufficient: uniqueness within the process is
        // guaranteed by the atomic RMW alone; no other memory operations
        // depend on this value being observed in a particular order.
        let id = NEXT_BGID.fetch_add(1, Ordering::Relaxed);
        if id < BGID_NAMESPACE_SIZE {
            record_allocation();
            Ok(id as u16)
        } else {
            // Cap the counter at BGID_NAMESPACE_SIZE rather than letting it
            // climb toward `u32::MAX` and eventually wrap back to 0, which
            // would resume issuing valid u16 IDs that collide with active
            // rings.
            NEXT_BGID.fetch_sub(1, Ordering::Relaxed);
            BGID_EXHAUSTED_COUNT.fetch_add(1, Ordering::Relaxed);
            let free_list_len = bgid_free_list()
                .lock()
                .expect("bgid free-list poisoned")
                .len();
            Err(BgidAllocError::Exhausted {
                fresh_used: BGID_NAMESPACE_SIZE,
                free_list_len,
            })
        }
    }

    /// Allocates up to `count` bgids in a single free-list lock acquisition.
    ///
    /// The hot per-thread bgid-lease path (IUR-3.e) batches allocation so the
    /// process-wide `bgid_free_list` mutex is acquired once per slice
    /// instead of once per id. The returned vector contains as many ids as
    /// were available: it is shorter than `count` only when the namespace
    /// drains mid-batch, in which case the caller can either fall back to
    /// the plain `recv`/`read` path or retry once leased ids are returned.
    ///
    /// Each returned id participates in `PEAK_USED` and
    /// `maybe_warn_namespace_pressure` exactly as if it had been issued
    /// through [`allocate`](Self::allocate), so operator-facing metrics
    /// remain consistent.
    ///
    /// # Errors
    ///
    /// Returns [`BgidAllocError::Exhausted`] only when zero ids could be
    /// obtained (free-list empty *and* counter at the namespace limit).
    /// A short non-empty batch is returned as `Ok` so partial progress is
    /// visible to the caller.
    pub fn allocate_batch(count: usize) -> Result<Vec<u16>, BgidAllocError> {
        if count == 0 {
            return Ok(Vec::new());
        }

        let mut out = Vec::with_capacity(count);
        let mut counter_exhausted = false;

        // Drain freed ids first under a single lock acquisition, then top
        // the batch up from the monotonic counter without holding the lock
        // any longer than necessary.
        {
            let mut free_list = bgid_free_list().lock().expect("bgid free-list poisoned");
            while out.len() < count {
                match free_list.pop() {
                    Some(id) => out.push(id),
                    None => break,
                }
            }
        }

        while out.len() < count {
            let id = NEXT_BGID.fetch_add(1, Ordering::Relaxed);
            if id < BGID_NAMESPACE_SIZE {
                out.push(id as u16);
            } else {
                NEXT_BGID.fetch_sub(1, Ordering::Relaxed);
                counter_exhausted = true;
                break;
            }
        }

        if out.is_empty() {
            BGID_EXHAUSTED_COUNT.fetch_add(1, Ordering::Relaxed);
            let free_list_len = bgid_free_list()
                .lock()
                .expect("bgid free-list poisoned")
                .len();
            return Err(BgidAllocError::Exhausted {
                fresh_used: BGID_NAMESPACE_SIZE,
                free_list_len,
            });
        }

        for _ in &out {
            record_allocation();
        }

        // Surface a single namespace-pressure tick for callers observing
        // `bgid_exhausted_count()` when the batch drained the counter
        // mid-slice, even though we still returned some ids.
        if counter_exhausted {
            BGID_EXHAUSTED_COUNT.fetch_add(1, Ordering::Relaxed);
        }

        Ok(out)
    }

    /// Returns a slice of previously-allocated bgids to the free-list in a
    /// single lock acquisition.
    ///
    /// Mirror image of [`allocate_batch`](Self::allocate_batch): used by
    /// [`super::super::bgid_lease::BgidLease`]'s `Drop` to return every id
    /// it owned without paying the `bgid_free_list` mutex per id. Each
    /// duplicate id present in the input or already in the free-list is
    /// dropped silently to preserve the no-duplicates invariant documented
    /// on [`deallocate`](Self::deallocate).
    pub fn deallocate_batch(bgids: &[u16]) {
        if bgids.is_empty() {
            return;
        }
        let mut free_list = bgid_free_list().lock().expect("bgid free-list poisoned");
        for &bgid in bgids {
            if !free_list.contains(&bgid) {
                free_list.push(bgid);
            }
        }
    }

    /// Returns a previously-allocated bgid to the free-list for reuse.
    ///
    /// Wired into [`super::BufferRing`]'s `Drop` implementation when the
    /// ring's bgid was issued by [`allocate`](Self::allocate); callers
    /// should not normally invoke this directly. The next call to
    /// [`allocate`](Self::allocate) will return this id before advancing
    /// the monotonic counter.
    ///
    /// # Idempotence
    ///
    /// Calling `deallocate` more than once for the same bgid is a no-op
    /// after the first call - the duplicate is silently dropped so the
    /// free-list never contains the same id twice. This defends against
    /// double-drop scenarios where, e.g., a buffer ring is moved out of an
    /// `Option` and the original holder is also dropped.
    ///
    /// # Assumption
    ///
    /// The caller must own `bgid`: it must have been returned by a prior
    /// [`allocate`](Self::allocate) call and not handed back through this
    /// method since. Returning a caller-provided constant (a bgid that was
    /// never issued by this allocator) pollutes the free-list and causes a
    /// later [`allocate`](Self::allocate) to issue an id that may collide
    /// with a ring active elsewhere in the process.
    pub fn deallocate(bgid: u16) {
        let mut free_list = bgid_free_list().lock().expect("bgid free-list poisoned");
        if !free_list.contains(&bgid) {
            free_list.push(bgid);
        }
    }

    /// Returns the number of bgids remaining in the namespace.
    ///
    /// Includes both unallocated counter slots and free-list entries
    /// available for reuse. When this reaches zero,
    /// [`allocate`](Self::allocate) returns
    /// [`super::BufferRingError::BgidExhausted`]. The value may decrease
    /// concurrently as other threads allocate.
    pub fn remaining() -> u32 {
        let used = NEXT_BGID.load(Ordering::Relaxed).min(BGID_NAMESPACE_SIZE);
        let free = bgid_free_list()
            .lock()
            .expect("bgid free-list poisoned")
            .len() as u32;
        BGID_NAMESPACE_SIZE - used + free
    }
}

/// Updates `PEAK_USED` and fires the throttled namespace-pressure
/// warning when in-flight occupancy crosses `BGID_OCCUPANCY_WARN_THRESHOLD`.
///
/// Called once per successful [`BgidAllocator::allocate`] return so the
/// high-water mark reflects every issued id, whether the slot came from
/// the free-list or from advancing the monotonic counter.
fn record_allocation() {
    let in_flight = current_in_flight();
    PEAK_USED.fetch_max(in_flight, Ordering::Relaxed);
    maybe_warn_namespace_pressure(in_flight);
}

/// Computes the current number of allocator-issued bgids that have not
/// been returned to the free-list.
///
/// Saturates at `u16::MAX` so the result always fits in `u16`. The
/// snapshot is best-effort: under concurrent allocate/deallocate the
/// counter and free-list reads are not atomic together, but the value
/// never overstates occupancy because both inputs are observed under the
/// same monotonic discipline.
fn current_in_flight() -> u16 {
    let issued = NEXT_BGID.load(Ordering::Relaxed).min(BGID_NAMESPACE_SIZE);
    let free = bgid_free_list()
        .lock()
        .expect("bgid free-list poisoned")
        .len() as u32;
    issued.saturating_sub(free).min(u16::MAX as u32) as u16
}

/// Returns the high-water mark for concurrently-allocated bgids since
/// process start.
///
/// Monotonic: deallocation never lowers this value. Reflects the worst
/// observed pressure on the 16-bit bgid namespace and is intended for
/// operational dashboards and capacity-planning tests.
#[must_use]
pub fn bgid_peak_used() -> u16 {
    PEAK_USED.load(Ordering::Relaxed)
}

/// Returns the current count of allocator-issued bgids not yet returned
/// to the free-list.
///
/// Saturates at `u16::MAX`. Intended for ad-hoc diagnostics; the value
/// can change between the read and the next allocate/deallocate call.
#[must_use]
pub fn bgid_inflight() -> u16 {
    current_in_flight()
}

/// Returns the cumulative number of [`BgidAllocator::allocate`] calls
/// that returned [`BgidAllocError::Exhausted`] for this process.
///
/// Monotonic and never resets while the process is alive. A non-zero
/// value indicates the caller-side fallback path (skip buffer-ring
/// registration, use plain `recv`/`read`) has been exercised at least
/// once; pair with [`bgid_peak_used`] / [`bgid_inflight`] to size the
/// namespace correctly for the workload.
#[must_use]
pub fn bgid_exhausted_count() -> u64 {
    BGID_EXHAUSTED_COUNT.load(Ordering::Relaxed)
}

/// Consistent snapshot of all process-wide bgid allocator counters.
///
/// Intended for operator diagnostics, logging on daemon session teardown,
/// and health-check endpoints. The values are sampled best-effort (not
/// under a single lock), so they may be slightly inconsistent under
/// concurrent allocation/deallocation - but they never overstate the
/// exhaustion count or in-flight count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BgidSnapshot {
    /// Cumulative exhaustion events since process start.
    pub exhausted_count: u64,
    /// Current bgids checked out (not yet returned to the free-list).
    pub in_flight: u16,
    /// High-water mark for concurrent bgid occupancy.
    pub peak_used: u16,
    /// Bgids available for allocation (counter headroom + free-list).
    pub remaining: u32,
}

/// Takes a consistent snapshot of the process-wide bgid allocator state.
///
/// Suitable for structured logging at session boundaries, health-check
/// responses, or periodic daemon monitoring. The snapshot captures all
/// four operator-facing metrics in a single call so the values are
/// temporally close.
#[must_use]
pub fn bgid_snapshot() -> BgidSnapshot {
    BgidSnapshot {
        exhausted_count: bgid_exhausted_count(),
        in_flight: bgid_inflight(),
        peak_used: bgid_peak_used(),
        remaining: BgidAllocator::remaining(),
    }
}

/// Per-session bgid exhaustion tracker.
///
/// Captures the process-wide exhaustion counter at construction time and
/// computes the delta at any later point. Daemon session handlers create
/// one at session start and query it at session end to attribute bgid
/// exhaustion events to individual connections.
///
/// # Usage
///
/// ```ignore
/// let session_stats = BgidSessionStats::new();
/// // ... run the session transfer ...
/// let exhaustions = session_stats.exhaustions_since_start();
/// if exhaustions > 0 {
///     tracing::warn!(
///         exhaustions,
///         "session experienced bgid exhaustion"
///     );
/// }
/// ```
#[derive(Debug, Clone, Copy)]
pub struct BgidSessionStats {
    /// Snapshot of `BGID_EXHAUSTED_COUNT` at session start.
    start_exhausted: u64,
    /// Snapshot of in-flight bgids at session start.
    start_in_flight: u16,
}

impl BgidSessionStats {
    /// Captures the current process-wide counters as the session baseline.
    #[must_use]
    pub fn new() -> Self {
        Self {
            start_exhausted: bgid_exhausted_count(),
            start_in_flight: bgid_inflight(),
        }
    }

    /// Returns the number of bgid exhaustion events that occurred since
    /// this session started.
    ///
    /// A positive value means at least one allocation during this session
    /// fell back to the non-registered receive path.
    #[must_use]
    pub fn exhaustions_since_start(&self) -> u64 {
        bgid_exhausted_count().saturating_sub(self.start_exhausted)
    }

    /// Returns the in-flight bgid count at the time this session started.
    #[must_use]
    pub fn start_in_flight(&self) -> u16 {
        self.start_in_flight
    }

    /// Returns the current in-flight bgid count.
    #[must_use]
    pub fn current_in_flight(&self) -> u16 {
        bgid_inflight()
    }

    /// Returns the net change in in-flight bgids since session start.
    ///
    /// Positive means more bgids are checked out now than when the session
    /// began (potential leak). Negative means bgids were returned (normal
    /// churn). The value wraps through zero safely via saturating
    /// arithmetic on the unsigned delta.
    #[must_use]
    pub fn in_flight_delta(&self) -> i32 {
        i32::from(bgid_inflight()) - i32::from(self.start_in_flight)
    }
}

impl Default for BgidSessionStats {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests;
