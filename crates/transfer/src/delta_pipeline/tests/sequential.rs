use std::path::PathBuf;

use engine::concurrent_delta::{DeltaResultStatus, DeltaWork};

use crate::delta_pipeline::{ReceiverDeltaPipeline, SequentialDeltaPipeline};

#[test]
fn sequential_submit_and_poll_single() {
    let mut pipeline = SequentialDeltaPipeline::new();
    let work = DeltaWork::whole_file(0, PathBuf::from("/dest/a.txt"), 1024);
    pipeline.submit_work(work).unwrap();

    let result = pipeline.poll_result().unwrap();
    assert!(result.is_success());
    assert_eq!(result.ndx().get(), 0);
    assert_eq!(result.bytes_written(), 1024);
    assert_eq!(result.literal_bytes(), 1024);
    assert_eq!(result.matched_bytes(), 0);
    assert_eq!(result.sequence(), 0);

    assert!(pipeline.poll_result().is_none());
}

#[test]
fn sequential_submit_multiple_preserves_order() {
    let mut pipeline = SequentialDeltaPipeline::new();
    for i in 0..5 {
        let work =
            DeltaWork::whole_file(i, PathBuf::from(format!("/dest/{i}")), u64::from(i) * 100);
        pipeline.submit_work(work).unwrap();
    }

    for i in 0..5u32 {
        let result = pipeline.poll_result().unwrap();
        assert_eq!(result.ndx().get(), i);
        assert_eq!(result.sequence(), u64::from(i));
        assert_eq!(result.bytes_written(), u64::from(i) * 100);
    }
    assert!(pipeline.poll_result().is_none());
}

#[test]
fn sequential_delta_work_uses_delta_strategy() {
    let mut pipeline = SequentialDeltaPipeline::new();
    let work = DeltaWork::delta(
        5,
        PathBuf::from("/dest/b.txt"),
        PathBuf::from("/basis/b.txt"),
        4096,
        1200,
        2896,
    );
    pipeline.submit_work(work).unwrap();

    let result = pipeline.poll_result().unwrap();
    assert!(result.is_success());
    assert_eq!(result.ndx().get(), 5);
    assert_eq!(result.bytes_written(), 4096);
    assert_eq!(result.matched_bytes(), 2896);
    assert_eq!(result.literal_bytes(), 1200);
}

#[test]
fn sequential_interleaved_submit_and_poll() {
    let mut pipeline = SequentialDeltaPipeline::new();

    // Submit one, poll one, submit another, poll another.
    let work0 = DeltaWork::whole_file(0, PathBuf::from("/dest/0"), 100);
    pipeline.submit_work(work0).unwrap();
    let r0 = pipeline.poll_result().unwrap();
    assert_eq!(r0.ndx().get(), 0);
    assert_eq!(r0.sequence(), 0);

    let work1 = DeltaWork::whole_file(1, PathBuf::from("/dest/1"), 200);
    pipeline.submit_work(work1).unwrap();
    let r1 = pipeline.poll_result().unwrap();
    assert_eq!(r1.ndx().get(), 1);
    assert_eq!(r1.sequence(), 1);

    assert!(pipeline.poll_result().is_none());
}

#[test]
fn sequential_flush_returns_remaining() {
    let mut pipeline = SequentialDeltaPipeline::new();
    for i in 0..4 {
        let work = DeltaWork::whole_file(i, PathBuf::from(format!("/dest/{i}")), 64);
        pipeline.submit_work(work).unwrap();
    }

    // Poll two, flush should return the remaining two.
    pipeline.poll_result().unwrap();
    pipeline.poll_result().unwrap();

    let remaining = Box::new(pipeline).flush();
    assert_eq!(remaining.len(), 2);
    assert_eq!(remaining[0].ndx().get(), 2);
    assert_eq!(remaining[0].sequence(), 2);
    assert_eq!(remaining[1].ndx().get(), 3);
    assert_eq!(remaining[1].sequence(), 3);
}

