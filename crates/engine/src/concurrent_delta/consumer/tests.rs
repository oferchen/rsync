//! Integration tests for [`crate::concurrent_delta::consumer::DeltaConsumer`].
//!
//! Covers in-order delivery guarantees, the bypass path, backpressure-driven
//! force-insert behaviour, metrics snapshots, and the spill-enabled
//! configuration round-trip.

use std::path::PathBuf;

use super::*;
use crate::concurrent_delta::DeltaWork;
use crate::concurrent_delta::work_queue;

/// Helper: creates a bounded work queue sized to hold `count` items.
fn spawn_producer(count: u32) -> (work_queue::WorkQueueSender, work_queue::WorkQueueReceiver) {
    work_queue::bounded_with_capacity(count.max(1) as usize)
}

fn send_items(tx: &work_queue::WorkQueueSender, count: u32) {
    for i in 0..count {
        let work = DeltaWork::whole_file(i, PathBuf::from(format!("/dst/{i}")), 64)
            .with_sequence(u64::from(i));
        tx.send(work).unwrap();
    }
}

#[test]
fn delivers_results_in_sequence_order() {
    let (tx, rx) = spawn_producer(50);
    let producer = std::thread::spawn(move || send_items(&tx, 50));

    let consumer = DeltaConsumer::spawn(rx, 64);
    let results: Vec<DeltaResult> = consumer.iter().collect();
    producer.join().unwrap();

    assert_eq!(results.len(), 50);
    for (i, r) in results.iter().enumerate() {
        assert_eq!(r.sequence(), i as u64, "out of order at position {i}");
        assert!(r.is_success());
    }
}

#[test]
fn into_iter_yields_all_results() {
    let (tx, rx) = spawn_producer(30);
    let producer = std::thread::spawn(move || send_items(&tx, 30));

    let consumer = DeltaConsumer::spawn(rx, 64);
    let results: Vec<DeltaResult> = consumer.into_iter().collect();
    producer.join().unwrap();

    assert_eq!(results.len(), 30);
    for (i, r) in results.iter().enumerate() {
        assert_eq!(r.sequence(), i as u64);
    }
}

#[test]
fn empty_queue_yields_no_results() {
    let (tx, rx) = spawn_producer(1);
    drop(tx); // Close immediately - no items sent.

    let consumer = DeltaConsumer::spawn(rx, 8);
    let results: Vec<DeltaResult> = consumer.iter().collect();

    assert!(results.is_empty());
}

#[test]
fn single_item() {
    let (tx, rx) = spawn_producer(1);
    tx.send(DeltaWork::whole_file(42, PathBuf::from("/dst/single"), 128).with_sequence(0))
        .unwrap();
    drop(tx);

    let consumer = DeltaConsumer::spawn(rx, 4);
    let results: Vec<DeltaResult> = consumer.iter().collect();

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].ndx().get(), 42);
    assert_eq!(results[0].sequence(), 0);
    assert_eq!(results[0].bytes_written(), 128);
}

#[test]
fn join_succeeds_after_drain() {
    let (tx, rx) = spawn_producer(10);
    let producer = std::thread::spawn(move || send_items(&tx, 10));

    let consumer = DeltaConsumer::spawn(rx, 16);
    let results: Vec<DeltaResult> = consumer.iter().collect();
    producer.join().unwrap();

    assert_eq!(results.len(), 10);
}

#[test]
fn join_after_into_iter() {
    let (tx, rx) = spawn_producer(5);
    let producer = std::thread::spawn(move || send_items(&tx, 5));

    let consumer = DeltaConsumer::spawn(rx, 16);
    for r in consumer.iter() {
        assert!(r.is_success());
    }
    consumer.join().unwrap();
    producer.join().unwrap();
}

#[test]
fn large_batch_in_order() {
    let count = 500u32;
    let (tx, rx) = work_queue::bounded_with_capacity(32);

    let producer = std::thread::spawn(move || {
        for i in 0..count {
            let work =
                DeltaWork::whole_file(i, PathBuf::from("/dst"), 64).with_sequence(u64::from(i));
            tx.send(work).unwrap();
        }
    });

    let consumer = DeltaConsumer::spawn(rx, count as usize);
    let results: Vec<DeltaResult> = consumer.into_iter().collect();
    producer.join().unwrap();

    assert_eq!(results.len(), count as usize);
    for (i, r) in results.iter().enumerate() {
        assert_eq!(
            r.sequence(),
            i as u64,
            "sequence mismatch at position {i}: expected {i}, got {}",
            r.sequence()
        );
    }
}

