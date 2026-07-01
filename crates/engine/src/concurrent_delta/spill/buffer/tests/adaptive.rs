//! Opt-in adaptive capacity wiring (ROB-3 / ROB-4) and in-flight window /
//! metrics forwarding (ROB-1) on the [`SpillableReorderBuffer`] facade.
//!
//! These tests pin two invariants:
//!
//! * The adaptive constructor is strictly opt-in - the fixed-capacity
//!   [`SpillableReorderBuffer::new`] path never grows or shrinks, and the
//!   adaptive path grows under sustained head-of-line pressure while still
//!   delivering every item in strict sequence order (no drop, no reorder).
//! * The ROB-1 observability accessors (`in_flight_window`, `metrics`,
//!   `reorder_stats`) forwarded from the inner ring track occupancy without
//!   altering delivery behaviour, and the high-water mark is monotonic.

use super::super::super::super::adaptive::AdaptiveCapacityPolicy;
use super::super::super::SpillableReorderBuffer;
use super::drain_all;

/// Fixed-capacity buffers must report zero adaptive grow / shrink events no
/// matter how much head-of-line pressure builds. A regression that flipped
/// the default to adaptive would trip this immediately.
#[test]
fn default_fixed_capacity_never_grows_or_shrinks() {
    let mut buf: SpillableReorderBuffer<u64> = SpillableReorderBuffer::new(16, 1 << 20);
    let start_capacity = buf.capacity();

    // Fill the ring right up against its fixed bound with a missing head.
    for seq in (1..16).rev() {
        buf.insert(seq, seq * 10).unwrap();
    }

    let stats = buf.reorder_stats();
    assert_eq!(stats.grow_events, 0, "fixed path must not grow");
    assert_eq!(stats.shrink_events, 0, "fixed path must not shrink");
    assert_eq!(
        stats.capacity, start_capacity,
        "fixed capacity must stay constant"
    );

    // Deliver the head and confirm strict in-order drain with no loss.
    buf.insert(0, 0).unwrap();
    let items = drain_all(&mut buf);
    assert_eq!(items.len(), 16);
    for (i, &val) in items.iter().enumerate() {
        assert_eq!(val, i as u64 * 10, "wrong value at index {i}");
    }
    assert!(buf.is_empty());
}

/// The opt-in adaptive path grows the inner ring beyond its `min` start when
/// out-of-order arrivals stretch the gap window, and still delivers every
/// item in sequence order. Growth is bounded by the policy `max`.
#[test]
fn adaptive_path_grows_under_pressure_and_preserves_order() {
    // Start small (min=4), allow growth up to 64. A large byte threshold keeps
    // everything resident so the ring - not the spill path - must absorb the
    // out-of-order fan-out.
    let policy = AdaptiveCapacityPolicy::new(4, 64, 2.0);
    let mut buf: SpillableReorderBuffer<u64> =
        SpillableReorderBuffer::with_adaptive_policy(policy, 1 << 30);

    assert_eq!(buf.capacity(), 4, "adaptive buffer starts at policy.min");

    // Insert sequences 1..40 with the head (0) still missing. The gap window
    // grows far past the initial capacity, forcing the ring to grow.
    for seq in 1..40u64 {
        buf.insert(seq, seq * 7).unwrap();
    }

    let stats = buf.reorder_stats();
    assert!(
        stats.grow_events > 0,
        "adaptive path must record grow events under sustained pressure"
    );
    assert!(
        buf.capacity() > 4 && buf.capacity() <= 64,
        "capacity must grow within [min, max]; got {}",
        buf.capacity()
    );

    // Now deliver the head and confirm strict in-order delivery of all 40
    // items - no drop, no reorder.
    buf.insert(0, 0).unwrap();
    let items = drain_all(&mut buf);
    assert_eq!(items.len(), 40, "every item must be delivered");
    for (i, &val) in items.iter().enumerate() {
        assert_eq!(val, i as u64 * 7, "out-of-order delivery at index {i}");
    }
    assert!(buf.is_empty());
}

/// The adaptive constructor honours the policy `max` bound: an arrival far
/// beyond `max` still surfaces `CapacityExceeded` rather than growing without
/// limit.
#[test]
fn adaptive_path_respects_max_bound() {
    let policy = AdaptiveCapacityPolicy::new(2, 8, 2.0);
    let mut buf: SpillableReorderBuffer<u64> =
        SpillableReorderBuffer::with_adaptive_policy(policy, 1 << 30);

    // Sequence 100 with head 0 missing needs a window of 101 slots, far past
    // the policy max of 8, so the insert must be rejected.
    let err = buf.insert(100, 1).unwrap_err();
    assert!(
        matches!(err, super::super::super::SpillError::Capacity(_)),
        "beyond-max insert must surface a capacity error; got {err:?}"
    );
    assert!(buf.capacity() <= 8, "capacity must not exceed policy.max");
}

/// `in_flight_window` forwards the inner ring's gap window and tracks how far
/// ahead of the delivery cursor buffered items reach - the leading indicator
/// of spill pressure - independent of the buffered item count.
#[test]
fn in_flight_window_tracks_gap_not_count() {
    let mut buf: SpillableReorderBuffer<u64> = SpillableReorderBuffer::new(16, 1 << 20);
    assert_eq!(buf.in_flight_window(), 0, "empty buffer has zero window");

    // A single far-ahead arrival stretches the window to its offset+1 while
    // only one slot is occupied.
    buf.insert(4, 40).unwrap();
    assert_eq!(buf.in_flight_window(), 5, "window spans [0, 4]");
    assert_eq!(buf.buffered_count(), 1, "only one slot occupied");

    // Filling the gap does not widen the window further.
    buf.insert(1, 10).unwrap();
    buf.insert(2, 20).unwrap();
    assert_eq!(buf.in_flight_window(), 5, "window unchanged by infill");

    // Delivering the head shifts the window down toward the cursor.
    buf.insert(0, 0).unwrap();
    assert_eq!(buf.next_in_order().unwrap().unwrap(), 0);
    assert_eq!(buf.in_flight_window(), 4, "window shifts down on delivery");
}

/// `metrics()` forwards the inner ring's diagnostic snapshot: instantaneous
/// depth tracks occupancy and the high-water mark is monotonic across the
/// insert / drain cycle. Forwarding must not perturb delivery order.
#[test]
fn metrics_forwarding_tracks_depth_and_monotonic_high_water() {
    let mut buf: SpillableReorderBuffer<u64> = SpillableReorderBuffer::new(16, 1 << 20);
    assert_eq!(buf.metrics().current_depth, 0);
    assert_eq!(buf.metrics().max_depth, 0);

    for seq in (1..6).rev() {
        buf.insert(seq, seq).unwrap();
    }
    // Five out-of-order items are resident, head (0) still missing.
    assert_eq!(buf.metrics().current_depth, 5);
    let peak = buf.metrics().max_depth;
    assert_eq!(peak, 5, "high-water mark records the peak occupancy");

    // Delivering drops current_depth but must never lower the high-water mark.
    buf.insert(0, 0).unwrap();
    assert_eq!(buf.metrics().current_depth, 6);
    assert_eq!(buf.metrics().max_depth, 6);

    let drained = drain_all(&mut buf);
    assert_eq!(drained.len(), 6);
    assert_eq!(buf.metrics().current_depth, 0, "empty after drain");
    assert!(
        buf.metrics().max_depth >= peak,
        "high-water mark must be monotonic across drains"
    );
    assert_eq!(buf.metrics().max_depth, 6);
}
