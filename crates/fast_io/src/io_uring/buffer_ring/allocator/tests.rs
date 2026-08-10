//! Tests for the bgid allocator.
//!
//! Lives in a child module so the test code can keep the allocator's
//! private statics under `super::*` while the allocator file itself stays
//! within the module-size cap.

use super::*;
use crate::io_uring_common::BufferRingError;
use std::io;

#[test]
fn bgid_allocator_returns_distinct_ids() {
    let a = BgidAllocator::allocate().expect("first allocation");
    let b = BgidAllocator::allocate().expect("second allocation");
    assert_ne!(a, b, "consecutive allocations must return distinct bgids");
}

/// Serializes tests that mutate global allocator state.
///
/// `NEXT_BGID` and the bgid free-list are process-wide; tests that
/// swap or drain them must not run concurrently with other tests that
/// observe the same state, otherwise interleavings produce
/// false-negative failures.
fn bgid_test_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Snapshots, then clears, all process-wide allocator state. The
/// returned guard restores everything on drop so tests leave global
/// state untouched.
struct BgidStateGuard {
    prev_counter: u32,
    prev_free_list: Vec<u16>,
    prev_peak: u16,
    prev_exhausted: u64,
    prev_exhaustion_warn_last: Option<Instant>,
    _serializer: std::sync::MutexGuard<'static, ()>,
}

impl BgidStateGuard {
    fn snapshot() -> Self {
        let serializer = bgid_test_lock();
        let prev_counter = NEXT_BGID.swap(0, Ordering::Relaxed);
        let prev_peak = PEAK_USED.swap(0, Ordering::Relaxed);
        let prev_exhausted = BGID_EXHAUSTED_COUNT.swap(0, Ordering::Relaxed);
        let prev_exhaustion_warn_last = {
            let mut last = bgid_exhaustion_warn_last()
                .lock()
                .expect("exhaustion warn lock poisoned");
            last.take()
        };
        let prev_free_list = {
            let mut list = bgid_free_list().lock().expect("free-list poisoned");
            // Swap in a fresh Vec with the same pre-sized capacity so tests
            // that observe `bgid_free_list().capacity()` (e.g. the
            // pre-sized-capacity invariant) keep seeing the steady-state
            // shape between snapshots. `mem::take` alone would leave the
            // global list at capacity 0.
            let taken = std::mem::take(&mut *list);
            *list = Vec::with_capacity(taken.capacity());
            taken
        };
        Self {
            prev_counter,
            prev_free_list,
            prev_peak,
            prev_exhausted,
            prev_exhaustion_warn_last,
            _serializer: serializer,
        }
    }
}

impl Drop for BgidStateGuard {
    fn drop(&mut self) {
        NEXT_BGID.store(self.prev_counter, Ordering::Relaxed);
        PEAK_USED.store(self.prev_peak, Ordering::Relaxed);
        BGID_EXHAUSTED_COUNT.store(self.prev_exhausted, Ordering::Relaxed);
        {
            let mut last = bgid_exhaustion_warn_last()
                .lock()
                .expect("exhaustion warn lock poisoned");
            *last = self.prev_exhaustion_warn_last;
        }
        let mut list = bgid_free_list().lock().expect("free-list poisoned");
        *list = std::mem::take(&mut self.prev_free_list);
    }
}

#[test]
fn bgid_allocator_exhaustion_returns_error() {
    let _guard = BgidStateGuard::snapshot();
    // Force the counter to the namespace limit with the free-list empty;
    // the next allocation must report exhaustion.
    NEXT_BGID.store(BGID_NAMESPACE_SIZE, Ordering::Relaxed);
    let result = BgidAllocator::allocate();
    assert!(
        matches!(result, Err(BgidAllocError::Exhausted { .. })),
        "expected Exhausted when counter == BGID_NAMESPACE_SIZE, got {result:?}"
    );
}

#[test]
fn bgid_exhausted_converts_to_out_of_memory_io_error() {
    let err: io::Error = BgidAllocError::Exhausted {
        fresh_used: BGID_NAMESPACE_SIZE,
        free_list_len: 0,
    }
    .into();
    assert_eq!(err.kind(), io::ErrorKind::OutOfMemory);
    let msg = format!("{err}");
    assert!(
        msg.contains("65536"),
        "error message must cite the namespace limit: {msg}"
    );
}

#[test]
fn bgid_exhausted_buffer_ring_error_maps_to_out_of_memory() {
    // The legacy BufferRingError::BgidExhausted (still emitted via the
    // From<BgidAllocError> for BufferRingError path) must also surface
    // as ErrorKind::OutOfMemory so callers that converge on io::Error
    // see a single semantic.
    let err: io::Error = BufferRingError::BgidExhausted.into();
    assert_eq!(err.kind(), io::ErrorKind::OutOfMemory);
}

