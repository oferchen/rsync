//! Baseline correctness: ordering, threshold trip, drains, and force-insert.

use super::super::super::SpillableReorderBuffer;
use super::drain_all;

#[test]
fn no_spill_under_threshold() {
    let mut buf: SpillableReorderBuffer<u64> = SpillableReorderBuffer::new(64, 1024); // 1 KB threshold

    // Insert a few items - well under threshold.
    for i in 0..10 {
        buf.insert(i, i * 10).unwrap();
    }

    let stats = buf.spill_stats();
    assert_eq!(stats.spilled_items, 0);
    assert_eq!(stats.spill_events, 0);
    assert_eq!(stats.memory_used, 80); // 10 * 8 bytes

    let items = drain_all(&mut buf);
    assert_eq!(items.len(), 10);
    for (i, &val) in items.iter().enumerate() {
        assert_eq!(val, i as u64 * 10);
    }
}

#[test]
fn spill_triggers_when_threshold_exceeded() {
    // Threshold of 40 bytes = 5 items of 8 bytes each.
    let mut buf: SpillableReorderBuffer<u64> = SpillableReorderBuffer::new(64, 40);

    // Insert items 5..=15 first (gap at 0..5).
    // After 6 items, memory > 40, should trigger spill.
    for i in (0..16).rev() {
        buf.insert(i, i * 100).unwrap();
    }

    let stats = buf.spill_stats();
    assert!(stats.spill_events > 0, "expected spill events, got 0");

    // Despite spilling, items should drain correctly in order.
    let items = drain_all(&mut buf);
    assert_eq!(items.len(), 16);
    for (i, &val) in items.iter().enumerate() {
        assert_eq!(val, i as u64 * 100, "wrong value at index {i}");
    }
}

#[test]
fn correct_delivery_order_after_spill_and_reload() {
    // Very tight threshold: 16 bytes = 2 items.
    let mut buf: SpillableReorderBuffer<u64> = SpillableReorderBuffer::new(32, 16);

    // Insert out of order.
    buf.insert(5, 50).unwrap();
    buf.insert(3, 30).unwrap();
    buf.insert(7, 70).unwrap();
    buf.insert(1, 10).unwrap();
    buf.insert(6, 60).unwrap();
    buf.insert(4, 40).unwrap();
    buf.insert(2, 20).unwrap();
    buf.insert(0, 0).unwrap();

    let items = drain_all(&mut buf);
    assert_eq!(items.len(), 8);
    let expected: Vec<u64> = (0..8).map(|i| i * 10).collect();
    assert_eq!(items, expected);
}

#[test]
fn cleanup_on_drop() {
    // The SpooledTempFile is cleaned up when the buffer is dropped.
    // We verify no panic occurs on drop.
    let mut buf: SpillableReorderBuffer<u64> = SpillableReorderBuffer::new(32, 16);

    for i in (0..20).rev() {
        buf.insert(i, i).unwrap();
    }

    let stats = buf.spill_stats();
    assert!(stats.spill_events > 0);

    drop(buf); // Should clean up temp file without panic.
}

#[test]
fn interleaved_spill_and_deliver() {
    // Threshold allows 3 items in memory (24 bytes for u64).
    let mut buf: SpillableReorderBuffer<u64> = SpillableReorderBuffer::new(32, 24);

    // Phase 1: Insert 0..4 in reverse, draining as we go.
    buf.insert(3, 30).unwrap();
    buf.insert(2, 20).unwrap();
    buf.insert(1, 10).unwrap();
    buf.insert(0, 0).unwrap();

    let items = drain_all(&mut buf);
    assert_eq!(items, vec![0, 10, 20, 30]);

    // Phase 2: Insert 4..8.
    buf.insert(7, 70).unwrap();
    buf.insert(6, 60).unwrap();
    buf.insert(5, 50).unwrap();
    buf.insert(4, 40).unwrap();

    let items = drain_all(&mut buf);
    assert_eq!(items, vec![40, 50, 60, 70]);

    assert!(buf.is_empty());
}

#[test]
fn exact_threshold_boundary() {
    // Threshold of exactly 40 bytes = 5 items.
    let mut buf: SpillableReorderBuffer<u64> = SpillableReorderBuffer::new(32, 40);

    // Insert exactly 5 items - should NOT spill (40 <= 40 is not > 40).
    for i in 0..5 {
        buf.insert(i, i).unwrap();
    }

    let stats = buf.spill_stats();
    assert_eq!(stats.spill_events, 0, "should not spill at exact threshold");
    assert_eq!(stats.memory_used, 40);

    // 6th item pushes over threshold - should trigger spill.
    buf.insert(5, 5).unwrap();
    let stats = buf.spill_stats();
    assert!(stats.spill_events > 0, "should spill above threshold");

    // All items still deliver correctly.
    let items = drain_all(&mut buf);
    assert_eq!(items, vec![0, 1, 2, 3, 4, 5]);
}

