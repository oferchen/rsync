//! Pluggable delta dispatch pipeline for the receiver transfer loop.
//!
//! Defines [`ReceiverDeltaPipeline`], a trait that abstracts how the receiver
//! dispatches per-file delta work items and collects their results. This
//! follows the Dependency Inversion principle - the receiver depends on this
//! abstraction rather than a concrete pipeline implementation.
//!
//! # Implementations
//!
//! - [`SequentialDeltaPipeline`] - processes items immediately in the calling
//!   thread with no concurrency. This matches upstream rsync's sequential
//!   `recv_files()` loop in `receiver.c` and is the default.
//! - A parallel implementation (dispatching to rayon workers via
//!   `engine::concurrent_delta`) is planned as a future addition.
//!
//! # Upstream Reference
//!
//! Upstream `receiver.c:recv_files()` processes files one at a time in a tight
//! loop. This trait preserves that interface while allowing the dispatch
//! strategy to be swapped for parallel execution without changing the receiver.

use std::io;

use engine::concurrent_delta::strategy::dispatch;
use engine::concurrent_delta::{DeltaResult, DeltaWork};

/// Abstraction over the delta dispatch loop in the receiver.
///
/// The receiver submits [`DeltaWork`] items as it reads file entries from the
/// wire, and polls for [`DeltaResult`] values to drive post-processing
/// (checksum verification, temp-file commit, metadata application).
///
/// Implementations may process work synchronously in [`submit_work`] or
/// dispatch it to background workers and return results asynchronously
/// via [`poll_result`].
///
/// [`submit_work`]: ReceiverDeltaPipeline::submit_work
/// [`poll_result`]: ReceiverDeltaPipeline::poll_result
pub trait ReceiverDeltaPipeline: Send {
    /// Submits a file for delta processing.
    ///
    /// The implementation may process the work item immediately (sequential)
    /// or enqueue it for background processing (parallel). In either case,
    /// the result becomes available via [`poll_result`](Self::poll_result).
    ///
    /// # Errors
    ///
    /// Returns an I/O error if the work item cannot be accepted - for example,
    /// when a background work queue has been shut down.
    fn submit_work(&mut self, work: DeltaWork) -> io::Result<()>;

    /// Tries to retrieve the next in-order result.
    ///
    /// Returns `Some(result)` if the next result in submission order is
    /// available, or `None` if no results are ready yet. Callers should
    /// poll after each [`submit_work`](Self::submit_work) call and again
    /// during flush to drain all pending results.
    fn poll_result(&mut self) -> Option<DeltaResult>;

    /// Drains all remaining results in submission order.
    ///
    /// Consumes the pipeline and returns any buffered or in-flight results.
    /// After this call, no further work can be submitted.
    fn flush(self: Box<Self>) -> Vec<DeltaResult>;
}

/// Sequential delta pipeline that processes each item immediately.
///
/// This is the default pipeline implementation. Each call to
/// [`submit_work`](ReceiverDeltaPipeline::submit_work) synchronously
/// dispatches the work item through the appropriate
/// [`DeltaStrategy`](engine::concurrent_delta::DeltaStrategy) and buffers
/// the result for the next [`poll_result`](ReceiverDeltaPipeline::poll_result)
/// call.
///
/// No threads are spawned. Processing order matches submission order, which
/// is identical to upstream rsync's sequential `recv_files()` loop.
///
/// # Upstream Reference
///
/// Mirrors the 1:1 dispatch in `receiver.c:recv_files()` where each file is
/// fully processed before moving to the next.
#[derive(Debug, Default)]
pub struct SequentialDeltaPipeline {
    /// Sequence counter for stamping work items before dispatch.
    next_sequence: u64,
    /// Results waiting to be polled, in submission order.
    ready: Vec<DeltaResult>,
    /// Read cursor into `ready` for FIFO delivery.
    cursor: usize,
}