#[test]
fn delta_work_items_processed_correctly() {
    let (tx, rx) = work_queue::bounded_with_capacity(8);

    let producer = std::thread::spawn(move || {
        // Mix of whole-file and delta items.
        tx.send(DeltaWork::whole_file(0, PathBuf::from("/dst/a"), 1024).with_sequence(0))
            .unwrap();
        tx.send(
            DeltaWork::delta(
                1,
                PathBuf::from("/dst/b"),
                PathBuf::from("/basis/b"),
                2048,
                800,
                1248,
            )
            .with_sequence(1),
        )
        .unwrap();
        tx.send(DeltaWork::whole_file(2, PathBuf::from("/dst/c"), 512).with_sequence(2))
            .unwrap();
    });

    let consumer = DeltaConsumer::spawn(rx, 8);
    let results: Vec<DeltaResult> = consumer.into_iter().collect();
    producer.join().unwrap();

    assert_eq!(results.len(), 3);

    // First: whole-file, all literal.
    assert_eq!(results[0].ndx().get(), 0);
    assert_eq!(results[0].literal_bytes(), 1024);
    assert_eq!(results[0].matched_bytes(), 0);

    // Second: delta, mixed literal/matched.
    assert_eq!(results[1].ndx().get(), 1);
    assert_eq!(results[1].literal_bytes(), 800);
    assert_eq!(results[1].matched_bytes(), 1248);

    // Third: whole-file, all literal.
    assert_eq!(results[2].ndx().get(), 2);
    assert_eq!(results[2].literal_bytes(), 512);
    assert_eq!(results[2].matched_bytes(), 0);
}

#[test]
fn small_reorder_capacity_still_delivers_all() {
    // Reorder capacity smaller than total items - the consumer must
    // drain ready items to free capacity before inserting more.
    let count = 20u32;
    let (tx, rx) = work_queue::bounded_with_capacity(4);

    let producer = std::thread::spawn(move || {
        for i in 0..count {
            let work =
                DeltaWork::whole_file(i, PathBuf::from("/dst"), 64).with_sequence(u64::from(i));
            tx.send(work).unwrap();
        }
    });

    let consumer = DeltaConsumer::spawn(rx, 4);
    let results: Vec<DeltaResult> = consumer.into_iter().collect();
    producer.join().unwrap();

    assert_eq!(results.len(), count as usize);
    for (i, r) in results.iter().enumerate() {
        assert_eq!(r.sequence(), i as u64);
    }
}

#[test]
fn drop_consumer_before_drain_does_not_hang() {
    let (tx, rx) = work_queue::bounded_with_capacity(8);

    let producer = std::thread::spawn(move || {
        for i in 0..5u32 {
            let work =
                DeltaWork::whole_file(i, PathBuf::from("/dst"), 64).with_sequence(u64::from(i));
            // Send may fail if consumer is dropped - that's ok.
            let _ = tx.send(work);
        }
    });

    let consumer = DeltaConsumer::spawn(rx, 16);
    drop(consumer);
    producer.join().unwrap();
}

#[test]
fn ndx_values_preserved_through_pipeline() {
    let (tx, rx) = work_queue::bounded_with_capacity(8);

    let producer = std::thread::spawn(move || {
        // Use non-sequential NDX values to verify they survive the pipeline.
        let ndx_values = [100, 42, 7, 999, 0];
        for (seq, &ndx) in ndx_values.iter().enumerate() {
            let work =
                DeltaWork::whole_file(ndx, PathBuf::from("/dst"), 64).with_sequence(seq as u64);
            tx.send(work).unwrap();
        }
    });

    let consumer = DeltaConsumer::spawn(rx, 8);
    let results: Vec<DeltaResult> = consumer.into_iter().collect();
    producer.join().unwrap();

    assert_eq!(results.len(), 5);
    // Results are in sequence order, so NDX values follow submission order.
    assert_eq!(results[0].ndx().get(), 100);
    assert_eq!(results[1].ndx().get(), 42);
    assert_eq!(results[2].ndx().get(), 7);
    assert_eq!(results[3].ndx().get(), 999);
    assert_eq!(results[4].ndx().get(), 0);
}

