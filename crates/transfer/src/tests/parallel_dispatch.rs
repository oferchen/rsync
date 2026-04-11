//! Integration tests for the parallel delta dispatch pipeline.
//!
//! Exercises the full `WorkQueue -> rayon dispatch -> ReorderBuffer` pipeline
//! from the `engine::concurrent_delta` module, validating correctness under
//! concurrent execution with 1000 small files (mix of whole-file and delta).
//!
//! # Upstream Reference
//!
//! Upstream rsync processes files sequentially in `receiver.c:recv_files()`.
//! Our concurrent pipeline must produce identical results regardless of worker
//! completion order. These tests verify that invariant.

use std::path::PathBuf;
use std::sync::Mutex;
use std::thread;

use engine::concurrent_delta::reorder::ReorderBuffer;
use engine::concurrent_delta::strategy::dispatch;
use engine::concurrent_delta::work_queue;
use engine::concurrent_delta::{DeltaResult, DeltaResultStatus, DeltaWork, DeltaWorkKind};

/// Total number of files for the large-batch test.
const LARGE_BATCH_SIZE: u32 = 1000;

/// Threshold below which sequential processing should be used.
/// Mirrors `PARALLEL_STAT_THRESHOLD` in the generator's batch_stat module.
const SEQUENTIAL_THRESHOLD: usize = 64;

/// Creates a mixed batch of `DeltaWork` items - alternating whole-file and delta.
///
/// Even-indexed items are whole-file transfers, odd-indexed items are delta
/// transfers with a synthetic basis path. Each item gets a monotonically
/// increasing sequence number for reorder verification.
fn create_mixed_batch(count: u32) -> Vec<DeltaWork> {
    (0..count)
        .map(|i| {
            let dest = PathBuf::from(format!("/dest/file_{i}.dat"));
            // Vary file sizes to exercise different code paths.
            let target_size = u64::from(i + 1) * 128;
            let work = if i % 2 == 0 {
                DeltaWork::whole_file(i, dest, target_size)
            } else {
                let basis = PathBuf::from(format!("/basis/file_{i}.dat"));
                DeltaWork::delta(i, dest, basis, target_size)
            };
            work.with_sequence(u64::from(i))
        })
        .collect()
}

/// Full pipeline: 1000 mixed files through bounded queue, parallel dispatch,
/// and reorder buffer. Verifies all results returned, ordering preserved,
/// and statistics correct.
#[test]
fn parallel_dispatch_1000_files_full_pipeline() {
    let batch = create_mixed_batch(LARGE_BATCH_SIZE);
    let (tx, rx) = work_queue::bounded();

    // Producer sends all items through the bounded queue.
    let producer = thread::spawn(move || {
        for work in batch {
            tx.send(work).unwrap();
        }
    });

    // Parallel workers dispatch each item through the strategy pattern.
    let collected = Mutex::new(Vec::with_capacity(LARGE_BATCH_SIZE as usize));
    rayon::scope(|s| {
        for work in rx.into_iter() {
            let collected = &collected;
            s.spawn(move |_| {
                let result = dispatch(&work);
                collected.lock().unwrap().push(result);
            });
        }
    });

    let results = collected.into_inner().unwrap();
    producer.join().unwrap();

    // All 1000 results must be present.
    assert_eq!(results.len(), LARGE_BATCH_SIZE as usize);

    // Feed into reorder buffer and verify sequential delivery.
    let mut reorder: ReorderBuffer<DeltaResult> =
        ReorderBuffer::new(LARGE_BATCH_SIZE as usize);
    for r in results {
        reorder.insert(r.sequence(), r).unwrap();
    }

    let ordered: Vec<DeltaResult> = reorder.drain_ready().collect();
    assert_eq!(ordered.len(), LARGE_BATCH_SIZE as usize);

    // Verify ordering, NDX correlation, and statistics for each result.
    let mut total_literal: u64 = 0;
    let mut total_matched: u64 = 0;
    let mut whole_file_count: u32 = 0;
    let mut delta_count: u32 = 0;

    for (i, result) in ordered.iter().enumerate() {
        let i_u32 = i as u32;
        let i_u64 = i as u64;

        // Sequence must match submission order.
        assert_eq!(
            result.sequence(),
            i_u64,
            "sequence mismatch at position {i}"
        );

        // NDX must correlate with the original work item.
        assert_eq!(result.ndx(), i_u32, "ndx mismatch at position {i}");

        // All results must succeed.
        assert!(result.is_success(), "result at position {i} is not success");

        let expected_size = (i_u32 + 1) as u64 * 128;
        assert_eq!(
            result.bytes_written(),
            expected_size,
            "bytes_written mismatch at position {i}"
        );

        if i_u32 % 2 == 0 {
            // Whole-file: all literal, no matched.
            assert_eq!(result.literal_bytes(), expected_size);
            assert_eq!(result.matched_bytes(), 0);
            whole_file_count += 1;
        } else {
            // Delta: 50/50 split per DeltaTransferStrategy.
            let expected_matched = expected_size / 2;
            let expected_literal = expected_size - expected_matched;
            assert_eq!(result.literal_bytes(), expected_literal);
            assert_eq!(result.matched_bytes(), expected_matched);
            delta_count += 1;
        }

        total_literal += result.literal_bytes();
        total_matched += result.matched_bytes();
    }

    // Verify mix counts.
    assert_eq!(whole_file_count, 500);
    assert_eq!(delta_count, 500);

    // Verify aggregate statistics are non-zero and consistent.
    assert!(total_literal > 0, "total literal bytes should be non-zero");
    assert!(total_matched > 0, "total matched bytes should be non-zero");

    // Whole-file items contribute all literal bytes, delta items split 50/50.
    // Total bytes_written = sum of (i+1)*128 for i in 0..1000
    let total_bytes: u64 = (1..=1000u64).map(|i| i * 128).sum();
    assert_eq!(total_literal + total_matched, total_bytes);
}

