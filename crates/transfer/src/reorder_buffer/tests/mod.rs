//! Tests for [`BoundedReorderBuffer`].

mod metrics;
mod passthrough;
mod property;

use super::*;

#[test]
fn in_order_delivery_yields_immediately() {
    let mut buf = BoundedReorderBuffer::new(8);

    let d = buf.insert(0, "a").unwrap();
    assert_eq!(d, vec!["a"]);

    let d = buf.insert(1, "b").unwrap();
    assert_eq!(d, vec!["b"]);

    let d = buf.insert(2, "c").unwrap();
    assert_eq!(d, vec!["c"]);

    assert_eq!(buf.next_expected(), 3);
    assert!(buf.is_empty());
}

#[test]
fn out_of_order_with_gap_fill() {
    let mut buf = BoundedReorderBuffer::new(8);

    let d = buf.insert(2, "c").unwrap();
    assert!(d.is_empty());

    let d = buf.insert(1, "b").unwrap();
    assert!(d.is_empty());

    // Filling the gap at 0 drains 0, 1, 2.
    let d = buf.insert(0, "a").unwrap();
    assert_eq!(d, vec!["a", "b", "c"]);
    assert_eq!(buf.next_expected(), 3);
}

#[test]
fn backpressure_enforcement() {
    let mut buf: BoundedReorderBuffer<i32> = BoundedReorderBuffer::new(4);

    // Insert seq 0 (delivered immediately).
    let d = buf.insert(0, 0).unwrap();
    assert_eq!(d, vec![0]);
    // Window is now [1, 5).

    // seq 5 is outside [1, 5) - backpressure.
    let err = buf.insert(5, 5).unwrap_err();
    assert_eq!(err.sequence, 5);
    assert_eq!(err.window_start, 1);
    assert_eq!(err.window_end, 5);
}

#[test]
fn window_advancement_opens_new_slots() {
    let mut buf = BoundedReorderBuffer::new(4);
    // Window is [0, 4). Insert 0, 1, 2, 3.
    let d = buf.insert(0, 'a').unwrap();
    assert_eq!(d, vec!['a']);
    let d = buf.insert(1, 'b').unwrap();
    assert_eq!(d, vec!['b']);
    let d = buf.insert(2, 'c').unwrap();
    assert_eq!(d, vec!['c']);
    let d = buf.insert(3, 'd').unwrap();
    assert_eq!(d, vec!['d']);
    // All delivered, next_expected = 4, window = [4, 8).

    // seq 7 is within [4, 8).
    let d = buf.insert(7, 'h').unwrap();
    assert!(d.is_empty());

    // seq 8 is outside [4, 8).
    assert!(buf.insert(8, 'i').is_err());

    // Fill 4, 5, 6 to drain through 7.
    let d = buf.insert(4, 'e').unwrap();
    assert_eq!(d, vec!['e']);
    // insert(4) fills the head: drains only 4 (5 is absent), next_expected = 5.

    let d = buf.insert(5, 'f').unwrap();
    assert_eq!(d, vec!['f']);
    // next_expected = 6, 6 not in buffer.

    let d = buf.insert(6, 'g').unwrap();
    assert_eq!(d, vec!['g', 'h']);
    // next_expected was 6, inserts 6, drains 6 then 7 (consecutive).
    assert_eq!(buf.next_expected(), 8);

    // Now seq 8 is within window [8, 12).
    let d = buf.insert(8, 'i').unwrap();
    assert_eq!(d, vec!['i']);
}

#[test]
fn stale_sequence_ignored() {
    let mut buf = BoundedReorderBuffer::new(4);
    buf.insert(0, 10).unwrap();
    buf.insert(1, 20).unwrap();
    // next_expected is now 2.

    // Inserting seq 0 again (already delivered) is silently ignored.
    let d = buf.insert(0, 99).unwrap();
    assert!(d.is_empty());
    assert_eq!(buf.next_expected(), 2);
}

#[test]
fn window_remaining_tracks_capacity() {
    let mut buf = BoundedReorderBuffer::new(4);
    assert_eq!(buf.window_remaining(), 4);

    buf.insert(2, "x").unwrap();
    assert_eq!(buf.window_remaining(), 3);

    buf.insert(3, "y").unwrap();
    assert_eq!(buf.window_remaining(), 2);

    // Head (0) never arrived, so nothing drains; 2 and 3 stay buffered.
    assert_eq!(buf.buffered_count(), 2);
}

#[test]
fn contiguous_drain_amortized() {
    let mut buf = BoundedReorderBuffer::new(64);

    // Insert 1..=50 (all out of order, gap at 0).
    for i in (1..=50).rev() {
        let d = buf.insert(i, i).unwrap();
        assert!(d.is_empty());
    }
    assert_eq!(buf.buffered_count(), 50);

    // Insert 0 - drains all 51 items in one call.
    let d = buf.insert(0, 0).unwrap();
    assert_eq!(d.len(), 51);
    for (i, &val) in d.iter().enumerate() {
        assert_eq!(val, i as u64);
    }
    assert!(buf.is_empty());
    assert_eq!(buf.next_expected(), 51);
}

#[test]
#[should_panic(expected = "window size must be non-zero")]
fn zero_window_panics() {
    let _: BoundedReorderBuffer<i32> = BoundedReorderBuffer::new(0);
}

#[test]
fn backpressure_error_display() {
    let err = BackpressureError {
        sequence: 10,
        window_start: 3,
        window_end: 7,
    };
    assert_eq!(err.to_string(), "sequence 10 outside window [3, 7)");
}

#[test]
fn window_size_accessor() {
    let buf: BoundedReorderBuffer<u8> = BoundedReorderBuffer::new(32);
    assert_eq!(buf.window_size(), 32);
}