#[test]
fn empty_buffer_operations() {
    let mut buf: SpillableReorderBuffer<u64> = SpillableReorderBuffer::new(8, 1024);

    assert!(buf.is_empty());
    assert_eq!(buf.buffered_count(), 0);
    assert_eq!(buf.next_expected(), 0);
    assert!(buf.next_in_order().unwrap().is_none());
    assert!(drain_all(&mut buf).is_empty());
}

#[test]
fn force_insert_with_spill() {
    let mut buf: SpillableReorderBuffer<u64> = SpillableReorderBuffer::new(4, 24); // 3 items before spill

    buf.force_insert(0, 0).unwrap();
    buf.force_insert(1, 10).unwrap();
    buf.force_insert(2, 20).unwrap();
    buf.force_insert(3, 30).unwrap();
    buf.force_insert(10, 100).unwrap(); // beyond capacity, triggers grow + possibly spill

    // Drain what's available.
    let items = drain_all(&mut buf);
    assert_eq!(items, vec![0, 10, 20, 30]);

    // Items 4-9 are missing, so 10 is not yet deliverable.
    assert!(buf.next_in_order().unwrap().is_none());
}

#[test]
fn spill_stats_tracking() {
    let mut buf: SpillableReorderBuffer<u64> = SpillableReorderBuffer::new(32, 32); // 4 items before spill

    for i in (0..10).rev() {
        buf.insert(i, i).unwrap();
    }

    let stats = buf.spill_stats();
    assert!(stats.spill_events > 0);
    assert_eq!(stats.threshold, 32);

    // Drain all - should trigger reloads.
    let items = drain_all(&mut buf);
    assert_eq!(items.len(), 10);

    let stats = buf.spill_stats();
    assert!(
        stats.reload_events > 0,
        "expected reload events after drain"
    );
    assert_eq!(stats.spilled_items, 0, "no items should remain spilled");
}

#[test]
fn large_scale_spill_and_drain() {
    // 100 items, threshold allows ~10 in memory.
    let mut buf: SpillableReorderBuffer<u64> = SpillableReorderBuffer::new(128, 80);

    // Insert all 100 items in reverse order.
    for i in (0..100).rev() {
        buf.insert(i, i * 7).unwrap();
    }

    let items = drain_all(&mut buf);
    assert_eq!(items.len(), 100);
    for (i, &val) in items.iter().enumerate() {
        assert_eq!(val, i as u64 * 7, "wrong value at position {i}");
    }

    let stats = buf.spill_stats();
    assert!(stats.spill_events > 0);
    assert!(stats.reload_events > 0);
    assert!(buf.is_empty());
}

#[test]
fn spillable_buffer_with_delta_results() {
    use crate::concurrent_delta::types::DeltaResult;

    let mut buf: SpillableReorderBuffer<DeltaResult> = SpillableReorderBuffer::new(32, 200); // ~2 items before spill

    // Insert several results out of order.
    buf.insert(
        2,
        DeltaResult::success(20u32, 2000, 500, 1500).with_sequence(2),
    )
    .unwrap();
    buf.insert(
        0,
        DeltaResult::success(10u32, 1000, 300, 700).with_sequence(0),
    )
    .unwrap();
    buf.insert(
        1,
        DeltaResult::needs_redo(15u32, "mismatch".to_string()).with_sequence(1),
    )
    .unwrap();

    let items = drain_all(&mut buf);
    assert_eq!(items.len(), 3);
    assert_eq!(items[0].ndx().get(), 10);
    assert!(items[0].is_success());
    assert_eq!(items[1].ndx().get(), 15);
    assert!(items[1].needs_retry());
    assert_eq!(items[2].ndx().get(), 20);
    assert!(items[2].is_success());
}

#[test]
fn spill_warned_flag_fires_on_first_spill() {
    // Threshold of 24 bytes = 3 items of 8 bytes each.
    let mut buf: SpillableReorderBuffer<u64> = SpillableReorderBuffer::new(64, 24);

    // Insert 3 items - at threshold, no spill yet.
    for i in 0..3 {
        buf.insert(i, i * 10).unwrap();
    }
    assert!(
        !buf.spill_warned(),
        "warning should not fire before threshold is exceeded"
    );

    // 4th item exceeds threshold - triggers spill and warning.
    buf.insert(3, 30).unwrap();
    assert!(buf.spill_warned(), "warning should fire on first spill");
    let stats = buf.spill_stats();
    assert!(stats.spill_events > 0, "spill must have occurred");
}