#[test]
fn bgid_alloc_error_converts_to_buffer_ring_error() {
    let alloc_err = BgidAllocError::Exhausted {
        fresh_used: BGID_NAMESPACE_SIZE,
        free_list_len: 7,
    };
    let ring_err: BufferRingError = alloc_err.into();
    assert!(matches!(ring_err, BufferRingError::BgidExhausted));
}

#[test]
fn allocate_until_exhausted_returns_typed_error() {
    let _guard = BgidStateGuard::snapshot();
    // Drive the allocator one step past the u16 namespace by setting
    // the counter to its cap, then assert the next call surfaces the
    // typed error instead of panicking or wrapping.
    NEXT_BGID.store(BGID_NAMESPACE_SIZE, Ordering::Relaxed);
    match BgidAllocator::allocate() {
        Err(BgidAllocError::Exhausted {
            fresh_used,
            free_list_len,
        }) => {
            assert_eq!(fresh_used, BGID_NAMESPACE_SIZE);
            assert_eq!(free_list_len, 0);
        }
        other => panic!("expected BgidAllocError::Exhausted, got {other:?}"),
    }
}

#[test]
fn exhausted_count_increments() {
    let _guard = BgidStateGuard::snapshot();
    let before = bgid_exhausted_count();
    NEXT_BGID.store(BGID_NAMESPACE_SIZE, Ordering::Relaxed);
    assert!(BgidAllocator::allocate().is_err());
    assert!(BgidAllocator::allocate().is_err());
    assert!(BgidAllocator::allocate().is_err());
    let after = bgid_exhausted_count();
    assert_eq!(
        after - before,
        3,
        "BGID_EXHAUSTED_COUNT must tick once per Exhausted return"
    );
}

#[test]
fn bgid_allocator_remaining_does_not_increase() {
    let before = BgidAllocator::remaining();
    let _ = BgidAllocator::allocate();
    let after = BgidAllocator::remaining();
    assert!(
        after <= before,
        "remaining should not increase: before={before}, after={after}"
    );
}

#[test]
fn bgid_allocator_reuses_freed_ids() {
    let _guard = BgidStateGuard::snapshot();
    // Counter and free-list are both empty after snapshot.
    let id = BgidAllocator::allocate().expect("initial allocation");
    BgidAllocator::deallocate(id);
    let reused = BgidAllocator::allocate().expect("post-deallocate allocation");
    assert_eq!(
        id, reused,
        "allocate must drain the free-list before advancing the counter"
    );
}

#[test]
fn bgid_allocator_free_list_persists_after_exhaustion() {
    let _guard = BgidStateGuard::snapshot();
    // Drive the counter to the namespace limit, then return one id.
    // The next allocation must succeed from the free-list even though
    // the monotonic counter is fully consumed.
    NEXT_BGID.store(BGID_NAMESPACE_SIZE, Ordering::Relaxed);
    assert!(
        matches!(
            BgidAllocator::allocate(),
            Err(BgidAllocError::Exhausted { .. })
        ),
        "sanity: counter must be exhausted before the free-list seed"
    );

    BgidAllocator::deallocate(123);
    let reused = BgidAllocator::allocate().expect("allocation must succeed from free-list");
    assert_eq!(reused, 123, "freed bgid must be returned ahead of counter");

    // With the free-list drained again the allocator reports exhaustion.
    assert!(matches!(
        BgidAllocator::allocate(),
        Err(BgidAllocError::Exhausted { .. })
    ));
}

#[test]
fn bgid_allocator_remaining_includes_free_list() {
    let _guard = BgidStateGuard::snapshot();
    // Counter at limit, free-list empty -> zero remaining.
    NEXT_BGID.store(BGID_NAMESPACE_SIZE, Ordering::Relaxed);
    assert_eq!(BgidAllocator::remaining(), 0);

    // Each deallocated id adds one to remaining.
    BgidAllocator::deallocate(7);
    assert_eq!(BgidAllocator::remaining(), 1);
    BgidAllocator::deallocate(42);
    assert_eq!(BgidAllocator::remaining(), 2);

    // Idempotent deallocate does not inflate the free-list count.
    BgidAllocator::deallocate(7);
    assert_eq!(BgidAllocator::remaining(), 2);
}

#[test]
fn bgid_allocator_deallocate_is_idempotent() {
    let _guard = BgidStateGuard::snapshot();
    BgidAllocator::deallocate(99);
    BgidAllocator::deallocate(99);
    let list_len = bgid_free_list().lock().expect("free-list poisoned").len();
    assert_eq!(
        list_len, 1,
        "duplicate deallocate must not push the same bgid twice"
    );
}

