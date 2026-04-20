//! End-to-end integration test for the WorkQueue -> drain_parallel -> ReorderBuffer pipeline.
//!
//! Verifies that items produced in sequence order, processed in parallel
//! (potentially out of order), and fed through the ReorderBuffer are
//! delivered to the consumer in strict sequence order.

use std::path::PathBuf;
use std::sync::mpsc;
use std::thread;

use engine::concurrent_delta::consumer::DeltaConsumer;
use engine::concurrent_delta::reorder::ReorderBuffer;
use engine::concurrent_delta::work_queue;
use engine::concurrent_delta::{DeltaResult, DeltaWork};

/// Full pipeline: producer -> WorkQueue -> drain_parallel_into -> ReorderBuffer -> ordered output.
///
/// Uses the streaming variant (`drain_parallel_into`) with a separate reorder
/// thread, mirroring the production architecture in `DeltaConsumer`.
#[test]
fn end_to_end_streaming_pipeline_delivers_in_order() {
    const N: u32 = 500;

    let (work_tx, work_rx) = work_queue::bounded_with_capacity(16);

    // Producer: sends N items with monotonically increasing sequence numbers.
    let producer = thread::spawn(move || {
        for i in 0..N {
            let work =
                DeltaWork::whole_file(i, PathBuf::from(format!("/dst/{i}")), u64::from(i) * 10)
                    .with_sequence(u64::from(i));
            work_tx.send(work).unwrap();
        }
    });

    // Stream channel: results arrive out of order from rayon workers.
    let (stream_tx, stream_rx) = mpsc::sync_channel::<DeltaResult>(32);

    // Drain thread: processes items in parallel, streams results as they complete.
    let drain_thread = thread::spawn(move || {
        work_rx.drain_parallel_into(
            |work| {
                // Simulate variable-cost work to induce out-of-order completion.
                let spins = ((work.ndx() * 7 + 13) % 200) as usize;
                let mut acc = 0u64;
                for j in 0..spins {
                    acc = acc.wrapping_add(j as u64);
                }
                let _ = std::hint::black_box(acc);

                DeltaResult::success(
                    work.ndx(),
                    work.target_size(),
                    work.target_size(),
                    0,
                )
                .with_sequence(work.sequence())
            },
            stream_tx,
        );
    });

    // Reorder thread: collects streamed results and delivers in sequence order.
    let (ordered_tx, ordered_rx) = mpsc::channel::<DeltaResult>();
    let reorder_thread = thread::spawn(move || {
        let mut reorder: ReorderBuffer<DeltaResult> = ReorderBuffer::new(N as usize);

        for result in stream_rx {
            match reorder.insert(result.sequence(), result.clone()) {
                Ok(()) => {}
                Err(_) => {
                    // Drain ready items to free capacity, then force insert.
                    for ready in reorder.drain_ready() {
                        ordered_tx.send(ready).unwrap();
                    }
                    reorder.force_insert(result.sequence(), result);
                }
            }
            for ready in reorder.drain_ready() {
                ordered_tx.send(ready).unwrap();
            }
        }

        // Final drain after stream closes.
        for ready in reorder.drain_ready() {
            ordered_tx.send(ready).unwrap();
        }

        reorder.finish();
    });

    // Consume ordered results and verify strict sequence ordering.
    let mut received = Vec::with_capacity(N as usize);
    for result in ordered_rx {
        received.push(result);
    }

    producer.join().unwrap();
    drain_thread.join().unwrap();
    reorder_thread.join().unwrap();

    assert_eq!(received.len(), N as usize, "expected {N} results");
    for (i, r) in received.iter().enumerate() {
        assert_eq!(
            r.sequence(),
            i as u64,
            "sequence mismatch at position {i}: expected {i}, got {}",
            r.sequence()
        );
        assert_eq!(r.ndx(), i as u32);
        assert_eq!(r.bytes_written(), (i as u64) * 10);
        assert!(r.is_success());
    }
}

/// Full pipeline via the `DeltaConsumer` abstraction - the production-ready
/// wiring that combines WorkQueue, drain_parallel, and ReorderBuffer into
/// a single spawn-and-iterate API.
#[test]
fn delta_consumer_end_to_end_ordering_guarantee() {
    const N: u32 = 1000;

    let (work_tx, work_rx) = work_queue::bounded_with_capacity(32);

    let producer = thread::spawn(move || {
        for i in 0..N {
            let work =
                DeltaWork::whole_file(i, PathBuf::from(format!("/dst/{i}")), u64::from(i + 1))
                    .with_sequence(u64::from(i));
            work_tx.send(work).unwrap();
        }
    });

    let consumer = DeltaConsumer::spawn(work_rx, N as usize);
    let results: Vec<DeltaResult> = consumer.into_iter().collect();
    producer.join().unwrap();

    assert_eq!(results.len(), N as usize);
    for (i, r) in results.iter().enumerate() {
        assert_eq!(
            r.sequence(),
            i as u64,
            "out of order at position {i}: expected seq {i}, got {}",
            r.sequence()
        );
        assert_eq!(r.ndx(), i as u32);
        assert!(r.is_success());
    }
}