#[test]
fn spill_warned_flag_fires_only_once() {
    // Very tight threshold: 16 bytes = 2 items.
    let mut buf: SpillableReorderBuffer<u64> = SpillableReorderBuffer::new(64, 16);

    // Fill past threshold multiple times to trigger multiple spill events.
    for i in (0..20).rev() {
        buf.insert(i, i * 100).unwrap();
    }

    let stats = buf.spill_stats();
    assert!(
        stats.spill_events > 1,
        "need multiple spills for this test, got {}",
        stats.spill_events
    );
    assert!(buf.spill_warned(), "warning flag should be set");

    // Drain and verify the flag stays set.
    let items = drain_all(&mut buf);
    assert_eq!(items.len(), 20);
    assert!(buf.spill_warned(), "flag must remain set after drain");
}

#[test]
fn spill_warned_false_when_no_spill() {
    let mut buf: SpillableReorderBuffer<u64> = SpillableReorderBuffer::new(64, 1024);

    for i in 0..10 {
        buf.insert(i, i).unwrap();
    }

    assert!(
        !buf.spill_warned(),
        "warning should not fire when under threshold"
    );
    assert_eq!(buf.spill_stats().spill_events, 0);
}

#[test]
fn spill_activations_counter_increments_on_each_spill() {
    // ROB-2: spill_activations is the per-call counter that adaptive ring
    // sizing (ROB-7) reads. It must rise once per successful spill_excess
    // call regardless of the on-disk record count produced by that call.

    // PerItem granularity so the test does not need to reason about the
    // whole-batch record-fan-out path. Threshold = 16 bytes = 2 items.
    use super::super::super::SpillGranularity;
    let mut buf: SpillableReorderBuffer<u64> =
        SpillableReorderBuffer::new(64, 16).with_granularity(SpillGranularity::PerItem);

    // First insert past threshold: one activation.
    for i in (0..4).rev() {
        buf.insert(i, i * 10).unwrap();
    }
    let after_first = buf.spill_stats().spill_activations;
    assert!(
        after_first >= 1,
        "expected >= 1 activation after first spill, got {after_first}"
    );

    // Drain to free memory, then refill past threshold again - another
    // activation must register on the second pressure event.
    let drained = drain_all(&mut buf);
    assert_eq!(drained.len(), 4);
    for i in (4..8).rev() {
        buf.insert(i, i * 10).unwrap();
    }
    let after_second = buf.spill_stats().spill_activations;
    assert!(
        after_second > after_first,
        "second pressure event must increment activations: {after_first} -> {after_second}"
    );

    // Third pressure event.
    let _ = drain_all(&mut buf);
    for i in (8..12).rev() {
        buf.insert(i, i * 10).unwrap();
    }
    let after_third = buf.spill_stats().spill_activations;
    assert!(
        after_third > after_second,
        "third pressure event must increment activations: {after_second} -> {after_third}"
    );
}

#[test]
fn spill_warning_fires_once_per_transfer() {
    // ROB-3: the one-shot warning must fire exactly once per buffer lifetime
    // even when many activations occur. We exercise the `spill_warned()`
    // accessor because it is the established convention in this module for
    // verifying warning behaviour (no `tracing-subscriber` dev-dep is wired).
    let mut buf: SpillableReorderBuffer<u64> = SpillableReorderBuffer::new(64, 16);

    // Before any spill: warning has not fired.
    assert!(!buf.spill_warned(), "warning must not fire pre-threshold");

    // Force at least 5 activations by alternating fill / drain cycles. Each
    // cycle climbs past the threshold, triggering one or more on-disk records.
    let mut base: u64 = 0;
    for _ in 0..5 {
        for i in (0..6).rev() {
            buf.insert(base + i, (base + i) * 10).unwrap();
        }
        let drained = drain_all(&mut buf);
        assert_eq!(drained.len(), 6);
        base += 6;
    }

    let stats = buf.spill_stats();
    assert!(
        stats.spill_activations >= 5,
        "expected >= 5 activations across 5 pressure cycles, got {}",
        stats.spill_activations
    );

    // Despite many activations the one-shot flag is still set (it never
    // un-sets) and the warning is documented to have fired exactly once.
    assert!(
        buf.spill_warned(),
        "warning flag must be set after the first activation"
    );
}
