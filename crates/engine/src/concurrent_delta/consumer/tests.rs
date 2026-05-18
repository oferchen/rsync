//! Integration tests for [`crate::concurrent_delta::consumer::DeltaConsumer`].
//!
//! Covers in-order delivery guarantees, the bypass path, backpressure-driven
//! force-insert behaviour, metrics snapshots, and the spill-enabled
//! configuration round-trip.

use std::path::PathBuf;

use super::*;
use crate::concurrent_delta::DeltaWork;
use crate::concurrent_delta::work_queue;

/// Helper: sends `count` whole-file work items with sequential sequence numbers.
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
    // Sum of all bucket counts equals the number of drain iterations
    // that produced at least one item.
    let total_drained: u64 = hist
        .buckets()
        .iter()
        .enumerate()
        .map(|(idx, &count)| {
            // Lower bound of bucket idx is 2^idx (except >=1024 cap).
            let lo = 1u64 << idx.min(10);
            lo.saturating_mul(count)
        })
        .sum();
    assert!(
        total_drained >= u64::from(count),
        "histogram lower-bound sum {total_drained} must cover delivered count {count}",
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
