//! Regression coverage for the ROB-2 / ROB-3 reorder-buffer telemetry
//! additions (#3667).
//!
//! Two surfaces are exercised:
//!
//! * **ROB-2 / ROB-3 (per-file applier)** -
//!   [`ParallelDeltaApplier::reorder_saturations`] advances on every
//!   per-file ring saturation; the one-shot warn guard inside
//!   [`ParallelDeltaApplier::note_reorder_saturation`] fires at most once
//!   per applier instance. A tiny per-file ring capacity is constructed,
//!   then chunks are submitted with sequence numbers that overflow the
//!   ring; the saturation counter must advance even though the applier
//!   funnels the underlying
//!   [`engine::concurrent_delta::reorder::CapacityExceeded`] into
//!   [`std::io::Error`] for the caller.
//!
//! * **ROB-2 (pipeline spill activations)** -
//!   [`engine::concurrent_delta::DeltaConsumerStats::spill_activations`]
//!   is plumbed from the underlying
//!   [`engine::concurrent_delta::spill::SpillableReorderBuffer`]'s
//!   granularity-invariant counter. A tight byte threshold drives spill
//!   events; the consumer-side stats snapshot must reflect a non-zero
//!   activation total by the time the pipeline drains.

use std::io::{self, Write};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use engine::concurrent_delta::{
    ConcurrentDeltaConfig, DeltaChunk, DeltaConsumer, DeltaWork, ParallelDeltaApplier, work_queue,
};

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

/// Blocks until the consumer's live telemetry reports that the reorder
/// buffer has spilled, so the caller releases the head-of-line item with
/// the spill precondition already established instead of hoping a sleep
/// outran the pipeline.
///
/// Both counters are polled: whichever one is still wired lets a broken
/// plumbing path fail fast on the assertion that names it, rather than
/// stalling until the deadline.
///
/// # Panics
///
/// Panics once `SPILL_WAIT` elapses with neither counter advancing - with
/// the head withheld the buffer's byte usage only grows, so a timeout means
/// the telemetry never reached [`DeltaConsumer::stats`].
fn await_spill(consumer: &DeltaConsumer) {
    const SPILL_WAIT: Duration = Duration::from_secs(30);
    const POLL_INTERVAL: Duration = Duration::from_millis(1);

    let deadline = Instant::now() + SPILL_WAIT;
    loop {
        let stats = consumer.stats();
        if stats.spill_events > 0 || stats.spill_activations > 0 {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "no spill observed within {SPILL_WAIT:?} while the head-of-line item \
             was withheld against a tight byte threshold"
        );
        std::thread::sleep(POLL_INTERVAL);
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

/// Drives a [`DeltaConsumer`] backed by a spillable reorder buffer with a
/// withheld head-of-line item and a tight 1 KiB byte threshold, mirroring
/// the pattern in the consumer-side spill regression
/// (`spillable_consumer_preserves_order_under_pressure`). The new
/// [`engine::concurrent_delta::DeltaConsumerStats::spill_activations`]
/// counter must be non-zero by the time the consumer drains, proving that
/// ROB-2's granularity-invariant activation counter is observable through
/// the public consumer-side API.
///
/// The spill precondition is established, not raced: seq 0 is released only
/// after the live [`DeltaConsumer::stats`] snapshot proves the byte budget
/// was already exceeded. Without the head, no contiguous run can drain, so
/// the buffer's byte usage only grows and the spill is guaranteed rather
/// than timing-dependent.
#[test]
fn delta_consumer_stats_exposes_spill_activations() {
    const COUNT: u32 = 1000;
    const THRESHOLD: u64 = 1024;

    let (tx, rx) = work_queue::bounded_with_capacity(COUNT as usize);
    let cfg = ConcurrentDeltaConfig::with_spill_threshold(THRESHOLD);
    let consumer = DeltaConsumer::spawn_with_config(rx, COUNT as usize, cfg);

    // Sequences 1..COUNT with the head withheld. The queue is bounded at
    // COUNT so these sends never block, and the reorder buffer cannot
    // deliver anything while seq 0 is missing.
    for seq in 1..COUNT {
        let work =
            DeltaWork::whole_file(seq, PathBuf::from("/dst"), 64).with_sequence(u64::from(seq));
        tx.send(work).expect("send out-of-order item");
    }

    await_spill(&consumer);

    tx.send(DeltaWork::whole_file(0u32, PathBuf::from("/dst"), 64).with_sequence(0))
        .expect("send head-of-line item");
    drop(tx);

    let delivered: usize = consumer.iter().count();
    let stats = consumer.stats();
    consumer.join().expect("consumer join");

    assert_eq!(
        delivered, COUNT as usize,
        "every produced item must be delivered"
    );
    assert!(
        stats.spill_events > 0,
        "ROB-2 prerequisite: the spill observed before the head was released \
         must still be visible after the drain; got spill_events={}",
        stats.spill_events
    );
    assert!(
        stats.spill_activations > 0,
        "ROB-2 plumbing must surface at least one spill activation through \
         DeltaConsumerStats; got spill_activations={} (spill_events={})",
        stats.spill_activations,
        stats.spill_events
    );
    // Activations are granularity-invariant: one per `spill_excess` call
    // that wrote at least one record. The PerItem-equivalent `spill_events`
    // counter is necessarily >= `spill_activations` because each call can
    // emit multiple records.
    assert!(
        stats.spill_events >= stats.spill_activations,
        "spill_events ({}) must be >= spill_activations ({}) by definition",
        stats.spill_events,
        stats.spill_activations
    );
}
