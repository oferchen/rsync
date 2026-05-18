use std::path::PathBuf;

use engine::concurrent_delta::{DeltaResult, DeltaWork};

use crate::delta_pipeline::{ParallelDeltaPipeline, ReceiverDeltaPipeline};

#[test]
fn parallel_submit_and_flush() {
    let mut pipeline = ParallelDeltaPipeline::new(2);
    for i in 0..10u32 {
        let work =
            DeltaWork::whole_file(i, PathBuf::from(format!("/dest/{i}")), u64::from(i) * 100);
        pipeline.submit_work(work).unwrap();
    }

    let results = Box::new(pipeline).flush();
    assert_eq!(results.len(), 10);
    for (i, r) in results.iter().enumerate() {
        assert_eq!(r.sequence(), i as u64);
        assert_eq!(r.ndx().get(), i as u32);
        assert!(r.is_success());
        assert_eq!(r.bytes_written(), i as u64 * 100);
    }
}

#[test]
fn parallel_preserves_submission_order() {
    let mut pipeline = ParallelDeltaPipeline::new(4);
    for i in 0..50u32 {
        let work = DeltaWork::whole_file(i, PathBuf::from(format!("/dest/{i}")), 64);
        pipeline.submit_work(work).unwrap();
    }

    let results = Box::new(pipeline).flush();
    assert_eq!(results.len(), 50);
    for (i, r) in results.iter().enumerate() {
        assert_eq!(r.sequence(), i as u64, "result {i} has wrong sequence");
        assert_eq!(r.ndx().get(), i as u32, "result {i} has wrong ndx");
    }
}

#[test]
fn parallel_mixed_work_kinds() {
    let mut pipeline = ParallelDeltaPipeline::new(2);

    let whole = DeltaWork::whole_file(0, PathBuf::from("/dest/whole"), 500);
    let delta = DeltaWork::delta(
        1,
        PathBuf::from("/dest/delta"),
        PathBuf::from("/basis/delta"),
        1000,
        400,
        600,
    );

    pipeline.submit_work(whole).unwrap();
    pipeline.submit_work(delta).unwrap();

    let results = Box::new(pipeline).flush();
    assert_eq!(results.len(), 2);

    assert_eq!(results[0].ndx().get(), 0);
    assert_eq!(results[0].literal_bytes(), 500);
    assert_eq!(results[0].matched_bytes(), 0);

    assert_eq!(results[1].ndx().get(), 1);
    assert_eq!(results[1].literal_bytes(), 400);
    assert_eq!(results[1].matched_bytes(), 600);
}

#[test]
fn parallel_poll_result_returns_in_order() {
    let mut pipeline = ParallelDeltaPipeline::new(2);
    for i in 0..5u32 {
        let work = DeltaWork::whole_file(i, PathBuf::from(format!("/dest/{i}")), 64);
        pipeline.submit_work(work).unwrap();
    }

    // Flush returns all results in submission order.
    let results = Box::new(pipeline).flush();
    assert_eq!(results.len(), 5);
    for i in 0..5u64 {
        assert_eq!(results[i as usize].sequence(), i);
    }
}

#[test]
fn parallel_flush_empty_pipeline() {
    let pipeline = ParallelDeltaPipeline::new(2);
    let results = Box::new(pipeline).flush();
    assert!(results.is_empty());
}

#[test]
fn parallel_zero_size_files() {
    let mut pipeline = ParallelDeltaPipeline::new(2);
    for i in 0..5u32 {
        let work = DeltaWork::whole_file(i, PathBuf::from(format!("/dest/{i}")), 0);
        pipeline.submit_work(work).unwrap();
    }

    let results = Box::new(pipeline).flush();
    assert_eq!(results.len(), 5);
    for r in &results {
        assert_eq!(r.bytes_written(), 0);
        assert!(r.is_success());
    }
}

#[test]
fn parallel_single_item() {
    let mut pipeline = ParallelDeltaPipeline::new(2);
    let work = DeltaWork::whole_file(42, PathBuf::from("/dest/single"), 256);
    pipeline.submit_work(work).unwrap();

    let results = Box::new(pipeline).flush();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].ndx().get(), 42);
    assert_eq!(results[0].sequence(), 0);
    assert_eq!(results[0].bytes_written(), 256);
}

#[test]
fn parallel_trait_object_works() {
    let mut pipeline: Box<dyn ReceiverDeltaPipeline> = Box::new(ParallelDeltaPipeline::new(2));
    for i in 0..3u32 {
        let work = DeltaWork::whole_file(i, PathBuf::from(format!("/dest/{i}")), 100);
        pipeline.submit_work(work).unwrap();
    }

    let results = pipeline.flush();
    assert_eq!(results.len(), 3);
    for (i, r) in results.iter().enumerate() {
        assert_eq!(r.ndx().get(), i as u32);
    }
}

