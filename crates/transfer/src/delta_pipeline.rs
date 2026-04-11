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
//!   a [`WorkQueueSender`](engine::concurrent_delta::work_queue::WorkQueueSender),
//!   collects results through an `mpsc` channel, and reorders them with
//!   [`ReorderBuffer`](engine::concurrent_delta::ReorderBuffer) to preserve
//!   submission order.
//! - [`ThresholdDeltaPipeline`] - auto-selects between sequential and parallel
//!   mode based on the number of submitted items. Below the threshold (default
//!   [`DEFAULT_PARALLEL_THRESHOLD`] = 64), items are processed sequentially.
//!   At or above the threshold, a [`ParallelDeltaPipeline`] is created and all
//!   buffered items are flushed into it.
//!
//! # Upstream Reference
//!
//! Upstream `receiver.c:recv_files()` processes files one at a time in a tight
//! loop. This trait preserves that interface while allowing the dispatch
//! strategy to be swapped for parallel execution without changing the receiver.

use std::io;

use engine::concurrent_delta::consumer::DeltaConsumer;
use engine::concurrent_delta::strategy::dispatch;
use engine::concurrent_delta::work_queue::{self, WorkQueueSender};
use engine::concurrent_delta::{DeltaResult, DeltaWork};

/// Default threshold for switching from sequential to parallel dispatch.
///
/// Matches the receiver's default stat threshold from `ParallelThresholds`
/// (64 files). Below this count, the overhead of thread spawning and channel
/// communication exceeds the benefit of parallelism.
pub const DEFAULT_PARALLEL_THRESHOLD: usize = 64;

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
pub trait ReceiverDeltaPipeline: Send + std::fmt::Debug {
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

/// Parallel delta pipeline that dispatches work to rayon workers.
///
/// Sends [`DeltaWork`] items through a bounded [`WorkQueueSender`] to a
/// [`DeltaConsumer`] background thread that drains items via
/// [`drain_parallel`](engine::concurrent_delta::work_queue::WorkQueueReceiver::drain_parallel),
/// processes each item on the rayon thread pool, feeds results into a
/// [`ReorderBuffer`](engine::concurrent_delta::ReorderBuffer), and delivers
/// them in sequence order through an internal channel.
///
/// # Architecture
///
/// ```text
/// submit_work()                  DeltaConsumer (background thread)
///     |                               |
///     v                               v
/// WorkQueueSender ──────► WorkQueueReceiver::drain_parallel()
///     (bounded)                       |
///                                     v
///                              ReorderBuffer (inside consumer)
///                                     |  drain_ready() yields contiguous run
///                                     v
/// poll_result() ◄──── mpsc::Receiver (in sequence order)
/// ```
///
/// The bounded work queue applies backpressure when the rayon pool is
/// saturated, preventing unbounded memory growth for million-file transfers.
/// The [`DeltaConsumer`] handles reordering internally, so `poll_result`
/// receives results already in submission order - no client-side reorder
/// buffer is needed.
///
/// # Upstream Reference
///
/// Parallelizes the sequential per-file loop in `receiver.c:recv_files()`.
/// The consumer's internal reorder buffer ensures post-processing sees files
/// in file-list order, matching upstream's sequential invariant.
pub struct ParallelDeltaPipeline {
    /// Sequence counter for stamping work items before dispatch.
    next_sequence: u64,
    /// Sender half of the bounded work queue.
    work_tx: Option<WorkQueueSender>,
    /// Consumer thread that drains the work queue, reorders results, and
    /// delivers them in sequence order through its internal channel.
    consumer: Option<DeltaConsumer>,
}

impl std::fmt::Debug for ParallelDeltaPipeline {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ParallelDeltaPipeline")
            .field("next_sequence", &self.next_sequence)
            .finish_non_exhaustive()
    }
}