/// Verifies `drain_parallel` convenience method produces correct results
/// for 1000 files and that reorder buffer recovers sequential order.
#[test]
fn drain_parallel_1000_files_with_reorder() {
    let batch = create_mixed_batch(LARGE_BATCH_SIZE);
    let (tx, rx) = work_queue::bounded();

    let producer = thread::spawn(move || {
        for work in batch {
            tx.send(work).unwrap();
        }
    });

    let results = rx.drain_parallel(dispatch);
    producer.join().unwrap();

    assert_eq!(results.len(), LARGE_BATCH_SIZE as usize);

    // Reorder and verify.
    let mut reorder: ReorderBuffer<DeltaResult> =
        ReorderBuffer::new(LARGE_BATCH_SIZE as usize);
    for r in results {
        reorder.insert(r.sequence(), r).unwrap();
    }

    let ordered: Vec<DeltaResult> = reorder.drain_ready().collect();
    assert_eq!(ordered.len(), LARGE_BATCH_SIZE as usize);

    for (i, result) in ordered.iter().enumerate() {
        assert_eq!(result.sequence(), i as u64);
        assert_eq!(result.ndx(), i as u32);
        assert!(result.is_success());
    }
}

/// Small batches below the parallel threshold should still produce correct
/// results when processed sequentially via `map_blocking` fallback.
#[test]
fn sequential_fallback_below_threshold() {
    let small_count = (SEQUENTIAL_THRESHOLD - 1) as u32;
    let batch = create_mixed_batch(small_count);

    // Use map_blocking with the threshold - should take the sequential path.
    let results = crate::parallel_io::map_blocking(batch, SEQUENTIAL_THRESHOLD, |work| {
        dispatch(&work)
    });

    assert_eq!(results.len(), small_count as usize);

    // map_blocking preserves input order, so results should already be sequential.
    for (i, result) in results.iter().enumerate() {
        assert_eq!(
            result.sequence(),
            i as u64,
            "sequence mismatch at position {i}"
        );
        assert_eq!(result.ndx(), i as u32, "ndx mismatch at position {i}");
        assert!(result.is_success());

        let expected_size = (i as u64 + 1) * 128;
        assert_eq!(result.bytes_written(), expected_size);
    }
}

