//! Tests for stall-duration and queue-depth metrics.

use crate::reorder_buffer::{BoundedReorderBuffer, ClockFn, ReorderBufferStats};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// Builds a deterministic clock that advances by `step` on each call.
///
/// Returns a boxed closure suitable for `BoundedReorderBuffer::with_clock`.
/// The clock starts at `Instant::now()` and increments monotonically.
fn fake_clock(step: Duration) -> ClockFn {
    let base = Instant::now();
    let counter = Arc::new(AtomicU64::new(0));
    Box::new(move || {
        let n = counter.fetch_add(1, Ordering::Relaxed);
        base + step * n as u32
    })
}

#[test]
fn in_order_delivery_no_stall() {
    let mut buf: BoundedReorderBuffer<&str> =
        BoundedReorderBuffer::with_clock(8, fake_clock(Duration::from_millis(10)));

    buf.insert(0, "a").unwrap();
    buf.insert(1, "b").unwrap();
    buf.insert(2, "c").unwrap();
    buf.insert(3, "d").unwrap();

    let m = buf.metrics();
    assert_eq!(
        m.stall_count, 0,
        "in-order delivery must produce zero stalls"
    );
    assert_eq!(m.total_stall_nanos, 0);
    assert_eq!(m.mean_stall_nanos(), 0);
    assert_eq!(m.items_delivered, 4);
    assert_eq!(m.current_depth, 0);
    // Each in-order insert momentarily has depth 1 before draining.
    assert_eq!(m.peak_depth, 1);
}

#[test]
fn out_of_order_produces_stall() {
    let step = Duration::from_millis(100);
    let mut buf: BoundedReorderBuffer<&str> = BoundedReorderBuffer::with_clock(8, fake_clock(step));

    // Insert seq 3 first - stall begins (non-head into empty buffer).
    // Clock call 0: entry check, clock call 1: after drain (no drain).
    buf.insert(3, "d").unwrap();
    buf.insert(1, "b").unwrap();
    buf.insert(2, "c").unwrap();

    let m = buf.metrics();
    assert_eq!(m.stall_count, 1, "one stall episode expected");
    assert_eq!(m.current_depth, 3);
    assert_eq!(m.peak_depth, 3);
    assert_eq!(m.items_delivered, 0);
    assert!(
        m.total_stall_nanos == 0,
        "stall not yet resolved - total should be 0"
    );

    // Insert seq 0 - fills the gap, stall ends, all 4 drain.
    buf.insert(0, "a").unwrap();

    let m = buf.metrics();
    assert_eq!(m.stall_count, 1);
    assert_eq!(m.items_delivered, 4);
    assert_eq!(m.current_depth, 0);
    assert_eq!(m.peak_depth, 4);
    assert!(
        m.total_stall_nanos > 0,
        "stall resolved - total_stall_nanos must be positive"
    );
}

#[test]
fn multiple_stall_episodes() {
    let mut buf: BoundedReorderBuffer<u64> =
        BoundedReorderBuffer::with_clock(16, fake_clock(Duration::from_millis(50)));

    // Episode 1: insert seq 2, then 1, then 0.
    buf.insert(2, 2).unwrap();
    buf.insert(1, 1).unwrap();
    buf.insert(0, 0).unwrap();

    let m = buf.metrics();
    assert_eq!(m.stall_count, 1);
    assert_eq!(m.items_delivered, 3);
    assert!(m.total_stall_nanos > 0);

    // Episode 2: insert seq 5, then 4, then 3.
    buf.insert(5, 5).unwrap();
    buf.insert(4, 4).unwrap();
    buf.insert(3, 3).unwrap();

    let m = buf.metrics();
    assert_eq!(m.stall_count, 2, "two distinct stall episodes");
    assert_eq!(m.items_delivered, 6);
}

#[test]
fn peak_depth_monotonically_nondecreasing() {
    let mut buf: BoundedReorderBuffer<u64> = BoundedReorderBuffer::new(64);

    let mut last_peak = 0u64;

    // Insert in reverse order 0..50 to build up depth.
    for i in (0..50).rev() {
        buf.insert(i, i).unwrap();
        let m = buf.metrics();
        assert!(
            m.peak_depth >= last_peak,
            "peak_depth must be monotonically non-decreasing: {} < {}",
            m.peak_depth,
            last_peak
        );
        last_peak = m.peak_depth;
    }

    assert_eq!(last_peak, 50, "peak should reach 50 items");

    // After full drain, peak stays at 50.
    let m = buf.metrics();
    assert_eq!(m.peak_depth, 50);
    assert_eq!(m.current_depth, 0);
}

