//! In-flight window and metrics forwarding (ROB-1) on the
//! [`SpillableReorderBuffer`] facade.
//!
//! These tests pin that the ROB-1 observability accessors (`in_flight_window`,
//! `metrics`) forwarded from the inner ring track occupancy without altering
//! delivery behaviour, and that the high-water mark is monotonic.

use super::super::super::SpillableReorderBuffer;
use super::drain_all;

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
