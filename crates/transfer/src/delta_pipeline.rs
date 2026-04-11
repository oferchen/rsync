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
//! - [`ParallelDeltaPipeline`] - dispatches work items to rayon workers via
//!   `engine::concurrent_delta` for parallel I/O on multi-core systems.
//! - [`ThresholdDeltaPipeline`] - automatically selects sequential below a
//!   configurable file-count threshold (default 64) and parallel above it.
//!   This matches the `PARALLEL_STAT_THRESHOLD` pattern used elsewhere in the
//!   receiver for signature computation and stat batching.
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

/// Parallel delta pipeline that dispatches work items to rayon workers.
///
/// Each call to [`submit_work`](ReceiverDeltaPipeline::submit_work) dispatches
/// the work item through [`dispatch`](engine::concurrent_delta::strategy::dispatch)
/// on a rayon thread via [`rayon::spawn`]. Results are collected through a
/// channel and reordered by sequence number so that
/// [`poll_result`](ReceiverDeltaPipeline::poll_result) returns them in
/// submission order.
///
/// This implementation benefits transfers with many files where delta
/// computation can overlap across cores. For small transfers the thread
/// dispatch overhead outweighs the parallelism benefit - prefer
/// [`SequentialDeltaPipeline`] or [`ThresholdDeltaPipeline`] in that case.
pub struct ParallelDeltaPipeline {
    /// Sequence counter for stamping work items before dispatch.
    next_sequence: u64,
    /// Sender half of the result channel.
    result_tx: std::sync::mpsc::Sender<DeltaResult>,
    /// Receiver half of the result channel.
    result_rx: std::sync::mpsc::Receiver<DeltaResult>,
    /// Results received out of order, keyed by sequence number.
    reorder: std::collections::BTreeMap<u64, DeltaResult>,
    /// Next sequence number expected by `poll_result`.
    next_poll_sequence: u64,
    /// Total number of submitted items not yet polled.
    in_flight: usize,
}

impl ParallelDeltaPipeline {
    /// Creates a new parallel pipeline.
    #[must_use]
    pub fn new() -> Self {
        let (result_tx, result_rx) = std::sync::mpsc::channel();
        Self {
            next_sequence: 0,
            result_tx,
            result_rx,
            reorder: std::collections::BTreeMap::new(),
            next_poll_sequence: 0,
            in_flight: 0,
        }
    }

    /// Drains all available results from the channel into the reorder buffer.
    fn drain_channel(&mut self) {
        while let Ok(result) = self.result_rx.try_recv() {
            self.reorder.insert(result.sequence(), result);
        }
    }
}

impl Default for ParallelDeltaPipeline {
    fn default() -> Self {
        Self::new()
    }
}

impl ReceiverDeltaPipeline for ParallelDeltaPipeline {
    fn submit_work(&mut self, mut work: DeltaWork) -> io::Result<()> {
        let seq = self.next_sequence;
        self.next_sequence += 1;
        self.in_flight += 1;
        work.set_sequence(seq);

        let tx = self.result_tx.clone();
        rayon::spawn(move || {
            let result = dispatch(&work);
            // Ignore send error - receiver may have been dropped during flush.
            let _ = tx.send(result);
        });
        Ok(())
    }

    fn poll_result(&mut self) -> Option<DeltaResult> {
        self.drain_channel();
        if let Some(result) = self.reorder.remove(&self.next_poll_sequence) {
            self.next_poll_sequence += 1;
            self.in_flight -= 1;
            Some(result)
        } else {
            None
        }
    }

    fn flush(mut self: Box<Self>) -> Vec<DeltaResult> {
        // Drop our sender clone so rayon workers hold the only senders.
        // Once all workers finish and drop their clones, recv() returns Err.
        drop(self.result_tx);

        // Collect all remaining results from workers.
        for result in &self.result_rx {
            self.reorder.insert(result.sequence(), result);
        }

        // Return in sequence order, starting from next_poll_sequence.
        self.reorder
            .into_values()
            .filter(|r| r.sequence() >= self.next_poll_sequence)
            .collect()
    }
}