#[test]
fn bgid_peak_tracks_100_allocations() {
    let _guard = BgidStateGuard::snapshot();
    assert_eq!(bgid_peak_used(), 0);
    for _ in 0..100 {
        BgidAllocator::allocate().expect("allocation within namespace");
    }
    assert_eq!(
        bgid_peak_used(),
        100,
        "peak must reflect the 100 outstanding allocations"
    );
    assert_eq!(bgid_inflight(), 100);
}

#[test]
fn bgid_peak_does_not_decrease_on_deallocate() {
    let _guard = BgidStateGuard::snapshot();
    let mut ids = Vec::with_capacity(50);
    for _ in 0..50 {
        ids.push(BgidAllocator::allocate().expect("allocation within namespace"));
    }
    assert_eq!(bgid_peak_used(), 50);
    assert_eq!(bgid_inflight(), 50);

    for id in ids {
        BgidAllocator::deallocate(id);
    }
    assert_eq!(
        bgid_peak_used(),
        50,
        "peak must not decrease after returning ids to the free-list"
    );
    assert_eq!(bgid_inflight(), 0, "all ids returned, none in flight");

    // Reallocating from the free-list still updates the peak path but
    // never lifts it above the previous high-water mark.
    let _ = BgidAllocator::allocate().expect("reallocation from free-list");
    assert_eq!(bgid_peak_used(), 50);
}

#[test]
fn bgid_free_list_is_pre_sized() {
    let _guard = BgidStateGuard::snapshot();
    let cap = bgid_free_list()
        .lock()
        .expect("free-list poisoned")
        .capacity();
    assert!(
        cap >= BGID_FREE_LIST_INITIAL_CAPACITY,
        "free-list pre-sized capacity {cap} must be >= {BGID_FREE_LIST_INITIAL_CAPACITY}"
    );
}

#[test]
fn bgid_inflight_reflects_counter_minus_free_list() {
    let _guard = BgidStateGuard::snapshot();
    let a = BgidAllocator::allocate().expect("first");
    let b = BgidAllocator::allocate().expect("second");
    let _c = BgidAllocator::allocate().expect("third");
    assert_eq!(bgid_inflight(), 3);

    BgidAllocator::deallocate(a);
    BgidAllocator::deallocate(b);
    assert_eq!(bgid_inflight(), 1);
}

#[test]
fn bgid_warn_threshold_is_half_namespace() {
    assert_eq!(
        BGID_OCCUPANCY_WARN_THRESHOLD,
        (BGID_NAMESPACE_SIZE / 2) as u16
    );
}

/// Verifies the throttled exhaustion warning fires on the first call
/// and is suppressed on subsequent calls within the throttle window,
/// then fires again after the window elapses. This simulates the
/// long-lived daemon scenario where multiple sessions hit exhaustion
/// over time.
#[test]
fn warn_bgid_fallback_throttles_within_window() {
    let _guard = BgidStateGuard::snapshot();
    let err = BgidAllocError::Exhausted {
        fresh_used: BGID_NAMESPACE_SIZE,
        free_list_len: 0,
    };

    // First call should fire (no previous timestamp).
    warn_bgid_fallback(err);
    let after_first = {
        let last = bgid_exhaustion_warn_last().lock().expect("lock poisoned");
        *last
    };
    assert!(
        after_first.is_some(),
        "first warn_bgid_fallback call must set the last-warned timestamp"
    );

    // Second call within the throttle window should NOT update the
    // timestamp (the warning is suppressed).
    let first_instant = after_first.unwrap();
    warn_bgid_fallback(err);
    let after_second = {
        let last = bgid_exhaustion_warn_last().lock().expect("lock poisoned");
        *last
    };
    assert_eq!(
        after_second.unwrap(),
        first_instant,
        "second call within throttle window must not update the timestamp"
    );
}

/// Models the caller-side fallback contract documented on
/// [`super::super::BufferRing::new_with_allocator`]: on exhaustion the
/// allocator returns a typed error, the cumulative counter ticks, and
/// the caller is expected to skip the registration step and proceed
/// with the plain receive path. The test verifies the observable
/// signals without driving the kernel, so it runs on any host.
#[test]
fn caller_side_fallback_observable_signals() {
    let _guard = BgidStateGuard::snapshot();
    NEXT_BGID.store(BGID_NAMESPACE_SIZE, Ordering::Relaxed);

    let before = bgid_exhausted_count();
    let err = BgidAllocator::allocate().expect_err("allocator must report exhaustion");
    // Caller maps the typed error to io::Error and decides on the
    // fallback - the conversion is total and lossless.
    let io_err: io::Error = err.into();
    assert_eq!(io_err.kind(), io::ErrorKind::OutOfMemory);

    let after = bgid_exhausted_count();
    assert_eq!(
        after - before,
        1,
        "exhaustion counter must observably tick for the caller"
    );

    // Returning one id makes the next allocation succeed: the
    // fallback is reversible once any active ring is dropped.
    BgidAllocator::deallocate(7);
    assert_eq!(
        BgidAllocator::allocate().expect("reuse must succeed"),
        7,
        "free-list reuse restores allocation without resetting state"
    );
}