#[test]
fn sequential_flush_empty_when_all_polled() {
    let mut pipeline = SequentialDeltaPipeline::new();
    let work = DeltaWork::whole_file(0, PathBuf::from("/dest/a"), 50);
    pipeline.submit_work(work).unwrap();
    pipeline.poll_result().unwrap();

    let remaining = Box::new(pipeline).flush();
    assert!(remaining.is_empty());
}

#[test]
fn sequential_flush_empty_pipeline() {
    let pipeline = SequentialDeltaPipeline::new();
    let remaining = Box::new(pipeline).flush();
    assert!(remaining.is_empty());
}

#[test]
fn sequential_flush_returns_all_when_none_polled() {
    let mut pipeline = SequentialDeltaPipeline::new();
    for i in 0..3 {
        let work = DeltaWork::whole_file(i, PathBuf::from(format!("/dest/{i}")), 32);
        pipeline.submit_work(work).unwrap();
    }

    let remaining = Box::new(pipeline).flush();
    assert_eq!(remaining.len(), 3);
    for (i, r) in remaining.iter().enumerate() {
        assert_eq!(r.ndx().get(), i as u32);
        assert_eq!(r.sequence(), i as u64);
    }
}

#[test]
fn sequential_zero_size_file() {
    let mut pipeline = SequentialDeltaPipeline::new();
    let work = DeltaWork::whole_file(0, PathBuf::from("/dest/empty"), 0);
    pipeline.submit_work(work).unwrap();

    let result = pipeline.poll_result().unwrap();
    assert!(result.is_success());
    assert_eq!(result.bytes_written(), 0);
    assert_eq!(result.literal_bytes(), 0);
    assert_eq!(result.matched_bytes(), 0);
}

#[test]
fn sequential_trait_object_works() {
    let mut pipeline: Box<dyn ReceiverDeltaPipeline> = Box::new(SequentialDeltaPipeline::new());
    let work = DeltaWork::whole_file(7, PathBuf::from("/dest/trait_obj"), 256);
    pipeline.submit_work(work).unwrap();

    let result = pipeline.poll_result().unwrap();
    assert_eq!(result.ndx().get(), 7);
    assert!(result.is_success());

    let remaining = pipeline.flush();
    assert!(remaining.is_empty());
}

#[test]
fn sequential_mixed_work_kinds() {
    let mut pipeline = SequentialDeltaPipeline::new();

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

    let r0 = pipeline.poll_result().unwrap();
    assert_eq!(r0.ndx().get(), 0);
    assert_eq!(r0.literal_bytes(), 500);
    assert_eq!(r0.matched_bytes(), 0);

    let r1 = pipeline.poll_result().unwrap();
    assert_eq!(r1.ndx().get(), 1);
    assert_eq!(r1.literal_bytes(), 400);
    assert_eq!(r1.matched_bytes(), 600);
}

#[test]
fn sequential_sequence_monotonically_increases() {
    let mut pipeline = SequentialDeltaPipeline::new();
    for i in 0..10 {
        let work = DeltaWork::whole_file(i, PathBuf::from(format!("/dest/{i}")), 16);
        pipeline.submit_work(work).unwrap();
    }

    let mut prev_seq = None;
    while let Some(result) = pipeline.poll_result() {
        if let Some(prev) = prev_seq {
            assert_eq!(result.sequence(), prev + 1);
        }
        prev_seq = Some(result.sequence());
    }
    assert_eq!(prev_seq, Some(9));
}

#[test]
fn sequential_result_status_variants() {
    let mut pipeline = SequentialDeltaPipeline::new();

    // Both whole-file and delta produce Success status via the strategies.
    let work = DeltaWork::whole_file(0, PathBuf::from("/dest/a"), 100);
    pipeline.submit_work(work).unwrap();
    let result = pipeline.poll_result().unwrap();
    assert_eq!(*result.status(), DeltaResultStatus::Success);
}