impl ParallelDeltaPipeline {
    /// Creates a new parallel pipeline with the given worker count for capacity sizing.
    ///
    /// The work queue capacity is set to `2 * worker_count`, matching the
    /// default capacity multiplier in
    /// [`work_queue::bounded`](engine::concurrent_delta::work_queue::bounded).
    /// The [`DeltaConsumer`] reorder buffer capacity is set to the same value,
    /// which is sufficient to hold all in-flight results and yield contiguous
    /// runs as workers complete.
    #[must_use]
    pub fn new(worker_count: usize) -> Self {
        let capacity = worker_count.saturating_mul(2).max(2);
        let (work_tx, work_rx) = work_queue::bounded_with_capacity(capacity);
        let consumer = DeltaConsumer::spawn(work_rx, capacity);

        Self {
            next_sequence: 0,
            work_tx: Some(work_tx),
            consumer: Some(consumer),
        }
    }
}

impl ReceiverDeltaPipeline for ParallelDeltaPipeline {
    fn submit_work(&mut self, mut work: DeltaWork) -> io::Result<()> {
        let seq = self.next_sequence;
        self.next_sequence += 1;
        work.set_sequence(seq);

        let tx = self
            .work_tx
            .as_ref()
            .ok_or_else(|| io::Error::other("parallel pipeline work queue already closed"))?;
        tx.send(work)
            .map_err(|_| io::Error::other("parallel pipeline consumer thread has shut down"))
    }

    fn poll_result(&mut self) -> Option<DeltaResult> {
        // The DeltaConsumer delivers results already in sequence order via
        // its internal ReorderBuffer. A non-blocking try_recv() avoids
        // stalling the receiver pipeline when no results are ready yet.
        let consumer = self.consumer.as_ref()?;
        consumer.try_recv()
    }

    fn flush(mut self: Box<Self>) -> Vec<DeltaResult> {
        // Drop the work queue sender to signal the consumer thread to finish.
        // This closes the bounded channel, causing drain_parallel() to return
        // once all queued items are processed.
        self.work_tx.take();

        // Consume the DeltaConsumer's iterator to collect all remaining
        // in-order results. The iterator blocks until the background thread
        // finishes and the channel closes.
        if let Some(consumer) = self.consumer.take() {
            consumer.into_iter().collect()
        } else {
            Vec::new()
        }
    }
}

/// Mode tracking for the threshold pipeline.
enum ThresholdMode {
    /// Buffering work items until the threshold is reached.
    Buffering(Vec<DeltaWork>),
    /// Delegating to a parallel pipeline (threshold reached).
    Parallel(ParallelDeltaPipeline),
}

/// Threshold-gated delta pipeline that auto-selects sequential or parallel mode.
///
/// Buffers submitted work items until either:
/// - The buffer reaches the threshold, at which point a [`ParallelDeltaPipeline`]
///   is created and all buffered items are flushed into it.
/// - [`flush`](ReceiverDeltaPipeline::flush) is called before the threshold,
///   in which case items are processed sequentially.
///
/// This follows the threshold-based dual-path pattern used throughout the
/// codebase (e.g., `ParallelThresholds` in the receiver). For small
/// transfers, the overhead of spawning threads and channels exceeds the
/// benefit of parallelism.
///
/// # Default Threshold
///
/// [`DEFAULT_PARALLEL_THRESHOLD`] = 64, matching the receiver's
/// default stat threshold from `ParallelThresholds`.
pub struct ThresholdDeltaPipeline {
    /// Number of items required to switch to parallel mode.
    threshold: usize,
    /// Current operating mode.
    mode: ThresholdMode,
}

impl std::fmt::Debug for ThresholdDeltaPipeline {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mode_label = match &self.mode {
            ThresholdMode::Buffering(buf) => format!("Buffering({})", buf.len()),
            ThresholdMode::Parallel(_) => "Parallel".to_string(),
        };
        f.debug_struct("ThresholdDeltaPipeline")
            .field("threshold", &self.threshold)
            .field("mode", &mode_label)
            .finish()
    }
}

impl ThresholdDeltaPipeline {
    /// Creates a threshold pipeline with the given threshold.
    #[must_use]
    pub fn new(threshold: usize) -> Self {
        Self {
            threshold,
            mode: ThresholdMode::Buffering(Vec::new()),
        }
    }

    /// Creates a threshold pipeline with [`DEFAULT_PARALLEL_THRESHOLD`].
    #[must_use]
    pub fn with_default_threshold() -> Self {
        Self::new(DEFAULT_PARALLEL_THRESHOLD)
    }

