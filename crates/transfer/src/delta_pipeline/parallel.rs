//! Parallel delta pipeline that dispatches work to rayon workers.

use std::io;

use engine::concurrent_delta::consumer::DeltaConsumer;
use engine::concurrent_delta::work_queue::{self, WorkQueueSender};
use engine::concurrent_delta::{DeltaResult, DeltaWork};

use super::ReceiverDeltaPipeline;

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
    ///
    /// Use [`new_adaptive`](Self::new_adaptive) when the average target file
    /// size is known - small-file workloads benefit from deeper queues.
    #[must_use]
    pub fn new(worker_count: usize) -> Self {
        let capacity = worker_count.saturating_mul(2).max(2);
        Self::with_capacity(capacity)
    }

    /// Creates a new parallel pipeline whose work queue depth adapts to the
    /// workload's average file size and the available core count.
    ///
    /// The capacity multiplier scales between 2x (large-file, I/O-bound) and
    /// 8x (small-file, CPU/syscall-bound) of `worker_count`, mirroring the
    /// heuristic in
    /// [`work_queue::adaptive_queue_depth`](engine::concurrent_delta::work_queue::adaptive_queue_depth).
    ///
    /// Pass `avg_target_size == 0` when the workload is unknown to fall back
    /// to the default 2x multiplier - identical to [`new`](Self::new).
    #[must_use]
    pub fn new_adaptive(worker_count: usize, avg_target_size: u64) -> Self {
        let capacity = adaptive_capacity(worker_count, avg_target_size);
        Self::with_capacity(capacity)
    }

    fn with_capacity(capacity: usize) -> Self {
        let (work_tx, work_rx) = work_queue::bounded_with_capacity(capacity);
        let consumer = DeltaConsumer::spawn(work_rx, capacity);

        Self {
            next_sequence: 0,
            work_tx: Some(work_tx),
            consumer: Some(consumer),
        }
    }

    /// Creates a parallel pipeline that bypasses reorder buffering.
    ///
    /// Results are delivered in completion order rather than submission order.
    /// This eliminates reorder overhead when strict file-list ordering is
    /// unnecessary - for example, when `--delay-updates` is off and files
    /// are committed immediately upon completion.
    #[must_use]
    pub fn new_bypass(worker_count: usize) -> Self {
        let capacity = worker_count.saturating_mul(2).max(2);
        Self::with_bypass_capacity(capacity)
    }

    /// Bypass-mode variant of [`new_adaptive`](Self::new_adaptive).
    #[must_use]
    pub fn new_bypass_adaptive(worker_count: usize, avg_target_size: u64) -> Self {
        let capacity = adaptive_capacity(worker_count, avg_target_size);
        Self::with_bypass_capacity(capacity)
    }

    fn with_bypass_capacity(capacity: usize) -> Self {
        let (work_tx, work_rx) = work_queue::bounded_with_capacity(capacity);
        let consumer = DeltaConsumer::spawn_bypass(work_rx);

        Self {
            next_sequence: 0,
            work_tx: Some(work_tx),
            consumer: Some(consumer),
        }
    }
}

/// Computes the bounded work queue capacity for a given worker count and
/// average target file size.
///
/// Mirrors [`work_queue::adaptive_queue_depth`] but operates on an explicit
/// `worker_count` rather than `rayon::current_num_threads()` so the receiver
/// can pass the configured thread budget when it differs from rayon's pool.
pub(super) fn adaptive_capacity(worker_count: usize, avg_target_size: u64) -> usize {
    const SMALL_FILE_THRESHOLD: u64 = 64 * 1024;
    const LARGE_FILE_THRESHOLD: u64 = 1024 * 1024;
    let multiplier: usize = if avg_target_size == 0 {
        2
    } else if avg_target_size < SMALL_FILE_THRESHOLD {
        8
    } else if avg_target_size > LARGE_FILE_THRESHOLD {
        2
    } else {
        4
    };
    worker_count.saturating_mul(multiplier).max(2)
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