impl SequentialDeltaPipeline {
    /// Creates a new sequential pipeline.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl ReceiverDeltaPipeline for SequentialDeltaPipeline {
    fn submit_work(&mut self, mut work: DeltaWork) -> io::Result<()> {
        let seq = self.next_sequence;
        self.next_sequence += 1;
        work.set_sequence(seq);
        let result = dispatch(&work);
        self.ready.push(result);
        Ok(())
    }

    fn poll_result(&mut self) -> Option<DeltaResult> {
        if self.cursor < self.ready.len() {
            let result = self.ready[self.cursor].clone();
            self.cursor += 1;
            Some(result)
        } else {
            None
        }
    }

    fn flush(self: Box<Self>) -> Vec<DeltaResult> {
        if self.cursor >= self.ready.len() {
            return Vec::new();
        }
        self.ready.into_iter().skip(self.cursor).collect()
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use engine::concurrent_delta::{DeltaResultStatus, DeltaWork};

    use super::*;

    #[test]
    fn sequential_submit_and_poll_single() {
        let mut pipeline = SequentialDeltaPipeline::new();
        let work = DeltaWork::whole_file(0, PathBuf::from("/dest/a.txt"), 1024);
        pipeline.submit_work(work).unwrap();

        let result = pipeline.poll_result().unwrap();
        assert!(result.is_success());
        assert_eq!(result.ndx(), 0);
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
            let work = DeltaWork::whole_file(
                i,
                PathBuf::from(format!("/dest/{i}")),
                u64::from(i) * 100,
            );
            pipeline.submit_work(work).unwrap();
        }

        for i in 0..5u32 {
            let result = pipeline.poll_result().unwrap();
            assert_eq!(result.ndx(), i);
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
        );
        pipeline.submit_work(work).unwrap();

        let result = pipeline.poll_result().unwrap();
        assert!(result.is_success());
        assert_eq!(result.ndx(), 5);
        assert_eq!(result.bytes_written(), 4096);
        // DeltaTransferStrategy splits 50/50.
        assert_eq!(result.matched_bytes(), 2048);
        assert_eq!(result.literal_bytes(), 2048);
    }

    #[test]
    fn sequential_interleaved_submit_and_poll() {
        let mut pipeline = SequentialDeltaPipeline::new();

        // Submit one, poll one, submit another, poll another.
        let work0 = DeltaWork::whole_file(0, PathBuf::from("/dest/0"), 100);
        pipeline.submit_work(work0).unwrap();
        let r0 = pipeline.poll_result().unwrap();
        assert_eq!(r0.ndx(), 0);
        assert_eq!(r0.sequence(), 0);

        let work1 = DeltaWork::whole_file(1, PathBuf::from("/dest/1"), 200);
        pipeline.submit_work(work1).unwrap();
        let r1 = pipeline.poll_result().unwrap();
        assert_eq!(r1.ndx(), 1);
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
        assert_eq!(remaining[0].ndx(), 2);
        assert_eq!(remaining[0].sequence(), 2);
        assert_eq!(remaining[1].ndx(), 3);
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
            assert_eq!(r.ndx(), i as u32);
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
        let mut pipeline: Box<dyn ReceiverDeltaPipeline> =
            Box::new(SequentialDeltaPipeline::new());
        let work = DeltaWork::whole_file(7, PathBuf::from("/dest/trait_obj"), 256);
        pipeline.submit_work(work).unwrap();

        let result = pipeline.poll_result().unwrap();
        assert_eq!(result.ndx(), 7);
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
        );

        pipeline.submit_work(whole).unwrap();
        pipeline.submit_work(delta).unwrap();

        let r0 = pipeline.poll_result().unwrap();
        assert_eq!(r0.ndx(), 0);
        assert_eq!(r0.literal_bytes(), 500);
        assert_eq!(r0.matched_bytes(), 0);

        let r1 = pipeline.poll_result().unwrap();
        assert_eq!(r1.ndx(), 1);
        assert_eq!(r1.literal_bytes(), 500); // 50/50 split
        assert_eq!(r1.matched_bytes(), 500);
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
}
