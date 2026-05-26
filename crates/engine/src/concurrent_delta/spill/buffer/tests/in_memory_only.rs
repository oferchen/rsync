//! Tests for the `in_memory_only` policy: the buffer returns
//! [`SpillError::SpillDisabled`] when the threshold is exceeded instead
//! of attempting disk I/O.

use super::super::super::SpillableReorderBuffer;

#[test]
fn in_memory_only_returns_spill_disabled_on_threshold_breach() {
    // Threshold of 24 bytes = 3 items of 8 bytes each.
    let mut buf: SpillableReorderBuffer<u64> =
        SpillableReorderBuffer::new(64, 24).with_in_memory_only(true);

    // First 3 items fit within the threshold.
    for i in 0..3 {
        buf.insert(i, i * 10).expect("should succeed under threshold");
    }

    // The 4th item exceeds the threshold and triggers a spill attempt,
    // which must fail with SpillDisabled.
    let err = buf
        .insert(3, 30)
        .expect_err("should fail with SpillDisabled");
    assert!(
        matches!(err, super::super::super::SpillError::SpillDisabled),
        "expected SpillDisabled, got: {err:?}"
    );
}

#[test]
fn in_memory_only_disabled_allows_normal_spill() {
    // Same threshold, but in_memory_only is false (default).
    let mut buf: SpillableReorderBuffer<u64> = SpillableReorderBuffer::new(64, 24);
    assert!(!buf.in_memory_only());

    // Insert enough items to exceed the threshold - should spill to disk.
    for i in (0..16).rev() {
        buf.insert(i, i * 100).unwrap();
    }

    let stats = buf.spill_stats();
    assert!(
        stats.spill_events > 0,
        "expected spill events with default policy"
    );

    // Items should drain correctly in order despite spilling.
    let items = super::drain_all(&mut buf);
    assert_eq!(items.len(), 16);
    for (i, &val) in items.iter().enumerate() {
        assert_eq!(val, i as u64 * 100);
    }
}

#[test]
fn in_memory_only_no_error_when_under_threshold() {
    let mut buf: SpillableReorderBuffer<u64> =
        SpillableReorderBuffer::new(64, 1024).with_in_memory_only(true);

    // All inserts stay under the 1 KB threshold.
    for i in 0..10 {
        buf.insert(i, i).expect("should succeed under threshold");
    }

    let stats = buf.spill_stats();
    assert_eq!(stats.spill_events, 0);
    assert_eq!(stats.memory_used, 80);
}

#[test]
fn in_memory_only_accessor_reflects_builder() {
    let buf: SpillableReorderBuffer<u64> =
        SpillableReorderBuffer::new(64, 1024).with_in_memory_only(true);
    assert!(buf.in_memory_only());

    let buf2: SpillableReorderBuffer<u64> =
        SpillableReorderBuffer::new(64, 1024).with_in_memory_only(false);
    assert!(!buf2.in_memory_only());
}

#[test]
fn in_memory_only_force_insert_returns_spill_disabled() {
    // Threshold of 16 bytes = 2 items of 8 bytes each.
    let mut buf: SpillableReorderBuffer<u64> =
        SpillableReorderBuffer::new(64, 16).with_in_memory_only(true);

    buf.force_insert(0, 0).expect("first insert under threshold");
    buf.force_insert(1, 10).expect("second insert at threshold");

    // Third force_insert exceeds the threshold.
    let err = buf
        .force_insert(2, 20)
        .expect_err("should fail with SpillDisabled");
    assert!(
        matches!(err, super::super::super::SpillError::SpillDisabled),
        "expected SpillDisabled, got: {err:?}"
    );
}