/// Default threshold for switching from sequential to parallel dispatch.
///
/// Matches [`PARALLEL_STAT_THRESHOLD`](crate::receiver::PARALLEL_STAT_THRESHOLD)
/// used for parallel stat batching in the receiver, reflecting the minimum
/// batch size where rayon dispatch overhead is amortized.
pub const DEFAULT_PARALLEL_THRESHOLD: usize = 64;

/// Threshold-gated delta pipeline that automatically selects sequential or
/// parallel dispatch based on the expected batch size.
///
/// For transfers with fewer files than the threshold, items are processed
/// sequentially in the calling thread via [`SequentialDeltaPipeline`]. When
/// the batch size meets or exceeds the threshold, items are dispatched to
/// rayon workers via [`ParallelDeltaPipeline`].
///
/// The batch size must be provided at construction time via
/// [`with_batch_size`](Self::with_batch_size) so the pipeline can select the
/// appropriate strategy before any work is submitted.
///
/// # Example
///
/// ```
/// use transfer::delta_pipeline::ThresholdDeltaPipeline;
///
/// // Small transfer - will use sequential dispatch.
/// let pipeline = ThresholdDeltaPipeline::new().with_batch_size(10);
/// assert!(!pipeline.is_parallel());
///
/// // Large transfer - will use parallel dispatch.
/// let pipeline = ThresholdDeltaPipeline::new().with_batch_size(100);
/// assert!(pipeline.is_parallel());
/// ```
pub struct ThresholdDeltaPipeline {
    /// Inner pipeline, selected based on batch size vs threshold.
    inner: Box<dyn ReceiverDeltaPipeline>,
    /// Whether the parallel path was selected.
    parallel: bool,
}

impl ThresholdDeltaPipeline {
    /// Creates a new threshold pipeline with the default threshold (64).
    ///
    /// The pipeline defaults to sequential dispatch until
    /// [`with_batch_size`](Self::with_batch_size) is called.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Box::new(SequentialDeltaPipeline::new()),
            parallel: false,
        }
    }

    /// Selects the dispatch strategy based on the batch size.
    ///
    /// Uses the default threshold ([`DEFAULT_PARALLEL_THRESHOLD`]).
    #[must_use]
    pub fn with_batch_size(self, batch_size: usize) -> Self {
        Self::with_threshold_and_batch_size(DEFAULT_PARALLEL_THRESHOLD, batch_size)
    }

    /// Selects the dispatch strategy based on a custom threshold and batch size.
    #[must_use]
    pub fn with_threshold_and_batch_size(threshold: usize, batch_size: usize) -> Self {
        if batch_size >= threshold {
            Self {
                inner: Box::new(ParallelDeltaPipeline::new()),
                parallel: true,
            }
        } else {
            Self {
                inner: Box::new(SequentialDeltaPipeline::new()),
                parallel: false,
            }
        }
    }

    /// Returns `true` if the parallel dispatch path was selected.
    #[must_use]
    pub const fn is_parallel(&self) -> bool {
        self.parallel
    }
}

impl Default for ThresholdDeltaPipeline {
    fn default() -> Self {
        Self::new()
    }
}

impl ReceiverDeltaPipeline for ThresholdDeltaPipeline {
    fn submit_work(&mut self, work: DeltaWork) -> io::Result<()> {
        self.inner.submit_work(work)
    }

    fn poll_result(&mut self) -> Option<DeltaResult> {
        self.inner.poll_result()
    }

