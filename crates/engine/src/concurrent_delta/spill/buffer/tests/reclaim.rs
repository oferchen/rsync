//! [`SpillReclaim`] policy tests.

use super::super::super::{SpillCodec, SpillableReorderBuffer, policy};
use super::super::HOT_ZONE;

/// Builds a buffer in a state where the next `next_in_order` call must
/// reload from disk AND memory_used is above threshold. Inserts seed
/// items, then forces additional items into the inner ring at
/// sequences high enough that they stay resident (the hot zone
/// preserves them) while the next_expected sequence is on disk.
fn seed_post_reload_state(buf: &mut SpillableReorderBuffer<u64>) {
    // Phase 1: insert items 0..6 (each 8 bytes) reverse so item 0 lands
    // in memory and items 5,4,... pressure-spill to disk.
    for i in (0..6).rev() {
        buf.insert(i, i * 100).unwrap();
    }
    // Drain the in-memory hot-zone item at next_expected so the next
    // delivery must come from the spill file.
    while let Some(item) = buf.inner.next_in_order() {
        buf.memory_used = buf.memory_used.saturating_sub(item.estimated_size());
        if buf.spill_index.contains_key(&buf.inner.next_expected()) {
            break;
        }
    }
    assert!(
        !buf.spill_index.is_empty(),
        "fixture must leave spilled items pending"
    );
    // Phase 2: force-insert additional items at higher sequences so the
    // in-memory footprint exceeds the threshold without triggering the
    // spill_excess loop. Their sequences are above the hot zone around
    // next_expected, so RespillAfterRead has eligible candidates.
    let next = buf.inner.next_expected();
    for offset in (HOT_ZONE + 1)..(HOT_ZONE + 5) {
        let seq = next + offset;
        buf.inner.force_insert(seq, seq * 100);
        buf.memory_used += 8;
    }
    assert!(
        buf.memory_used > buf.threshold,
        "fixture must leave memory above threshold to exercise RespillAfterRead"
    );
}

#[test]
fn reclaim_default_keeps_in_memory_after_read() {
    // Default reclaim policy: after reload-from-disk delivery, the buffer
    // does not run an extra spill_excess pass. memory_used and the spill
    // event counter remain unchanged across the reload.
    let mut buf: SpillableReorderBuffer<u64> = SpillableReorderBuffer::new(32, 16);
    assert_eq!(buf.reclaim(), policy::SpillReclaim::KeepInMemory);

    seed_post_reload_state(&mut buf);
    let before = buf.spill_stats();
    assert!(before.spill_events > 0, "fixture must spill");
    assert!(
        before.memory_used > buf.threshold(),
        "fixture must leave memory above threshold"
    );

    // Reload-and-deliver one item from disk.
    let reload_seq = buf.next_expected();
    let item = buf
        .next_in_order()
        .unwrap()
        .expect("spilled item must reload");
    assert_eq!(item, reload_seq * 100);

    let after = buf.spill_stats();
    assert!(
        after.reload_events > before.reload_events,
        "reload event counter must advance"
    );
    assert_eq!(
        after.spill_events, before.spill_events,
        "KeepInMemory must not trigger an extra spill_excess pass"
    );
    assert_eq!(
        after.memory_used, before.memory_used,
        "KeepInMemory leaves the in-memory footprint untouched"
    );
}

#[test]
fn reclaim_respill_drops_memory_and_rereads() {
    // RespillAfterRead policy: after each reload-from-disk delivery, the
    // buffer pushes in-memory residue back to disk so memory_used
    // returns to threshold. The spill event counter strictly advances.
    let mut buf: SpillableReorderBuffer<u64> =
        SpillableReorderBuffer::new(32, 16).with_reclaim(policy::SpillReclaim::RespillAfterRead);
    assert_eq!(buf.reclaim(), policy::SpillReclaim::RespillAfterRead);

    seed_post_reload_state(&mut buf);
    let before = buf.spill_stats();
    assert!(before.spill_events > 0, "fixture must spill");
    assert!(
        before.memory_used > buf.threshold(),
        "fixture must leave memory above threshold"
    );
    let spilled_before = before.spilled_items;

    // Reload-and-deliver one item from disk. The post-read reclaim path
    // re-runs spill_excess, so the spill-event counter strictly advances
    // and at least one further in-memory item ends up back on disk.
    let reload_seq = buf.next_expected();
    let item = buf
        .next_in_order()
        .unwrap()
        .expect("spilled item must reload");
    assert_eq!(item, reload_seq * 100);

    let after = buf.spill_stats();
    assert!(
        after.reload_events > before.reload_events,
        "reload event counter must advance"
    );
    assert!(
        after.spill_events > before.spill_events,
        "RespillAfterRead must trigger an extra spill_excess pass"
    );
    assert!(
        after.memory_used <= buf.threshold(),
        "memory must fall back under threshold after re-spill"
    );
    // RespillAfterRead replaces the just-reloaded entry on disk with
    // residue evicted from memory. The post-state must contain at
    // least one disk-resident item that was previously in RAM, proving
    // a re-spill (re-read-able from disk) actually happened.
    assert!(
        after.spilled_items > spilled_before.saturating_sub(1),
        "RespillAfterRead must leave residue spilled to disk"
    );
}
