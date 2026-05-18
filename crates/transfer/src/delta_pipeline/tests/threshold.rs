use std::path::PathBuf;

use engine::concurrent_delta::DeltaWork;

use crate::delta_pipeline::threshold::ThresholdMode;
use crate::delta_pipeline::{
    DEFAULT_PARALLEL_THRESHOLD, ReceiverDeltaPipeline, ThresholdDeltaPipeline,
};

#[test]
fn threshold_below_threshold_uses_sequential() {
    let threshold = 10;
    let mut pipeline = ThresholdDeltaPipeline::new(threshold);
    for i in 0..5u32 {
        let work = DeltaWork::whole_file(i, PathBuf::from(format!("/dest/{i}")), 100);
        pipeline.submit_work(work).unwrap();
    }

    // While buffering, poll returns None.
    assert!(pipeline.poll_result().is_none());

    // Flush processes sequentially.
    let results = Box::new(pipeline).flush();
    assert_eq!(results.len(), 5);
    for (i, r) in results.iter().enumerate() {
        assert_eq!(r.ndx().get(), i as u32);
        assert!(r.is_success());
    }
}

#[test]
fn threshold_at_threshold_switches_to_parallel() {
    let threshold = 5;
    let mut pipeline = ThresholdDeltaPipeline::new(threshold);
    for i in 0..5u32 {
        let work = DeltaWork::whole_file(i, PathBuf::from(format!("/dest/{i}")), 64);
        pipeline.submit_work(work).unwrap();
    }

    // After reaching threshold, mode should be parallel.
    assert!(matches!(pipeline.mode, ThresholdMode::Parallel(_)));

    let results = Box::new(pipeline).flush();
    assert_eq!(results.len(), 5);
    for (i, r) in results.iter().enumerate() {
        assert_eq!(r.ndx().get(), i as u32);
    }
}

#[test]
fn threshold_above_threshold_continues_parallel() {
    let threshold = 3;
    let mut pipeline = ThresholdDeltaPipeline::new(threshold);
    for i in 0..10u32 {
        let work = DeltaWork::whole_file(i, PathBuf::from(format!("/dest/{i}")), 32);
        pipeline.submit_work(work).unwrap();
    }

    let results = Box::new(pipeline).flush();
    assert_eq!(results.len(), 10);
    for (i, r) in results.iter().enumerate() {
        assert_eq!(r.ndx().get(), i as u32);
        assert_eq!(r.sequence(), i as u64);
    }
}

#[test]
fn threshold_default_threshold_value() {
    let pipeline = ThresholdDeltaPipeline::with_default_threshold();
    assert_eq!(pipeline.threshold, DEFAULT_PARALLEL_THRESHOLD);
    assert_eq!(pipeline.threshold, 64);
}

#[test]
fn threshold_empty_flush() {
    let pipeline = ThresholdDeltaPipeline::new(10);
    let results = Box::new(pipeline).flush();
    assert!(results.is_empty());
}

#[test]
fn threshold_poll_returns_none_while_buffering() {
    let mut pipeline = ThresholdDeltaPipeline::new(100);
    for i in 0..50u32 {
        let work = DeltaWork::whole_file(i, PathBuf::from(format!("/dest/{i}")), 16);
        pipeline.submit_work(work).unwrap();
        assert!(pipeline.poll_result().is_none());
    }
}

#[test]
fn threshold_mixed_work_kinds() {
    let threshold = 3;
    let mut pipeline = ThresholdDeltaPipeline::new(threshold);

    let whole = DeltaWork::whole_file(0, PathBuf::from("/dest/whole"), 500);
    let delta = DeltaWork::delta(
        1,
        PathBuf::from("/dest/delta"),
        PathBuf::from("/basis/delta"),
        1000,
        400,
        600,
    );
    let whole2 = DeltaWork::whole_file(2, PathBuf::from("/dest/whole2"), 200);

    pipeline.submit_work(whole).unwrap();
    pipeline.submit_work(delta).unwrap();
    pipeline.submit_work(whole2).unwrap();

    let results = Box::new(pipeline).flush();
    assert_eq!(results.len(), 3);
    assert_eq!(results[0].literal_bytes(), 500);
    assert_eq!(results[0].matched_bytes(), 0);
    assert_eq!(results[1].literal_bytes(), 400);
    assert_eq!(results[1].matched_bytes(), 600);
    assert_eq!(results[2].literal_bytes(), 200);
}