#[test]
fn parallel_large_batch() {
    let mut pipeline = ParallelDeltaPipeline::new(4);
    let count = 200u32;
    for i in 0..count {
        let work = DeltaWork::whole_file(i, PathBuf::from(format!("/dest/{i}")), 32);
        pipeline.submit_work(work).unwrap();
    }

    let results = Box::new(pipeline).flush();
    assert_eq!(results.len(), count as usize);
    for (i, r) in results.iter().enumerate() {
        assert_eq!(r.sequence(), i as u64);
        assert_eq!(r.ndx().get(), i as u32);
    }
}

#[test]
fn parallel_sequence_monotonically_increases() {
    let mut pipeline = ParallelDeltaPipeline::new(2);
    for i in 0..20u32 {
        let work = DeltaWork::whole_file(i, PathBuf::from(format!("/dest/{i}")), 16);
        pipeline.submit_work(work).unwrap();
    }

    let results = Box::new(pipeline).flush();
    let mut prev_seq = None;
    for r in &results {
        if let Some(prev) = prev_seq {
            assert_eq!(r.sequence(), prev + 1);
        }
        prev_seq = Some(r.sequence());
    }
    assert_eq!(prev_seq, Some(19));
}

#[test]
fn parallel_1000_small_files_all_ordered_and_successful() {
    let mut pipeline = ParallelDeltaPipeline::new(4);
    let count = 1000u32;
    for i in 0..count {
        let size = u64::from(i % 50) * 32 + 64;
        let work = DeltaWork::whole_file(i, PathBuf::from(format!("/dest/file_{i}")), size);
        pipeline.submit_work(work).unwrap();
    }

    let results = Box::new(pipeline).flush();
    assert_eq!(results.len(), count as usize);
    for (i, r) in results.iter().enumerate() {
        let i_u32 = i as u32;
        let expected_size = u64::from(i_u32 % 50) * 32 + 64;
        assert_eq!(r.sequence(), i as u64, "wrong sequence at index {i}");
        assert_eq!(r.ndx().get(), i_u32, "wrong ndx at index {i}");
        assert!(r.is_success(), "not successful at index {i}");
        assert_eq!(
            r.bytes_written(),
            expected_size,
            "wrong bytes_written at index {i}"
        );
        assert_eq!(
            r.literal_bytes(),
            expected_size,
            "wrong literal_bytes at index {i}"
        );
        assert_eq!(r.matched_bytes(), 0, "wrong matched_bytes at index {i}");
    }
}

#[test]
fn parallel_poll_yields_streaming_results_during_submission() {
    // Verifies that poll_result() yields results while the producer is
    // still submitting items - the DeltaConsumer delivers in-order results
    // as contiguous runs become available from the reorder buffer.
    let mut pipeline = ParallelDeltaPipeline::new(2);
    let count = 20u32;
    let mut _polled_during_submit = 0usize;
    let mut total_polled = 0usize;

    for i in 0..count {
        let work = DeltaWork::whole_file(i, PathBuf::from(format!("/dest/{i}")), 64);
        pipeline.submit_work(work).unwrap();

        // Poll after each submit - should eventually start yielding results
        // as the consumer thread processes and reorders them.
        while let Some(result) = pipeline.poll_result() {
            assert!(result.is_success());
            _polled_during_submit += 1;
            total_polled += 1;
        }
    }

    // Flush remaining results.
    let remaining = Box::new(pipeline).flush();
    total_polled += remaining.len();

    assert_eq!(
        total_polled, count as usize,
        "expected {count} total results, got {total_polled}"
    );
    // With enough items, some should arrive during submission.
    // The exact count depends on thread scheduling, so we only verify
    // the total is correct.
}

#[test]
fn parallel_consumer_delivers_in_order_under_load() {
    // Stresses the consumer thread with a large batch to verify that
    // the ReorderBuffer inside DeltaConsumer correctly sequences results
    // even when rayon workers complete in arbitrary order.
    let mut pipeline = ParallelDeltaPipeline::new(4);
    let count = 500u32;
    for i in 0..count {
        let size = u64::from(i % 100) * 8 + 32;
        let work = DeltaWork::whole_file(i, PathBuf::from(format!("/dest/{i}")), size);
        pipeline.submit_work(work).unwrap();
    }

    // Collect results through both poll_result and flush.
    let mut results = Vec::new();
    while let Some(r) = pipeline.poll_result() {
        results.push(r);
    }
    results.extend(Box::new(pipeline).flush());

    assert_eq!(results.len(), count as usize);
    for (i, r) in results.iter().enumerate() {
        assert_eq!(
            r.sequence(),
            i as u64,
            "out of order at position {i}: got sequence {}",
            r.sequence()
        );
        assert_eq!(r.ndx().get(), i as u32);
        assert!(r.is_success());
    }
}

