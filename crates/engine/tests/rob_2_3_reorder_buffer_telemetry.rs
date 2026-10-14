//! Regression coverage for the ROB-2 / ROB-3 reorder-buffer telemetry
//! additions on the per-file applier.
//!
//! [`ParallelDeltaApplier::reorder_saturations`] advances on every per-file
//! ring saturation; the one-shot warn guard inside
//! [`ParallelDeltaApplier::note_reorder_saturation`] fires at most once per
//! applier instance. A tiny per-file ring capacity is constructed, then
//! chunks are submitted with sequence numbers that overflow the ring; the
//! saturation counter must advance even though the applier funnels the
//! underlying [`engine::concurrent_delta::reorder::CapacityExceeded`] into
//! [`std::io::Error`] for the caller.

use std::io::{self, Write};

use engine::concurrent_delta::{DeltaChunk, ParallelDeltaApplier};

/// Sink that swallows every write; the test only cares about the reorder
/// buffer's saturation outcome, not the bytes themselves.
struct NullSink;

impl Write for NullSink {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        Ok(data.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Submits an out-of-window chunk against a deliberately tiny per-file
/// reorder ring. The first saturation must increment
/// [`ParallelDeltaApplier::reorder_saturations`] and the second event
/// must also advance the counter monotonically; the warn guard's
/// one-shot semantics are validated by the constant counter delta of
/// exactly one per saturation event.
#[test]
fn per_file_ring_saturation_advances_counter_monotonically() {
    // Capacity 2 means only sequence offsets [0, 2) fit in the ring. Submitting
    // chunk_sequence=5 with the head still missing forces a CapacityExceeded.
    let applier = ParallelDeltaApplier::new(1).with_per_file_reorder_capacity(2);
    applier
        .register_file(0u32, Box::new(NullSink))
        .expect("register_file");

    assert_eq!(
        applier.reorder_saturations(),
        0,
        "fresh applier must report zero saturations"
    );

    // chunk_sequence 5 with capacity 2 sits outside the [0, 2) window so the
    // ring rejects it. The applier maps the error back into an io::Error and
    // updates the ROB-2 counter en route.
    let chunk = DeltaChunk::literal(0u32, 5, vec![0u8; 8]);
    let err = applier
        .apply_one_chunk(chunk)
        .expect_err("oversized sequence must saturate the per-file ring");
    let msg = err.to_string();
    assert!(
        msg.contains("parallel apply reorder full"),
        "saturation error must keep the historical message marker; got: {msg}"
    );

    assert_eq!(
        applier.reorder_saturations(),
        1,
        "first ReorderSaturated must advance the counter exactly once"
    );

    // Second saturation: same applier, different file. Counter must still
    // advance by exactly one - the ROB-3 warn guard is one-shot per applier
    // but the ROB-2 counter is per-event.
    applier
        .register_file(1u32, Box::new(NullSink))
        .expect("register_file second slot");
    let chunk2 = DeltaChunk::literal(1u32, 9, vec![0u8; 8]);
    let _ = applier
        .apply_one_chunk(chunk2)
        .expect_err("second saturation event still reports an error");

    assert_eq!(
        applier.reorder_saturations(),
        2,
        "subsequent saturations must keep advancing reorder_saturations"
    );
}

/// Sanity check that a healthy in-order stream against a tight ring does
/// not advance the saturation counter. A regression that wired the counter
/// to a hot path (e.g. ordinary `insert` calls) would trip this test.
#[test]
fn in_order_submission_never_increments_saturation_counter() {
    let applier = ParallelDeltaApplier::new(1).with_per_file_reorder_capacity(2);
    applier
        .register_file(0u32, Box::new(NullSink))
        .expect("register_file");

    for seq in 0..4u64 {
        let chunk = DeltaChunk::literal(0u32, seq, vec![0u8; 4]);
        applier
            .apply_one_chunk(chunk)
            .expect("in-order submission must succeed");
    }

    assert_eq!(
        applier.reorder_saturations(),
        0,
        "in-order submission must not touch the saturation counter"
    );
}