#[test]
fn try_recv_returns_none_when_no_results_ready() {
    let (tx, rx) = work_queue::bounded_with_capacity(8);
    let consumer = DeltaConsumer::spawn(rx, 16);

    assert!(consumer.try_recv().is_none());

    // Send items so the consumer thread can finish.
    send_items(&tx, 3);
    drop(tx);

    let results: Vec<DeltaResult> = consumer.iter().collect();
    assert_eq!(results.len(), 3);
}

#[test]
fn try_recv_returns_results_when_available() {
    let (tx, rx) = work_queue::bounded_with_capacity(8);

    send_items(&tx, 5);
    drop(tx);

    let consumer = DeltaConsumer::spawn(rx, 16);

    let mut results = Vec::new();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        match consumer.try_recv() {
            Some(r) => results.push(r),
            None => {
                if results.len() == 5 {
                    break;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "timed out waiting for results"
                );
                std::thread::yield_now();
            }
        }
    }

    assert_eq!(results.len(), 5);
    for (i, r) in results.iter().enumerate() {
        assert_eq!(r.sequence(), i as u64);
    }
}

#[test]
fn try_recv_on_empty_queue_returns_none() {
    let (tx, rx) = work_queue::bounded_with_capacity(4);
    drop(tx);

    let consumer = DeltaConsumer::spawn(rx, 8);

    // Give the consumer thread a moment to finish.
    std::thread::sleep(std::time::Duration::from_millis(50));

    assert!(consumer.try_recv().is_none());
    consumer.join().unwrap();
}

#[test]
fn bypass_delivers_all_results() {
    let (tx, rx) = spawn_producer(50);
    let producer = std::thread::spawn(move || send_items(&tx, 50));

    let consumer = DeltaConsumer::spawn_bypass(rx);
    let results: Vec<DeltaResult> = consumer.iter().collect();
    producer.join().unwrap();

    assert_eq!(results.len(), 50);
    // All items delivered - verify by collecting ndx values.
    let mut ndx_values: Vec<u32> = results.iter().map(|r| r.ndx().get()).collect();
    ndx_values.sort_unstable();
    let expected: Vec<u32> = (0..50).collect();
    assert_eq!(ndx_values, expected);
}

#[test]
fn bypass_empty_queue_yields_no_results() {
    let (tx, rx) = spawn_producer(1);
    drop(tx);

    let consumer = DeltaConsumer::spawn_bypass(rx);
    let results: Vec<DeltaResult> = consumer.iter().collect();
    assert!(results.is_empty());
}

#[test]
fn bypass_single_item() {
    let (tx, rx) = spawn_producer(1);
    tx.send(DeltaWork::whole_file(42, PathBuf::from("/dst/single"), 128).with_sequence(0))
        .unwrap();
    drop(tx);

    let consumer = DeltaConsumer::spawn_bypass(rx);
    let results: Vec<DeltaResult> = consumer.iter().collect();

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].ndx().get(), 42);
    assert_eq!(results[0].bytes_written(), 128);
}

#[test]
fn bypass_join_succeeds() {
    let (tx, rx) = spawn_producer(10);
    let producer = std::thread::spawn(move || send_items(&tx, 10));

    let consumer = DeltaConsumer::spawn_bypass(rx);
    let results: Vec<DeltaResult> = consumer.iter().collect();
    producer.join().unwrap();

    assert_eq!(results.len(), 10);
    consumer.join().unwrap();
}

#[test]
fn bypass_large_batch_delivers_all() {
    let count = 500u32;
    let (tx, rx) = work_queue::bounded_with_capacity(32);

    let producer = std::thread::spawn(move || {
        for i in 0..count {
            let work =
                DeltaWork::whole_file(i, PathBuf::from("/dst"), 64).with_sequence(u64::from(i));
            tx.send(work).unwrap();
        }
    });

    let consumer = DeltaConsumer::spawn_bypass(rx);
    let results: Vec<DeltaResult> = consumer.into_iter().collect();
    producer.join().unwrap();

    assert_eq!(results.len(), count as usize);
    // Verify all items present (order may differ from submission).
    let mut ndx_values: Vec<u32> = results.iter().map(|r| r.ndx().get()).collect();
    ndx_values.sort_unstable();
    let expected: Vec<u32> = (0..count).collect();
    assert_eq!(ndx_values, expected);
}