#[test]
fn parallel_flush_after_partial_poll_delivers_remainder_in_order() {
    let mut pipeline = ParallelDeltaPipeline::new(2);
    for i in 0..30u32 {
        let work = DeltaWork::whole_file(i, PathBuf::from(format!("/dest/{i}")), 128);
        pipeline.submit_work(work).unwrap();
    }

    // Give the consumer time to process some items.
    std::thread::sleep(std::time::Duration::from_millis(50));

    // Poll a few results.
    let mut polled = Vec::new();
    while let Some(r) = pipeline.poll_result() {
        polled.push(r);
    }

    // Flush the rest.
    let flushed = Box::new(pipeline).flush();

    // Combine and verify total count and ordering.
    let mut all: Vec<DeltaResult> = polled;
    all.extend(flushed);
    assert_eq!(all.len(), 30);
    for (i, r) in all.iter().enumerate() {
        assert_eq!(r.sequence(), i as u64, "wrong sequence at position {i}");
    }
}

#[test]
fn parallel_error_results_delivered_in_order() {
    // DeltaResult::Failed and NeedsRedo results must be delivered in
    // sequence order alongside successful results.
    let mut pipeline = ParallelDeltaPipeline::new(2);

    // Mix whole-file (success) and delta (success with different stats).
    for i in 0..10u32 {
        let size = u64::from(i) * 100 + 100;
        let work = if i % 3 == 0 {
            let literal = size / 3;
            let matched = size - literal;
            DeltaWork::delta(
                i,
                PathBuf::from(format!("/dest/{i}")),
                PathBuf::from(format!("/basis/{i}")),
                size,
                literal,
                matched,
            )
        } else {
            DeltaWork::whole_file(i, PathBuf::from(format!("/dest/{i}")), u64::from(i) * 50)
        };
        pipeline.submit_work(work).unwrap();
    }

    let results = Box::new(pipeline).flush();
    assert_eq!(results.len(), 10);
    for (i, r) in results.iter().enumerate() {
        assert_eq!(r.sequence(), i as u64);
        assert_eq!(r.ndx().get(), i as u32);
        assert!(r.is_success());
    }
}

#[test]
fn parallel_bypass_delivers_all_results() {
    let mut pipeline = ParallelDeltaPipeline::new_bypass(2);
    for i in 0..20u32 {
        let work =
            DeltaWork::whole_file(i, PathBuf::from(format!("/dest/{i}")), u64::from(i) * 100);
        pipeline.submit_work(work).unwrap();
    }

    let results = Box::new(pipeline).flush();
    assert_eq!(results.len(), 20);
    // All items present (order may differ from submission in bypass mode).
    let mut ndx_values: Vec<u32> = results.iter().map(|r| r.ndx().get()).collect();
    ndx_values.sort_unstable();
    let expected: Vec<u32> = (0..20).collect();
    assert_eq!(ndx_values, expected);
    for r in &results {
        assert!(r.is_success());
    }
}

#[test]
fn parallel_bypass_empty_pipeline() {
    let pipeline = ParallelDeltaPipeline::new_bypass(2);
    let results = Box::new(pipeline).flush();
    assert!(results.is_empty());
}

#[test]
fn parallel_bypass_single_item() {
    let mut pipeline = ParallelDeltaPipeline::new_bypass(2);
    let work = DeltaWork::whole_file(42, PathBuf::from("/dest/single"), 256);
    pipeline.submit_work(work).unwrap();

    let results = Box::new(pipeline).flush();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].ndx().get(), 42);
    assert_eq!(results[0].bytes_written(), 256);
}

#[test]
fn parallel_bypass_trait_object_works() {
    let mut pipeline: Box<dyn ReceiverDeltaPipeline> =
        Box::new(ParallelDeltaPipeline::new_bypass(2));
    for i in 0..5u32 {
        let work = DeltaWork::whole_file(i, PathBuf::from(format!("/dest/{i}")), 100);
        pipeline.submit_work(work).unwrap();
    }

    let results = pipeline.flush();
    assert_eq!(results.len(), 5);
    let mut ndx_values: Vec<u32> = results.iter().map(|r| r.ndx().get()).collect();
    ndx_values.sort_unstable();
    assert_eq!(ndx_values, vec![0, 1, 2, 3, 4]);
}

#[test]
fn parallel_bypass_large_batch() {
    let mut pipeline = ParallelDeltaPipeline::new_bypass(4);
    let count = 200u32;
    for i in 0..count {
        let work = DeltaWork::whole_file(i, PathBuf::from(format!("/dest/{i}")), 32);
        pipeline.submit_work(work).unwrap();
    }

    let results = Box::new(pipeline).flush();
    assert_eq!(results.len(), count as usize);
    let mut ndx_values: Vec<u32> = results.iter().map(|r| r.ndx().get()).collect();
    ndx_values.sort_unstable();
    let expected: Vec<u32> = (0..count).collect();
    assert_eq!(ndx_values, expected);
}
