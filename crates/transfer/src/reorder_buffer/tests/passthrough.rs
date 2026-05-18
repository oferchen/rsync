//! Tests for passthrough (bypass) mode.

use crate::reorder_buffer::BoundedReorderBuffer;

#[test]
fn passthrough_delivers_immediately() {
    let mut buf: BoundedReorderBuffer<&str> = BoundedReorderBuffer::passthrough();
    assert!(buf.is_passthrough());

    let d = buf.insert(5, "hello").unwrap();
    assert_eq!(d, vec!["hello"]);

    let d = buf.insert(0, "world").unwrap();
    assert_eq!(d, vec!["world"]);
}

#[test]
fn passthrough_no_backpressure() {
    let mut buf: BoundedReorderBuffer<u64> = BoundedReorderBuffer::passthrough();
    // Any sequence number is accepted.
    let d = buf.insert(u64::MAX - 1, 999).unwrap();
    assert_eq!(d, vec![999]);
}

#[test]
fn passthrough_no_reordering() {
    let mut buf: BoundedReorderBuffer<&str> = BoundedReorderBuffer::passthrough();

    // Insert out of order: 2, 0, 1.
    let d = buf.insert(2, "c").unwrap();
    assert_eq!(d, vec!["c"]);

    let d = buf.insert(0, "a").unwrap();
    assert_eq!(d, vec!["a"]);

    let d = buf.insert(1, "b").unwrap();
    assert_eq!(d, vec!["b"]);
}

#[test]
fn passthrough_metrics_track_delivery() {
    let mut buf: BoundedReorderBuffer<u64> = BoundedReorderBuffer::passthrough();
    buf.insert(0, 10).unwrap();
    buf.insert(1, 20).unwrap();
    buf.insert(2, 30).unwrap();

    let m = buf.metrics();
    assert_eq!(m.items_delivered, 3);
    assert_eq!(m.current_depth, 0);
    assert_eq!(m.peak_depth, 0);
    assert_eq!(m.stall_count, 0);
    assert_eq!(m.total_stall_nanos, 0);
}

#[test]
fn passthrough_buffered_count_is_zero() {
    let mut buf: BoundedReorderBuffer<u64> = BoundedReorderBuffer::passthrough();
    buf.insert(0, 42).unwrap();
    // Items pass through immediately - nothing buffered.
    assert_eq!(buf.buffered_count(), 0);
    assert!(buf.is_empty());
}

#[test]
fn passthrough_window_size_is_zero() {
    let buf: BoundedReorderBuffer<u64> = BoundedReorderBuffer::passthrough();
    assert_eq!(buf.window_size(), 0);
}

#[test]
fn passthrough_next_expected_stays_zero() {
    let mut buf: BoundedReorderBuffer<u64> = BoundedReorderBuffer::passthrough();
    buf.insert(0, 10).unwrap();
    buf.insert(1, 20).unwrap();
    // In bypass mode, next_expected is not advanced.
    assert_eq!(buf.next_expected(), 0);
}

#[test]
fn passthrough_large_batch() {
    let mut buf: BoundedReorderBuffer<u64> = BoundedReorderBuffer::passthrough();
    let mut all_drained = Vec::new();

    for i in 0..500u64 {
        let d = buf.insert(499 - i, i).unwrap();
        all_drained.extend(d);
    }

    assert_eq!(all_drained.len(), 500);
    // Values arrive in insertion order (0, 1, 2, ..., 499).
    for (i, &val) in all_drained.iter().enumerate() {
        assert_eq!(val, i as u64);
    }
}

#[test]
fn non_passthrough_flag_is_false() {
    let buf: BoundedReorderBuffer<u64> = BoundedReorderBuffer::new(4);
    assert!(!buf.is_passthrough());
}