#[test]
fn threshold_trait_object_works() {
    let mut pipeline: Box<dyn ReceiverDeltaPipeline> = Box::new(ThresholdDeltaPipeline::new(5));
    for i in 0..3u32 {
        let work = DeltaWork::whole_file(i, PathBuf::from(format!("/dest/{i}")), 100);
        pipeline.submit_work(work).unwrap();
    }

    let results = pipeline.flush();
    assert_eq!(results.len(), 3);
}

#[test]
fn threshold_single_item_below_threshold() {
    let mut pipeline = ThresholdDeltaPipeline::new(10);
    let work = DeltaWork::whole_file(7, PathBuf::from("/dest/single"), 128);
    pipeline.submit_work(work).unwrap();

    let results = Box::new(pipeline).flush();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].ndx().get(), 7);
    assert_eq!(results[0].bytes_written(), 128);
}

#[test]
fn threshold_exact_threshold_count() {
    let threshold = 4;
    let mut pipeline = ThresholdDeltaPipeline::new(threshold);

    // Submit exactly threshold items.
    for i in 0..4u32 {
        let work = DeltaWork::whole_file(i, PathBuf::from(format!("/dest/{i}")), 50);
        pipeline.submit_work(work).unwrap();
    }

    assert!(matches!(pipeline.mode, ThresholdMode::Parallel(_)));

    let results = Box::new(pipeline).flush();
    assert_eq!(results.len(), 4);
}

#[test]
fn threshold_one_below_threshold() {
    let threshold = 4;
    let mut pipeline = ThresholdDeltaPipeline::new(threshold);

    // Submit one fewer than threshold.
    for i in 0..3u32 {
        let work = DeltaWork::whole_file(i, PathBuf::from(format!("/dest/{i}")), 50);
        pipeline.submit_work(work).unwrap();
    }

    assert!(matches!(pipeline.mode, ThresholdMode::Buffering(_)));

    let results = Box::new(pipeline).flush();
    assert_eq!(results.len(), 3);
}