/// Mixed work kinds (whole-file and delta) produce correctly ordered results
/// with accurate per-item statistics preserved through the pipeline.
#[test]
fn mixed_work_kinds_preserve_stats_in_order() {
    const N: u32 = 200;

    let (work_tx, work_rx) = work_queue::bounded_with_capacity(16);

    let producer = thread::spawn(move || {
        for i in 0..N {
            let work = if i % 3 == 0 {
                DeltaWork::delta(
                    i,
                    PathBuf::from(format!("/dst/{i}")),
                    PathBuf::from(format!("/basis/{i}")),
                    u64::from(i) * 100,
                    u64::from(i) * 40,
                    u64::from(i) * 60,
                )
                .with_sequence(u64::from(i))
            } else {
                DeltaWork::whole_file(i, PathBuf::from(format!("/dst/{i}")), u64::from(i) * 100)
                    .with_sequence(u64::from(i))
            };
            work_tx.send(work).unwrap();
        }
    });

    let consumer = DeltaConsumer::spawn(work_rx, N as usize);
    let results: Vec<DeltaResult> = consumer.into_iter().collect();
    producer.join().unwrap();

    assert_eq!(results.len(), N as usize);
    for (i, r) in results.iter().enumerate() {
        let i_u32 = i as u32;
        let i_u64 = i as u64;

        assert_eq!(r.sequence(), i_u64, "sequence mismatch at {i}");
        assert_eq!(r.ndx(), i_u32);
        assert!(r.is_success());

        if i_u32 % 3 == 0 {
            // Delta items: literal_bytes and matched_bytes from the work item.
            assert_eq!(r.literal_bytes(), i_u64 * 40);
            assert_eq!(r.matched_bytes(), i_u64 * 60);
        } else {
            // Whole-file items: all bytes are literal, zero matched.
            assert_eq!(r.literal_bytes(), i_u64 * 100);
            assert_eq!(r.matched_bytes(), 0);
        }
    }
}

/// Stress test: small reorder capacity forces frequent capacity-exceeded
/// handling, verifying the pipeline remains correct under backpressure.
#[test]
fn small_reorder_capacity_stress() {
    const N: u32 = 300;
    const REORDER_CAP: usize = 8;

    let (work_tx, work_rx) = work_queue::bounded_with_capacity(4);

    let producer = thread::spawn(move || {
        for i in 0..N {
            let work =
                DeltaWork::whole_file(i, PathBuf::from("/dst"), 64).with_sequence(u64::from(i));
            work_tx.send(work).unwrap();
        }
    });

    let consumer = DeltaConsumer::spawn(work_rx, REORDER_CAP);
    let results: Vec<DeltaResult> = consumer.into_iter().collect();
    producer.join().unwrap();

    assert_eq!(results.len(), N as usize);
    for (i, r) in results.iter().enumerate() {
        assert_eq!(
            r.sequence(),
            i as u64,
            "ordering broken at position {i} with small reorder capacity"
        );
    }
}

/// Verifies that `drain_parallel` (batch variant) combined with ReorderBuffer
/// also produces correctly ordered output.
#[test]
fn batch_drain_parallel_with_reorder_buffer() {
    const N: u32 = 250;

    let (work_tx, work_rx) = work_queue::bounded_with_capacity(16);

    let producer = thread::spawn(move || {
        for i in 0..N {
            let work =
                DeltaWork::whole_file(i, PathBuf::from("/dst"), 64).with_sequence(u64::from(i));
            work_tx.send(work).unwrap();
        }
    });

    // drain_parallel collects all results (out of order).
    let results = work_rx.drain_parallel(|work| {
        // Variable spin to induce reordering.
        let spins = ((work.ndx() * 11 + 3) % 150) as usize;
        let mut acc = 0u64;
        for j in 0..spins {
            acc = acc.wrapping_add(j as u64);
        }
        let _ = std::hint::black_box(acc);

        DeltaResult::success(work.ndx(), work.target_size(), work.target_size(), 0)
            .with_sequence(work.sequence())
    });
    producer.join().unwrap();

    assert_eq!(results.len(), N as usize);

    // Feed into ReorderBuffer and extract in order.
    let mut reorder: ReorderBuffer<DeltaResult> = ReorderBuffer::new(N as usize);
    for r in results {
        reorder.insert(r.sequence(), r).unwrap();
    }

    let ordered: Vec<DeltaResult> = reorder.drain_ready().collect();
    assert_eq!(ordered.len(), N as usize);

    for (i, r) in ordered.iter().enumerate() {
        assert_eq!(r.sequence(), i as u64);
        assert_eq!(r.ndx(), i as u32);
    }
}