/// Verifies that exactly-at-threshold triggers parallel dispatch
/// and still produces correct results.
#[test]
fn parallel_dispatch_at_threshold_boundary() {
    let at_threshold = SEQUENTIAL_THRESHOLD as u32;
    let batch = create_mixed_batch(at_threshold);

    // At threshold, map_blocking takes the parallel path.
    let results = crate::parallel_io::map_blocking(batch, SEQUENTIAL_THRESHOLD, |work| {
        dispatch(&work)
    });

    assert_eq!(results.len(), at_threshold as usize);

    // Parallel path via rayon par_iter preserves order.
    for (i, result) in results.iter().enumerate() {
        assert_eq!(result.sequence(), i as u64);
        assert_eq!(result.ndx(), i as u32);
        assert!(result.is_success());
    }
}

/// Verifies that results contain the correct status variants for mixed
/// DeltaResult types (success, redo, failed) through the pipeline.
#[test]
fn pipeline_handles_mixed_result_statuses() {
    let (tx, rx) = work_queue::bounded();
    let count = 100u32;

    let producer = thread::spawn(move || {
        for i in 0..count {
            let work =
                DeltaWork::whole_file(i, PathBuf::from(format!("/dest/{i}")), 256)
                    .with_sequence(u64::from(i));
            tx.send(work).unwrap();
        }
    });

    // Simulate mixed outcomes: most succeed, some redo, some fail.
    let results = rx.drain_parallel(|work| {
        let ndx = work.ndx();
        let seq = work.sequence();
        if ndx % 10 == 5 {
            DeltaResult::needs_redo(ndx, "checksum mismatch".to_string())
                .with_sequence(seq)
        } else if ndx % 10 == 9 {
            DeltaResult::failed(ndx, "I/O error".to_string()).with_sequence(seq)
        } else {
            dispatch(&work)
        }
    });
    producer.join().unwrap();

    assert_eq!(results.len(), count as usize);

    // Reorder and verify status distribution.
    let mut reorder: ReorderBuffer<DeltaResult> = ReorderBuffer::new(count as usize);
    for r in results {
        reorder.insert(r.sequence(), r).unwrap();
    }

    let ordered: Vec<DeltaResult> = reorder.drain_ready().collect();
    assert_eq!(ordered.len(), count as usize);

    let mut success_count = 0u32;
    let mut redo_count = 0u32;
    let mut failed_count = 0u32;

    for (i, result) in ordered.iter().enumerate() {
        let i_u32 = i as u32;
        assert_eq!(result.ndx(), i_u32);
        assert_eq!(result.sequence(), i as u64);

        match result.status() {
            DeltaResultStatus::Success => success_count += 1,
            DeltaResultStatus::NeedsRedo { .. } => {
                assert_eq!(i_u32 % 10, 5);
                redo_count += 1;
            }
            DeltaResultStatus::Failed { .. } => {
                assert_eq!(i_u32 % 10, 9);
                failed_count += 1;
            }
        }
    }

    // 10 redo (5, 15, 25, ..., 95), 10 failed (9, 19, 29, ..., 99), 80 success.
    assert_eq!(redo_count, 10);
    assert_eq!(failed_count, 10);
    assert_eq!(success_count, 80);
}

/// Verifies that work item kind is correctly dispatched for all 1000 items.
#[test]
fn work_kind_dispatch_correctness() {
    let batch = create_mixed_batch(LARGE_BATCH_SIZE);

    // Verify the batch itself has the expected mix.
    let whole_file: Vec<_> = batch
        .iter()
        .filter(|w| w.kind() == DeltaWorkKind::WholeFile)
        .collect();
    let delta: Vec<_> = batch
        .iter()
        .filter(|w| w.kind() == DeltaWorkKind::Delta)
        .collect();

    assert_eq!(whole_file.len(), 500);
    assert_eq!(delta.len(), 500);

    // Verify all whole-file items have no basis path.
    for w in &whole_file {
        assert!(w.basis_path().is_none());
        assert!(!w.is_delta());
    }

    // Verify all delta items have a basis path.
    for w in &delta {
        assert!(w.basis_path().is_some());
        assert!(w.is_delta());
    }
}

/// Empty batch produces no results and does not hang.
#[test]
fn empty_batch_produces_no_results() {
    let (tx, rx) = work_queue::bounded();
    drop(tx); // close immediately
    let results: Vec<DeltaResult> = rx.drain_parallel(dispatch);
    assert!(results.is_empty());
}