#[test]
fn metrics_snapshot_starts_zeroed() {
    let (_tx, rx) = work_queue::bounded_with_capacity(4);
    let consumer = DeltaConsumer::spawn(rx, 8);
    let m = consumer.metrics();
    assert_eq!(m.force_insert_count, 0);
    assert_eq!(m.drain_batch_size_histogram.total_samples(), 0);
    assert_eq!(m.drain_pause_histogram.total_samples(), 0);
}

/// Verifies the `force_insert` counter increments end-to-end when
/// synthetic backpressure forces the consumer to break its capacity
/// bound. Reproduces the small-capacity HoL pattern from the design
/// doc: a producer that submits sequences out of order leaves the
/// `next_expected` slot empty while later sequences fill the ring.
#[test]
fn metrics_force_insert_counter_increments_under_backpressure() {
    // Capacity 2 with 12 in-flight, where the bounded queue serialises
    // submissions, forces every later result to race past the empty
    // next_expected slot. The producer holds back seq 0 until the rest
    // have queued so the reorder ring overflows.
    let count = 12u32;
    let (tx, rx) = work_queue::bounded_with_capacity(count as usize);
    let producer = std::thread::spawn(move || {
        // Submit sequences 1..count first to fill the rayon pipeline,
        // then submit seq 0 to release the gap. The reorder ring is
        // size 2 so it cannot absorb the late sequences; force_insert
        // fires to keep the pipeline alive.
        for i in 1..count {
            let work =
                DeltaWork::whole_file(i, PathBuf::from("/dst"), 64).with_sequence(u64::from(i));
            tx.send(work).unwrap();
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
        tx.send(DeltaWork::whole_file(0, PathBuf::from("/dst"), 64).with_sequence(0))
            .unwrap();
    });

    let consumer = DeltaConsumer::spawn(rx, 2);
    let results: Vec<DeltaResult> = consumer.iter().collect();
    producer.join().unwrap();
    assert_eq!(results.len(), count as usize);
    let snap = consumer.metrics();
    assert!(
        snap.force_insert_count > 0,
        "expected force_insert_count > 0 under backpressure, got {snap:?}",
    );
    consumer.join().unwrap();
}

/// Regression: the saturated-buffer fallback (`force_insert`) must
/// publish the same cumulative count through the lock-free
/// [`DeltaConsumer::stats`] accessor as it does through the
/// `Mutex`-guarded metrics snapshot, and the consumer must still drain
/// in sequence order so downstream commit logic sees the contracted
/// ordering even when the ordering buffer broke its capacity bound.
///
/// The shape of this test (capacity 2, 16 items, seq 0 sent last) is the
/// canonical reproducer for the ordering-fallback path called out in
/// the `concurrent_delta` design notes - it is the only path that
/// exercises both the deadlock-breaker branch in `run_bare_loop` and
/// the ring growth inside `ReorderBuffer::force_insert`.
#[test]
fn stats_force_inserts_matches_metrics_and_preserves_order() {
    let count = 16u32;
    let (tx, rx) = work_queue::bounded_with_capacity(count as usize);
    let producer = std::thread::spawn(move || {
        for i in 1..count {
            let work =
                DeltaWork::whole_file(i, PathBuf::from("/dst"), 64).with_sequence(u64::from(i));
            tx.send(work).unwrap();
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
        tx.send(DeltaWork::whole_file(0, PathBuf::from("/dst"), 64).with_sequence(0))
            .unwrap();
    });

    let consumer = DeltaConsumer::spawn(rx, 2);
    let results: Vec<DeltaResult> = consumer.iter().collect();
    producer.join().unwrap();

    assert_eq!(results.len(), count as usize);
    for (i, r) in results.iter().enumerate() {
        assert_eq!(
            r.sequence(),
            i as u64,
            "force_insert fallback must still drain in sequence order; \
             out of order at position {i}: got seq {}",
            r.sequence(),
        );
    }

    let stats = consumer.stats();
    let metrics = consumer.metrics();
    assert!(
        stats.force_inserts > 0,
        "expected force_inserts > 0 under backpressure, got stats={stats:?}",
    );
    assert_eq!(
        stats.force_inserts, metrics.force_insert_count,
        "lock-free stats counter must agree with the metrics snapshot \
         (stats={stats:?}, metrics={metrics:?})",
    );
    consumer.join().unwrap();
}

/// Cross-check: when no backpressure occurs the lock-free counter stays
/// at zero, proving the increment is gated on the fallback path and not
/// on every insert.
#[test]
fn stats_force_inserts_zero_without_backpressure() {
    let count = 32u32;
    let (tx, rx) = work_queue::bounded_with_capacity(count as usize);
    let producer = std::thread::spawn(move || send_items(&tx, count));

    let consumer = DeltaConsumer::spawn(rx, count as usize);
    let results: Vec<DeltaResult> = consumer.iter().collect();
    producer.join().unwrap();

    assert_eq!(results.len(), count as usize);
    assert_eq!(
        consumer.stats().force_inserts,
        0,
        "ample reorder capacity must never trigger the fallback",
    );
    consumer.join().unwrap();
}

/// Verifies the drain-batch histogram accumulates buckets when the
/// consumer delivers a contiguous run in a single drain iteration.
#[test]
fn metrics_drain_batch_histogram_accumulates() {
    let count = 16u32;
    let (tx, rx) = spawn_producer(count);
    send_items(&tx, count);
    drop(tx);

    let consumer = DeltaConsumer::spawn(rx, count as usize);
    let results: Vec<DeltaResult> = consumer.iter().collect();
    assert_eq!(results.len(), count as usize);
    let snap = consumer.metrics();
    let hist = snap.drain_batch_size_histogram;
    assert!(
        hist.total_samples() > 0,
        "expected at least one drain-batch sample, got {hist:?}",
    );
    // Bucket idx covers drain sizes in [2^idx, 2^(idx+1) - 1]; the cap
    // bucket (idx >= 10) is unbounded above. Upper-bound sum must cover
    // every delivered item - the previous lower-bound estimate could
    // legitimately underestimate when drains clustered in the top half
    // of a bucket (e.g. six drains of size three give lo=2*6=12 < 16).
    let upper_bound_total: u64 = hist
        .buckets()
        .iter()
        .enumerate()
        .map(|(idx, &count)| {
            let hi = if idx >= 10 {
                u64::MAX
            } else {
                (1u64 << (idx + 1)) - 1
            };
            hi.saturating_mul(count)
        })
        .sum();
    assert!(
        upper_bound_total >= u64::from(count),
        "histogram upper-bound sum {upper_bound_total} must cover delivered count {count}",
    );
    consumer.join().unwrap();
}

// ---- SpillableReorderBuffer wiring tests (task #1884) ----

/// Drives a 1000-item workload through the spill-enabled consumer with
/// a deliberately delayed head-of-line item so the reorder buffer fills
/// up before any contiguous run can be drained. A tight 1 KiB byte
/// budget guarantees the spill machinery engages while delivery remains
/// strictly in submission order.
#[test]
fn spillable_consumer_preserves_order_under_pressure() {
    const COUNT: u32 = 1000;
    // Tight budget vs ~52-byte DeltaResult: ~19 items fit before spill.
    const THRESHOLD: u64 = 1024;

    let (tx, rx) = work_queue::bounded_with_capacity(COUNT as usize);

    // Send sequences 1..COUNT first so the reorder buffer fills with
    // out-of-order items, then send seq 0 last so the head is missing
    // until the very end. Memory pressure exceeds the threshold long
    // before delivery becomes possible, forcing repeated spills.
    let producer = std::thread::spawn(move || {
        for seq in 1..COUNT {
            let work =
                DeltaWork::whole_file(seq, PathBuf::from("/dst"), 64).with_sequence(u64::from(seq));
            tx.send(work).unwrap();
        }
        // Small pause to let the reorder thread build up the buffer
        // before the head-of-line item unblocks the drain.
        std::thread::sleep(std::time::Duration::from_millis(50));
        tx.send(DeltaWork::whole_file(0u32, PathBuf::from("/dst"), 64).with_sequence(0))
            .unwrap();
    });

    let cfg = ConcurrentDeltaConfig::with_spill_threshold(THRESHOLD);
    let consumer = DeltaConsumer::spawn_with_config(rx, COUNT as usize, cfg);
    let results: Vec<DeltaResult> = consumer.iter().collect();
    let stats = consumer.stats();
    producer.join().unwrap();

    assert_eq!(results.len(), COUNT as usize, "all items must be delivered");
    for (i, r) in results.iter().enumerate() {
        assert_eq!(
            r.sequence(),
            i as u64,
            "out of order at position {i}: got seq {}",
            r.sequence()
        );
        assert!(r.is_success(), "result at {i} should be success");
    }
    assert!(
        stats.spill_events > 0,
        "1 KiB budget against 1000 items must trigger spills, got {}",
        stats.spill_events
    );
}

/// Baseline comparison: the spill-enabled and non-spill paths must deliver
/// the same sequence of result payloads byte-for-byte. The spill layer is
/// a local-only memory bound, never a wire-protocol change.
#[test]
fn spillable_consumer_matches_bare_output_byte_for_byte() {
    use crate::concurrent_delta::SpillCodec;
    const COUNT: u32 = 256;

    fn run(cfg: Option<ConcurrentDeltaConfig>) -> Vec<DeltaResult> {
        let (tx, rx) = work_queue::bounded_with_capacity(COUNT as usize);
        let producer = std::thread::spawn(move || {
            for seq in (0..COUNT).rev() {
                let work = DeltaWork::whole_file(seq, PathBuf::from("/dst"), 64)
                    .with_sequence(u64::from(seq));
                tx.send(work).unwrap();
            }
        });
        let consumer = match cfg {
            Some(c) => DeltaConsumer::spawn_with_config(rx, COUNT as usize, c),
            None => DeltaConsumer::spawn(rx, COUNT as usize),
        };
        let out: Vec<DeltaResult> = consumer.iter().collect();
        producer.join().unwrap();
        out
    }

    let baseline = run(None);
    let spilled = run(Some(ConcurrentDeltaConfig::with_spill_threshold(8 * 1024)));

    assert_eq!(baseline.len(), spilled.len(), "result counts must match");
    for (i, (a, b)) in baseline.iter().zip(spilled.iter()).enumerate() {
        assert_eq!(a.sequence(), b.sequence(), "sequence mismatch at {i}");
        assert_eq!(a.ndx().get(), b.ndx().get(), "ndx mismatch at {i}");
        assert_eq!(a.bytes_written(), b.bytes_written(), "bytes_written at {i}");
        assert_eq!(a.literal_bytes(), b.literal_bytes(), "literal at {i}");
        assert_eq!(a.matched_bytes(), b.matched_bytes(), "matched at {i}");
        assert_eq!(a.is_success(), b.is_success(), "status at {i}");

        // SpillCodec round-trips the binary encoding the spill layer uses;
        // identical encodings prove the payloads are byte-equivalent.
        let mut buf_a = Vec::new();
        let mut buf_b = Vec::new();
        a.encode(&mut buf_a).unwrap();
        b.encode(&mut buf_b).unwrap();
        assert_eq!(buf_a, buf_b, "encoded payload differs at {i}");
    }
}

#[test]
fn spawn_with_config_off_matches_spawn() {
    let cfg = ConcurrentDeltaConfig::off();

    let (tx_a, rx_a) = spawn_producer(20);
    let prod_a = std::thread::spawn(move || send_items(&tx_a, 20));
    let baseline = DeltaConsumer::spawn(rx_a, 32).iter().collect::<Vec<_>>();
    prod_a.join().unwrap();

    let (tx_b, rx_b) = spawn_producer(20);
    let prod_b = std::thread::spawn(move || send_items(&tx_b, 20));
    let configured = DeltaConsumer::spawn_with_config(rx_b, 32, cfg)
        .iter()
        .collect::<Vec<_>>();
    prod_b.join().unwrap();

    assert_eq!(baseline.len(), configured.len());
    for (a, b) in baseline.iter().zip(configured.iter()) {
        assert_eq!(a.sequence(), b.sequence());
        assert_eq!(a.ndx().get(), b.ndx().get());
    }
}

#[test]
fn stats_zero_when_spill_disabled() {
    let (tx, rx) = spawn_producer(10);
    let producer = std::thread::spawn(move || send_items(&tx, 10));
    let consumer = DeltaConsumer::spawn(rx, 16);
    let _: Vec<DeltaResult> = consumer.iter().collect();
    producer.join().unwrap();
    assert_eq!(consumer.stats().spill_events, 0);
}

// ---- force_insert coverage: drops, duplicates, payload integrity, ----
// ---- monotonicity, and bare-loop-vs-spillable path isolation.       ----
//
// MEMORY.md flagged the bare-loop `force_insert` as a "smell" because the
// fallback breaks the capacity bound without a test that proves the
// downstream invariants survive it. The metrics-counter regression tests
// above already prove the counter moves; the tests below prove that the
// data stream itself does not silently drop, duplicate, or corrupt items
// when the fallback fires, and that the lock-free `stats` counter behaves
// monotonically across a sustained backpressure window.

/// Drives a much larger backpressure workload (200 items, ring of 4) and
/// asserts (a) every submitted sequence is delivered exactly once and
/// (b) every NDX value survives the force_insert path unchanged. A drop
/// would shrink the delivered set; a duplicate would inflate it. Either
/// would surface here as a concrete bug rather than the "no test, no
/// metric" smell logged in MEMORY.md.
#[test]
fn force_insert_no_drops_no_duplicates_under_sustained_backpressure() {
    use std::collections::BTreeSet;

    const COUNT: u32 = 200;
    const RING: usize = 4;

    let (tx, rx) = work_queue::bounded_with_capacity(COUNT as usize);
    let producer = std::thread::spawn(move || {
        // Submit every sequence > 0 first so the ring saturates while
        // `next_expected = 0` is still missing; then release seq 0 last.
        // Use NDX = sequence + 1000 so a silent NDX swap would be loud.
        for seq in 1..COUNT {
            let work = DeltaWork::whole_file(seq + 1000, PathBuf::from("/dst"), 64)
                .with_sequence(u64::from(seq));
            tx.send(work).unwrap();
        }
        std::thread::sleep(std::time::Duration::from_millis(30));
        tx.send(DeltaWork::whole_file(1000u32, PathBuf::from("/dst"), 64).with_sequence(0))
            .unwrap();
    });

    let consumer = DeltaConsumer::spawn(rx, RING);
    let results: Vec<DeltaResult> = consumer.iter().collect();
    producer.join().unwrap();

    // (1) No drops, no duplicates: exact count of deliveries.
    assert_eq!(
        results.len(),
        COUNT as usize,
        "force_insert must not drop or duplicate items; got {} expected {}",
        results.len(),
        COUNT,
    );

    // (2) Each sequence 0..COUNT appears exactly once.
    let mut seen: BTreeSet<u64> = BTreeSet::new();
    for r in &results {
        assert!(
            seen.insert(r.sequence()),
            "duplicate sequence {} after force_insert fallback",
            r.sequence(),
        );
    }
    for seq in 0..u64::from(COUNT) {
        assert!(
            seen.contains(&seq),
            "missing sequence {seq} after force_insert fallback",
        );
    }

    // (3) NDX-to-sequence correspondence is preserved: ndx == seq + 1000.
    // This catches a swap or partial overwrite inside the ring slot.
    for r in &results {
        assert_eq!(
            u64::from(r.ndx().get()),
            r.sequence() + 1000,
            "NDX/sequence pairing corrupted: ndx={} seq={}",
            r.ndx().get(),
            r.sequence(),
        );
    }

    // (4) The fallback genuinely fired and we are on the bare-loop path
    // (no spill backend wired), so this isolates the smell.
    let stats = consumer.stats();
    assert_eq!(stats.spill_events, 0, "bare loop must not spill");
    assert!(
        stats.force_inserts > 0,
        "expected force_inserts > 0 with ring={RING} vs {COUNT} items, got {stats:?}",
    );
    consumer.join().unwrap();
}

/// Drives a delta-shaped workload through the force_insert fallback and
/// asserts every payload field round-trips unchanged. The original smell
/// is "ordering broken under extreme backpressure"; a silent overwrite of
/// the wrong slot during ring growth would also corrupt the literal/
/// matched/bytes_written triple. Submitting distinct payload values per
/// sequence makes that corruption observable.
#[test]
fn force_insert_preserves_payload_fields() {
    const COUNT: u32 = 24;
    let (tx, rx) = work_queue::bounded_with_capacity(COUNT as usize);

    let producer = std::thread::spawn(move || {
        // Encode `seq` into every field so any cross-slot bleed shows up.
        for seq in 1..COUNT {
            let target = u64::from(seq) * 64;
            let work = DeltaWork::whole_file(seq, PathBuf::from("/dst"), target)
                .with_sequence(u64::from(seq));
            tx.send(work).unwrap();
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
        tx.send(DeltaWork::whole_file(0u32, PathBuf::from("/dst"), 0).with_sequence(0))
            .unwrap();
    });

    let consumer = DeltaConsumer::spawn(rx, 2);
    let results: Vec<DeltaResult> = consumer.iter().collect();
    producer.join().unwrap();

    assert_eq!(results.len(), COUNT as usize);
    for (i, r) in results.iter().enumerate() {
        let seq = i as u64;
        assert_eq!(r.sequence(), seq, "sequence at position {i}");
        assert_eq!(u64::from(r.ndx().get()), seq, "ndx at seq {seq}");
        let expected = seq * 64;
        assert_eq!(r.bytes_written(), expected, "bytes_written at seq {seq}");
        assert_eq!(r.literal_bytes(), expected, "literal_bytes at seq {seq}");
        assert_eq!(r.matched_bytes(), 0, "matched_bytes at seq {seq}");
        assert!(r.is_success(), "status at seq {seq}");
    }

    let stats = consumer.stats();
    assert!(
        stats.force_inserts > 0,
        "fallback must fire with ring=2 vs {COUNT} items, got {stats:?}",
    );
    consumer.join().unwrap();
}

/// Polls [`DeltaConsumer::stats`] while pulling results one at a time and
/// asserts the cumulative `force_inserts` counter never decreases across
/// the run. Monotonicity is the contract operators depend on when they
/// graph the counter; a saturating-add bug, a reset on reload, or a race
/// on the spill path's atomic could silently violate it.
#[test]
fn force_insert_counter_is_monotonic_during_drain() {
    const COUNT: u32 = 64;
    let (tx, rx) = work_queue::bounded_with_capacity(COUNT as usize);

    let producer = std::thread::spawn(move || {
        for seq in 1..COUNT {
            let work =
                DeltaWork::whole_file(seq, PathBuf::from("/dst"), 64).with_sequence(u64::from(seq));
            tx.send(work).unwrap();
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
        tx.send(DeltaWork::whole_file(0u32, PathBuf::from("/dst"), 64).with_sequence(0))
            .unwrap();
    });

    let consumer = DeltaConsumer::spawn(rx, 2);

    let mut prev = consumer.stats().force_inserts;
    let mut peak = prev;
    let mut received = 0u32;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while received < COUNT {
        match consumer.try_recv() {
            Some(_) => {
                received += 1;
                let cur = consumer.stats().force_inserts;
                assert!(
                    cur >= prev,
                    "force_inserts went backwards: {prev} -> {cur} at received={received}",
                );
                prev = cur;
                if cur > peak {
                    peak = cur;
                }
            }
            None => {
                assert!(
                    std::time::Instant::now() < deadline,
                    "timeout draining (received={received}, prev={prev})",
                );
                std::thread::yield_now();
            }
        }
    }
    producer.join().unwrap();

    // Final stats must agree with the peak we observed and with the
    // Mutex-guarded metrics snapshot. A regression that reset the
    // counter on shutdown would trip the first assert; a spill/bare
    // counter desync would trip the second.
    let final_stats = consumer.stats();
    let final_metrics = consumer.metrics();
    assert!(
        final_stats.force_inserts >= peak,
        "final force_inserts {} below observed peak {peak}",
        final_stats.force_inserts,
    );
    assert_eq!(
        final_stats.force_inserts, final_metrics.force_insert_count,
        "stats counter must agree with metrics snapshot at end of run",
    );
    consumer.join().unwrap();
}

/// Pins the bare-loop path: spawning without a spill config exercises
/// `run_bare_loop` in `consumer/loops.rs`, distinct from
/// `run_spillable_loop`. A future refactor that accidentally routes the
/// bare path through the spillable backend (or vice versa) would surface
/// here as a non-zero `spill_events` count when no threshold was set.
#[test]
fn force_insert_isolated_to_bare_loop_path() {
    const COUNT: u32 = 32;
    let (tx, rx) = work_queue::bounded_with_capacity(COUNT as usize);

    let producer = std::thread::spawn(move || {
        for seq in 1..COUNT {
            let work =
                DeltaWork::whole_file(seq, PathBuf::from("/dst"), 64).with_sequence(u64::from(seq));
            tx.send(work).unwrap();
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
        tx.send(DeltaWork::whole_file(0u32, PathBuf::from("/dst"), 64).with_sequence(0))
            .unwrap();
    });

    let consumer = DeltaConsumer::spawn(rx, 2);
    let results: Vec<DeltaResult> = consumer.iter().collect();
    producer.join().unwrap();

    assert_eq!(results.len(), COUNT as usize);
    let stats = consumer.stats();
    assert!(
        stats.force_inserts > 0,
        "expected bare-loop fallback to fire, got {stats:?}",
    );
    assert_eq!(
        stats.spill_events, 0,
        "bare-loop path must not engage spill machinery, got {stats:?}",
    );
    consumer.join().unwrap();
}