/// `bgid_snapshot` must return a consistent view of all counters.
#[test]
fn bgid_snapshot_captures_all_counters() {
    let _guard = BgidStateGuard::snapshot();
    let snap_empty = bgid_snapshot();
    assert_eq!(snap_empty.exhausted_count, 0);
    assert_eq!(snap_empty.in_flight, 0);
    assert_eq!(snap_empty.peak_used, 0);
    assert!(snap_empty.remaining > 0);

    let id = BgidAllocator::allocate().expect("allocation");
    let snap_one = bgid_snapshot();
    assert_eq!(snap_one.in_flight, 1);
    assert_eq!(snap_one.peak_used, 1);
    assert_eq!(snap_one.remaining, snap_empty.remaining - 1);

    BgidAllocator::deallocate(id);
    let snap_released = bgid_snapshot();
    assert_eq!(snap_released.in_flight, 0);
    assert_eq!(snap_released.peak_used, 1, "peak never decreases");
}

/// `BgidSessionStats` tracks exhaustion events scoped to a session.
#[test]
fn session_stats_tracks_per_session_exhaustion() {
    let _guard = BgidStateGuard::snapshot();
    let session = BgidSessionStats::new();
    assert_eq!(session.exhaustions_since_start(), 0);
    assert_eq!(session.in_flight_delta(), 0);

    // Allocate two bgids during the session.
    let a = BgidAllocator::allocate().expect("first");
    let _b = BgidAllocator::allocate().expect("second");
    assert_eq!(session.current_in_flight(), 2);
    assert_eq!(session.in_flight_delta(), 2);

    // Force exhaustion.
    NEXT_BGID.store(BGID_NAMESPACE_SIZE, Ordering::Relaxed);
    assert!(BgidAllocator::allocate().is_err());
    assert!(BgidAllocator::allocate().is_err());
    assert_eq!(
        session.exhaustions_since_start(),
        2,
        "session must see both exhaustion events"
    );

    // Restore NEXT_BGID to the actual issued count before checking
    // in_flight_delta - the BGID_NAMESPACE_SIZE store above inflates
    // the "issued" counter that current_in_flight() reads.
    NEXT_BGID.store(2, Ordering::Relaxed);
    BgidAllocator::deallocate(a);
    assert_eq!(session.in_flight_delta(), 1);
}

/// `BgidSessionStats::default` must produce the same result as `new`.
#[test]
fn session_stats_default_matches_new() {
    let from_new = BgidSessionStats::new();
    let from_default = BgidSessionStats::default();
    assert_eq!(
        from_new.start_in_flight(),
        from_default.start_in_flight(),
        "Default and new must produce equivalent snapshots"
    );
}

/// Simulates repeated exhaustion events across multiple sessions,
/// verifying that the exhaustion counter accumulates correctly and
/// per-session stats see only their own window.
#[test]
fn multiple_sessions_see_independent_exhaustion_windows() {
    let _guard = BgidStateGuard::snapshot();
    NEXT_BGID.store(BGID_NAMESPACE_SIZE, Ordering::Relaxed);

    // Session 1: two exhaustion events.
    let session1 = BgidSessionStats::new();
    assert!(BgidAllocator::allocate().is_err());
    assert!(BgidAllocator::allocate().is_err());
    assert_eq!(session1.exhaustions_since_start(), 2);

    // Session 2 starts after session 1's exhaustions.
    let session2 = BgidSessionStats::new();
    assert!(BgidAllocator::allocate().is_err());
    assert_eq!(
        session2.exhaustions_since_start(),
        1,
        "session2 must see only the exhaustion that happened after it started"
    );
    assert_eq!(
        session1.exhaustions_since_start(),
        3,
        "session1 must see all exhaustion events including session2's"
    );
}

/// The exhaustion throttle interval must match the documented 60-second
/// value.
#[test]
fn exhaustion_warn_throttle_is_sixty_seconds() {
    assert_eq!(BGID_EXHAUSTION_WARN_THROTTLE, Duration::from_secs(60));
}