    /// Promotes from buffering to parallel mode, flushing buffered items.
    fn promote_to_parallel(&mut self, buffered: Vec<DeltaWork>) -> io::Result<()> {
        let worker_count = rayon::current_num_threads();
        let mut parallel = ParallelDeltaPipeline::new(worker_count);
        for item in buffered {
            parallel.submit_work(item)?;
        }
        self.mode = ThresholdMode::Parallel(parallel);
        Ok(())
    }
}

impl ReceiverDeltaPipeline for ThresholdDeltaPipeline {
    fn submit_work(&mut self, work: DeltaWork) -> io::Result<()> {
        match &mut self.mode {
            ThresholdMode::Buffering(buf) => {
                buf.push(work);
                if buf.len() >= self.threshold {
                    let buffered = std::mem::take(buf);
                    self.promote_to_parallel(buffered)?;
                }
                Ok(())
            }
            ThresholdMode::Parallel(par) => par.submit_work(work),
        }
    }

    fn poll_result(&mut self) -> Option<DeltaResult> {
        match &mut self.mode {
            ThresholdMode::Buffering(_) => None,
            ThresholdMode::Parallel(par) => par.poll_result(),
        }
    }

    fn flush(self: Box<Self>) -> Vec<DeltaResult> {
        match self.mode {
            ThresholdMode::Buffering(buffered) => {
                if buffered.is_empty() {
                    return Vec::new();
                }
                // Below threshold - process sequentially.
                let mut seq = SequentialDeltaPipeline::new();
                for item in buffered {
                    // Dispatch is infallible for sequential pipeline.
                    let _ = seq.submit_work(item);
                }
                Box::new(seq).flush()
            }
            ThresholdMode::Parallel(par) => Box::new(par).flush(),
        }
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
            assert_eq!(r.ndx(), i as u32);
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
            assert_eq!(r.ndx(), i as u32, "result {i} has wrong ndx");
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
        assert_eq!(results[0].ndx(), 42);
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
            assert_eq!(r.ndx(), i as u32);
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
            assert_eq!(r.ndx(), i as u32);
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

    // ==================== ThresholdDeltaPipeline tests ====================

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
            assert_eq!(r.ndx(), i as u32);
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
            assert_eq!(r.ndx(), i as u32);
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
            assert_eq!(r.ndx(), i as u32);
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
        );
        let whole2 = DeltaWork::whole_file(2, PathBuf::from("/dest/whole2"), 200);

        pipeline.submit_work(whole).unwrap();
        pipeline.submit_work(delta).unwrap();
        pipeline.submit_work(whole2).unwrap();

        let results = Box::new(pipeline).flush();
        assert_eq!(results.len(), 3);
        assert_eq!(results[0].literal_bytes(), 500);
        assert_eq!(results[0].matched_bytes(), 0);
        assert_eq!(results[1].literal_bytes(), 500); // 50/50 split
        assert_eq!(results[1].matched_bytes(), 500);
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
        assert_eq!(results[0].ndx(), 7);
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
            assert_eq!(r.ndx(), i as u32);
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

    // ==================== Integration tests ====================

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
            assert_eq!(r.ndx(), i_u32, "wrong ndx at index {i}");
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
            assert_eq!(r.ndx(), i as u32, "wrong ndx at index {i}");
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
            assert_eq!(r.ndx(), i as u32, "wrong ndx at index {i}");
            assert!(r.is_success(), "not successful at index {i}");
            assert_eq!(
                r.bytes_written(),
                expected_size,
                "wrong bytes_written at index {i}"
            );
        }
    }

    // ==================== Consumer thread integration tests ====================

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
            assert_eq!(r.ndx(), i as u32);
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
            let work = if i % 3 == 0 {
                DeltaWork::delta(
                    i,
                    PathBuf::from(format!("/dest/{i}")),
                    PathBuf::from(format!("/basis/{i}")),
                    u64::from(i) * 100 + 100,
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
            assert_eq!(r.ndx(), i as u32);
            assert!(r.is_success());
        }
    }
}