#[test]
fn threshold_large_batch_parallel() {
    let threshold = 10;
    let mut pipeline = ThresholdDeltaPipeline::new(threshold);
    let count = 100u32;
    for i in 0..count {
        let work = DeltaWork::whole_file(i, PathBuf::from(format!("/dest/{i}")), 16);
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
fn threshold_preserves_order_in_parallel_mode() {
    let threshold = 2;
    let mut pipeline = ThresholdDeltaPipeline::new(threshold);
    for i in 0..30u32 {
        let work = DeltaWork::whole_file(i, PathBuf::from(format!("/dest/{i}")), 32);
        pipeline.submit_work(work).unwrap();
    }

    let results = Box::new(pipeline).flush();
    assert_eq!(results.len(), 30);
    let mut prev_seq = None;
    for r in &results {
        if let Some(prev) = prev_seq {
            assert_eq!(r.sequence(), prev + 1);
        }
        prev_seq = Some(r.sequence());
    }
}

#[test]
fn threshold_zero_size_files() {
    let mut pipeline = ThresholdDeltaPipeline::new(3);
    for i in 0..3u32 {
        let work = DeltaWork::whole_file(i, PathBuf::from(format!("/dest/{i}")), 0);
        pipeline.submit_work(work).unwrap();
    }

    let results = Box::new(pipeline).flush();
    assert_eq!(results.len(), 3);
    for r in &results {
        assert_eq!(r.bytes_written(), 0);
        assert!(r.is_success());
    }
}

#[test]
fn threshold_sequential_fallback_for_small_transfers() {
    let mut pipeline = ThresholdDeltaPipeline::with_default_threshold();
    let count = 30u32;
    for i in 0..count {
        let work = DeltaWork::whole_file(i, PathBuf::from(format!("/dest/small_{i}")), 256);
        pipeline.submit_work(work).unwrap();
        // While below threshold, poll always returns None (items are buffered).
        assert!(
            pipeline.poll_result().is_none(),
            "poll should return None while buffering at item {i}"
        );
    }

    // Mode must still be Buffering since 30 < 64.
    assert!(
        matches!(pipeline.mode, ThresholdMode::Buffering(_)),
        "expected Buffering mode for {count} items (threshold 64)"
    );

    // Flush processes via SequentialDeltaPipeline path.
    let results = Box::new(pipeline).flush();
    assert_eq!(results.len(), count as usize);
    for (i, r) in results.iter().enumerate() {
        assert_eq!(r.sequence(), i as u64, "wrong sequence at index {i}");
        assert_eq!(r.ndx().get(), i as u32, "wrong ndx at index {i}");
        assert!(r.is_success(), "not successful at index {i}");
        assert_eq!(r.bytes_written(), 256, "wrong bytes_written at index {i}");
    }
}

#[test]
fn threshold_mixed_waves_below_then_above() {
    let threshold = 64;
    let mut pipeline = ThresholdDeltaPipeline::new(threshold);

    // Wave 1: 30 items - stays below threshold.
    for i in 0..30u32 {
        let work = DeltaWork::whole_file(i, PathBuf::from(format!("/dest/w1_{i}")), 128);
        pipeline.submit_work(work).unwrap();
    }
    assert!(
        matches!(pipeline.mode, ThresholdMode::Buffering(_)),
        "expected Buffering after 30 items"
    );

    // Wave 2: 40 more items - pushes past threshold at item 64.
    for i in 30..70u32 {
        let work = DeltaWork::whole_file(i, PathBuf::from(format!("/dest/w2_{i}")), 256);
        pipeline.submit_work(work).unwrap();
    }
    assert!(
        matches!(pipeline.mode, ThresholdMode::Parallel(_)),
        "expected Parallel after 70 items (threshold 64)"
    );

    let results = Box::new(pipeline).flush();
    assert_eq!(results.len(), 70);
    for (i, r) in results.iter().enumerate() {
        let expected_size = if i < 30 { 128u64 } else { 256u64 };
        assert_eq!(r.sequence(), i as u64, "wrong sequence at index {i}");
        assert_eq!(r.ndx().get(), i as u32, "wrong ndx at index {i}");
        assert!(r.is_success(), "not successful at index {i}");
        assert_eq!(
            r.bytes_written(),
            expected_size,
            "wrong bytes_written at index {i}"
        );
    }
}

#[test]
fn threshold_bypass_below_threshold_uses_sequential() {
    let mut pipeline = ThresholdDeltaPipeline::new_bypass(10);
    for i in 0..5u32 {
        let work = DeltaWork::whole_file(i, PathBuf::from(format!("/dest/{i}")), 100);
        pipeline.submit_work(work).unwrap();
    }

    // Below threshold, items are buffered and processed sequentially.
    assert!(pipeline.poll_result().is_none());

    let results = Box::new(pipeline).flush();
    assert_eq!(results.len(), 5);
    for (i, r) in results.iter().enumerate() {
        assert_eq!(r.ndx().get(), i as u32);
        assert!(r.is_success());
    }
}

#[test]
fn threshold_bypass_at_threshold_switches_to_parallel_bypass() {
    let threshold = 5;
    let mut pipeline = ThresholdDeltaPipeline::new_bypass(threshold);
    for i in 0..10u32 {
        let work = DeltaWork::whole_file(i, PathBuf::from(format!("/dest/{i}")), 64);
        pipeline.submit_work(work).unwrap();
    }

    assert!(matches!(pipeline.mode, ThresholdMode::Parallel(_)));

    let results = Box::new(pipeline).flush();
    assert_eq!(results.len(), 10);
    let mut ndx_values: Vec<u32> = results.iter().map(|r| r.ndx().get()).collect();
    ndx_values.sort_unstable();
    let expected: Vec<u32> = (0..10).collect();
    assert_eq!(ndx_values, expected);
}