    fn flush(self: Box<Self>) -> Vec<DeltaResult> {
        self.inner.flush()
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
            let work =
                DeltaWork::whole_file(i, PathBuf::from(format!("/dest/{i}")), u64::from(i) * 100);
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
        let mut pipeline: Box<dyn ReceiverDeltaPipeline> = Box::new(SequentialDeltaPipeline::new());
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

    // ==================== ParallelDeltaPipeline tests ====================

    #[test]
    fn parallel_submit_and_poll_single() {
        let mut pipeline = ParallelDeltaPipeline::new();
        let work = DeltaWork::whole_file(0, PathBuf::from("/dest/a.txt"), 1024);
        pipeline.submit_work(work).unwrap();

        // Parallel dispatch is async - flush to guarantee completion.
        let remaining = Box::new(pipeline).flush();
        assert_eq!(remaining.len(), 1);
        assert!(remaining[0].is_success());
        assert_eq!(remaining[0].ndx(), 0);
        assert_eq!(remaining[0].bytes_written(), 1024);
    }

    #[test]
    fn parallel_submit_multiple_preserves_order() {
        let mut pipeline = ParallelDeltaPipeline::new();
        for i in 0..20 {
            let work =
                DeltaWork::whole_file(i, PathBuf::from(format!("/dest/{i}")), u64::from(i) * 100);
            pipeline.submit_work(work).unwrap();
        }

        let results = Box::new(pipeline).flush();
        assert_eq!(results.len(), 20);
        for (i, r) in results.iter().enumerate() {
            assert_eq!(r.ndx(), i as u32);
            assert_eq!(r.sequence(), i as u64);
            assert_eq!(r.bytes_written(), (i as u64) * 100);
        }
    }

    #[test]
    fn parallel_interleaved_submit_and_poll() {
        let mut pipeline = ParallelDeltaPipeline::new();

        // Submit several items, then spin-poll until we get the first one.
        for i in 0..5u32 {
            let work = DeltaWork::whole_file(i, PathBuf::from(format!("/dest/{i}")), 64);
            pipeline.submit_work(work).unwrap();
        }

        let results = Box::new(pipeline).flush();
        assert_eq!(results.len(), 5);
        for (i, r) in results.iter().enumerate() {
            assert_eq!(r.sequence(), i as u64);
        }
    }

    #[test]
    fn parallel_flush_empty_pipeline() {
        let pipeline = ParallelDeltaPipeline::new();
        let remaining = Box::new(pipeline).flush();
        assert!(remaining.is_empty());
    }

    #[test]
    fn parallel_mixed_work_kinds() {
        let mut pipeline = ParallelDeltaPipeline::new();

        let whole = DeltaWork::whole_file(0, PathBuf::from("/dest/whole"), 500);
        let delta = DeltaWork::delta(
            1,
            PathBuf::from("/dest/delta"),
            PathBuf::from("/basis/delta"),
            1000,
        );

        pipeline.submit_work(whole).unwrap();
        pipeline.submit_work(delta).unwrap();

        let results = Box::new(pipeline).flush();
        assert_eq!(results.len(), 2);

        assert_eq!(results[0].ndx(), 0);
        assert_eq!(results[0].literal_bytes(), 500);
        assert_eq!(results[0].matched_bytes(), 0);

        assert_eq!(results[1].ndx(), 1);
        assert_eq!(results[1].literal_bytes(), 500); // 50/50 split
        assert_eq!(results[1].matched_bytes(), 500);
    }

    #[test]
    fn parallel_trait_object_works() {
        let mut pipeline: Box<dyn ReceiverDeltaPipeline> =
            Box::new(ParallelDeltaPipeline::new());
        let work = DeltaWork::whole_file(7, PathBuf::from("/dest/trait_obj"), 256);
        pipeline.submit_work(work).unwrap();

        let remaining = pipeline.flush();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].ndx(), 7);
        assert!(remaining[0].is_success());
    }

    #[test]
    fn parallel_large_batch() {
        let mut pipeline = ParallelDeltaPipeline::new();
        let count = 200u32;
        for i in 0..count {
            let work = DeltaWork::whole_file(i, PathBuf::from(format!("/dest/{i}")), 32);
            pipeline.submit_work(work).unwrap();
        }

        let results = Box::new(pipeline).flush();
        assert_eq!(results.len(), count as usize);
        for (i, r) in results.iter().enumerate() {
            assert_eq!(r.ndx(), i as u32);
            assert_eq!(r.sequence(), i as u64);
            assert!(r.is_success());
        }
    }

    #[test]
    fn parallel_poll_then_flush_returns_remainder() {
        let mut pipeline = ParallelDeltaPipeline::new();
        for i in 0..10u32 {
            let work = DeltaWork::whole_file(i, PathBuf::from(format!("/dest/{i}")), 64);
            pipeline.submit_work(work).unwrap();
        }

        // Spin-poll until we get at least one result.
        let mut polled = Vec::new();
        loop {
            if let Some(r) = pipeline.poll_result() {
                polled.push(r);
                break;
            }
            std::thread::yield_now();
        }

        let remaining = Box::new(pipeline).flush();
        let total = polled.len() + remaining.len();
        assert_eq!(total, 10);
    }

    // ==================== ThresholdDeltaPipeline tests ====================

    #[test]
    fn threshold_below_uses_sequential() {
        let pipeline = ThresholdDeltaPipeline::new().with_batch_size(10);
        assert!(!pipeline.is_parallel());
    }

    #[test]
    fn threshold_at_boundary_uses_parallel() {
        let pipeline =
            ThresholdDeltaPipeline::with_threshold_and_batch_size(DEFAULT_PARALLEL_THRESHOLD, 64);
        assert!(pipeline.is_parallel());
    }

    #[test]
    fn threshold_above_uses_parallel() {
        let pipeline = ThresholdDeltaPipeline::new().with_batch_size(100);
        assert!(pipeline.is_parallel());
    }

    #[test]
    fn threshold_zero_batch_uses_sequential() {
        let pipeline = ThresholdDeltaPipeline::new().with_batch_size(0);
        assert!(!pipeline.is_parallel());
    }

    #[test]
    fn threshold_custom_threshold() {
        let pipeline = ThresholdDeltaPipeline::with_threshold_and_batch_size(10, 9);
        assert!(!pipeline.is_parallel());

        let pipeline = ThresholdDeltaPipeline::with_threshold_and_batch_size(10, 10);
        assert!(pipeline.is_parallel());
    }

    #[test]
    fn threshold_sequential_path_submit_and_poll() {
        let mut pipeline = ThresholdDeltaPipeline::new().with_batch_size(5);
        assert!(!pipeline.is_parallel());

        let work = DeltaWork::whole_file(0, PathBuf::from("/dest/a"), 256);
        pipeline.submit_work(work).unwrap();

        let result = pipeline.poll_result().unwrap();
        assert!(result.is_success());
        assert_eq!(result.ndx(), 0);
        assert_eq!(result.bytes_written(), 256);
    }

    #[test]
    fn threshold_parallel_path_submit_and_flush() {
        let mut pipeline = ThresholdDeltaPipeline::new().with_batch_size(100);
        assert!(pipeline.is_parallel());

        for i in 0..5u32 {
            let work = DeltaWork::whole_file(i, PathBuf::from(format!("/dest/{i}")), 128);
            pipeline.submit_work(work).unwrap();
        }

        let results = Box::new(pipeline).flush();
        assert_eq!(results.len(), 5);
        for (i, r) in results.iter().enumerate() {
            assert_eq!(r.ndx(), i as u32);
            assert!(r.is_success());
        }
    }

    #[test]
    fn threshold_default_is_sequential() {
        let pipeline = ThresholdDeltaPipeline::default();
        assert!(!pipeline.is_parallel());
    }

    #[test]
    fn threshold_trait_object_works() {
        let mut pipeline: Box<dyn ReceiverDeltaPipeline> =
            Box::new(ThresholdDeltaPipeline::new().with_batch_size(5));
        let work = DeltaWork::whole_file(3, PathBuf::from("/dest/obj"), 64);
        pipeline.submit_work(work).unwrap();

        let result = pipeline.poll_result().unwrap();
        assert_eq!(result.ndx(), 3);

        let remaining = pipeline.flush();
        assert!(remaining.is_empty());
    }

    #[test]
    fn threshold_flush_empty() {
        let pipeline = ThresholdDeltaPipeline::new().with_batch_size(10);
        let remaining = Box::new(pipeline).flush();
        assert!(remaining.is_empty());
    }

    #[test]
    fn threshold_default_constant_matches_receiver() {
        assert_eq!(DEFAULT_PARALLEL_THRESHOLD, 64);
    }
}
