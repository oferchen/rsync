//! [`SpillGranularity`] wiring tests (STN-5 #2339).

use std::io::SeekFrom;

use super::super::super::{SpillCodec, SpillGranularity, SpillableReorderBuffer};
use super::super::HOT_ZONE;
use super::drain_all;

/// Total bytes that ended up on the spill backend, regardless of which
/// flavour (`SpooledTempFile` or `Directory`) was selected.
fn spill_file_size<T: SpillCodec>(buf: &mut SpillableReorderBuffer<T>) -> u64 {
    let backend = buf
        .spill_file
        .as_mut()
        .expect("spill backend must exist for size probe");
    backend.file().seek(SeekFrom::End(0)).expect("seek end")
}

/// Populates `buf` with enough out-of-order items to force the
/// configured spill path to run several spill events. Items are
/// inserted in descending sequence order so the hot-zone filter does
/// not protect them. Each item is 8 bytes apiece (`u64`).
fn force_batch_spill(buf: &mut SpillableReorderBuffer<u64>, min_items: usize) {
    let n = (min_items + HOT_ZONE as usize + 4) as u64;
    for i in (0..n).rev() {
        buf.insert(i, i).expect("insert under capacity");
    }
}

#[test]
fn granularity_whole_batch_writes_single_chunk() {
    // Default granularity packs every candidate selected by a single
    // `spill_excess` call into one length-prefixed record. The on-disk
    // size for that record is therefore `4 + sum(payloads)` with the
    // 4-byte header paid exactly once.
    let mut buf: SpillableReorderBuffer<u64> = SpillableReorderBuffer::new(128, 16);
    assert_eq!(buf.granularity(), SpillGranularity::WholeBatch);

    force_batch_spill(&mut buf, 8);

    // Walk the disk: each whole-batch record is `[u32 len][payload]`.
    // The total file size therefore equals the per-record overhead
    // (4 bytes) times the number of spill events plus the sum of the
    // encoded payloads.
    let stats = buf.spill_stats();
    let spilled = stats.spilled_items as u64;
    let on_disk = spill_file_size(&mut buf);
    let payload_bytes = spilled * 8; // u64 SpillCodec writes 8 bytes per item
    let header_bytes = stats.spill_events * 4;
    assert!(spilled > 0, "test setup must trigger at least one spill");
    assert_eq!(
        on_disk,
        payload_bytes + header_bytes,
        "WholeBatch records must amortise the 4-byte header per spill event \
         (spilled={spilled}, events={}, payload_bytes={payload_bytes}, header_bytes={header_bytes})",
        stats.spill_events
    );
    // At least one event must actually be a multi-item batch, otherwise
    // the optimisation is indistinguishable from per-item.
    assert!(
        spilled > stats.spill_events,
        "expected at least one multi-item batch (spilled={spilled}, events={})",
        stats.spill_events
    );

    // Sanity: items must still drain in order.
    let items = drain_all(&mut buf);
    assert!(!items.is_empty());
    for (i, v) in items.iter().enumerate() {
        assert_eq!(*v, i as u64, "WholeBatch reload must preserve order");
    }
}

#[test]
fn granularity_per_item_writes_n_chunks() {
    // Per-item granularity writes one `[u8 tag][u32 len][payload]`
    // record per spilled item, so the disk footprint includes one
    // 5-byte header (1-byte compression tag + 4-byte length prefix)
    // per item.
    let mut buf: SpillableReorderBuffer<u64> =
        SpillableReorderBuffer::new(128, 16).with_granularity(SpillGranularity::PerItem);
    assert_eq!(buf.granularity(), SpillGranularity::PerItem);

    force_batch_spill(&mut buf, 8);

    let stats = buf.spill_stats();
    let spilled = stats.spilled_items as u64;
    let on_disk = spill_file_size(&mut buf);
    let payload_bytes = spilled * 8;
    let header_bytes = spilled * 5; // one tag byte + one length prefix per item
    assert!(spilled > 0, "test setup must trigger at least one spill");
    assert_eq!(
        on_disk,
        payload_bytes + header_bytes,
        "PerItem records carry one 5-byte header (tag + length) per item \
         (spilled={spilled}, payload_bytes={payload_bytes}, header_bytes={header_bytes})"
    );

    // Drain order is the same contract as the WholeBatch path.
    let items = drain_all(&mut buf);
    assert!(!items.is_empty());
    for (i, v) in items.iter().enumerate() {
        assert_eq!(*v, i as u64, "PerItem reload must preserve order");
    }
}

#[test]
fn granularity_per_item_round_trip_byte_identical() {
    // Encoding and decoding under PerItem granularity round-trips every
    // item back to its original value. This pins the contract that the
    // chosen layout never corrupts payload bytes.
    let mut buf: SpillableReorderBuffer<u64> =
        SpillableReorderBuffer::new(64, 16).with_granularity(SpillGranularity::PerItem);

    let inputs: Vec<u64> = (0..24).map(|i| (i as u64) * 7919).collect();
    for (seq, value) in inputs.iter().enumerate().rev() {
        buf.insert(seq as u64, *value).expect("insert");
    }
    assert!(buf.spill_stats().spill_events > 0);

    let drained = drain_all(&mut buf);
    assert_eq!(drained, inputs, "PerItem round-trip must be byte-identical");
}

#[test]
fn granularity_whole_batch_round_trip_byte_identical() {
    // The default WholeBatch path must also round-trip every payload
    // exactly, even when several items share one packed record.
    let mut buf: SpillableReorderBuffer<u64> = SpillableReorderBuffer::new(64, 16);
    assert_eq!(buf.granularity(), SpillGranularity::WholeBatch);

    let inputs: Vec<u64> = (0..24)
        .map(|i| (i as u64).wrapping_mul(2654435761))
        .collect();
    for (seq, value) in inputs.iter().enumerate().rev() {
        buf.insert(seq as u64, *value).expect("insert");
    }
    assert!(buf.spill_stats().spill_events > 0);

    let drained = drain_all(&mut buf);
    assert_eq!(
        drained, inputs,
        "WholeBatch round-trip must be byte-identical"
    );
}