#[test]
fn queue_depth_tracks_buffered_count() {
    let mut buf: BoundedReorderBuffer<&str> = BoundedReorderBuffer::new(16);

    buf.insert(3, "d").unwrap();
    assert_eq!(buf.metrics().current_depth, 1);

    buf.insert(5, "f").unwrap();
    assert_eq!(buf.metrics().current_depth, 2);

    buf.insert(4, "e").unwrap();
    assert_eq!(buf.metrics().current_depth, 3);

    // Head (0) never arrived, so nothing drains; depth stays at 3.
    assert_eq!(buf.metrics().current_depth, 3);
    assert_eq!(buf.metrics().current_depth, buf.buffered_count() as u64);
}

#[test]
fn items_delivered_counts_all_drained() {
    let mut buf: BoundedReorderBuffer<u64> = BoundedReorderBuffer::new(8);

    for i in 0..5 {
        buf.insert(i, i).unwrap();
    }
    assert_eq!(buf.metrics().items_delivered, 5);

    // Insert more after gap.
    buf.insert(7, 7).unwrap();
    buf.insert(6, 6).unwrap();
    buf.insert(5, 5).unwrap();
    assert_eq!(buf.metrics().items_delivered, 8);
}

#[test]
fn mean_stall_nanos_derived_correctly() {
    let stats = ReorderBufferStats {
        current_depth: 0,
        peak_depth: 5,
        stall_count: 4,
        total_stall_nanos: 1_000_000,
        items_delivered: 10,
    };
    assert_eq!(stats.mean_stall_nanos(), 250_000);
}

#[test]
fn mean_stall_nanos_zero_when_no_stalls() {
    let stats = ReorderBufferStats::default();
    assert_eq!(stats.mean_stall_nanos(), 0);
}

#[test]
fn stale_insert_does_not_affect_metrics() {
    let mut buf: BoundedReorderBuffer<u64> = BoundedReorderBuffer::new(8);
    buf.insert(0, 0).unwrap();
    buf.insert(1, 1).unwrap();

    let m_before = buf.metrics();

    // Stale insert (seq 0 already delivered).
    buf.insert(0, 99).unwrap();

    let m_after = buf.metrics();
    assert_eq!(m_before.stall_count, m_after.stall_count);
    assert_eq!(m_before.peak_depth, m_after.peak_depth);
    assert_eq!(m_before.items_delivered, m_after.items_delivered);
}

/// Property test: monotonic counters across random permutations.
mod prop_metrics {
    use crate::reorder_buffer::BoundedReorderBuffer;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn counters_monotonic_across_permutation(n in 1u64..128) {
            let window = n.max(1);
            let mut buf = BoundedReorderBuffer::new(window);

            let mut indices: Vec<u64> = (0..n).collect();
            indices.reverse();

            let mut prev_stall_count = 0u64;
            let mut prev_peak = 0u64;
            let mut prev_total_stall = 0u64;

            for seq in indices {
                buf.insert(seq, seq).unwrap();
                let m = buf.metrics();

                prop_assert!(
                    m.stall_count >= prev_stall_count,
                    "stall_count must be monotonically non-decreasing"
                );
                prop_assert!(
                    m.peak_depth >= prev_peak,
                    "peak_depth must be monotonically non-decreasing"
                );
                prop_assert!(
                    m.total_stall_nanos >= prev_total_stall,
                    "total_stall_nanos must be monotonically non-decreasing"
                );
                prop_assert!(
                    m.peak_depth >= m.current_depth,
                    "peak_depth must be >= current_depth"
                );

                prev_stall_count = m.stall_count;
                prev_peak = m.peak_depth;
                prev_total_stall = m.total_stall_nanos;
            }

            // After full drain, all items delivered.
            let final_m = buf.metrics();
            prop_assert_eq!(final_m.items_delivered, n);
            prop_assert_eq!(final_m.current_depth, 0);
        }

        #[test]
        fn identity_permutation_zero_stalls(n in 1u64..128) {
            let window = n.max(1);
            let mut buf = BoundedReorderBuffer::new(window);

            for seq in 0..n {
                buf.insert(seq, seq).unwrap();
            }

            let m = buf.metrics();
            prop_assert_eq!(m.stall_count, 0, "identity permutation must have zero stalls");
            prop_assert_eq!(m.total_stall_nanos, 0);
            prop_assert_eq!(m.items_delivered, n);
        }
    }
}
