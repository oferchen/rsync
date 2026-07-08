//! Parallel delta pipeline that dispatches work to rayon workers.

use std::io;

use engine::concurrent_delta::consumer::DeltaConsumer;
use engine::concurrent_delta::work_queue::{
    self, AdaptiveQueueController, DynamicWorkQueue, WorkQueueSender, adaptive_queue_enabled,
};
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
    /// AIMD controller that grows/shrinks the dynamic queue's admission depth.
    ///
    /// `Some` on the default adaptive path; `None` when
    /// `OC_RSYNC_ADAPTIVE_QUEUE` pins the deterministic static depth.
    controller: Option<AdaptiveQueueController>,
    /// Submits observed since the controller last ticked. Ticking every
    /// [`CONTROLLER_TICK_WINDOW`] submits keeps each block-rate sample wide
    /// enough to clear the controller's `MIN_SAMPLES` floor.
    submits_since_tick: u32,
}

/// Hard floor for the adaptive admission depth.
///
/// Matches the `.max(2)` floor of the static capacity heuristics, so the
/// controller can throttle a saturated consumer hard without ever collapsing
/// admission below two in-flight items.
const ADAPTIVE_MIN_DEPTH: usize = 2;

/// Multiplier applied to the baseline depth to derive the adaptive ceiling.
///
/// Bounds worst-case in-flight work - and the reorder window sized to match -
/// to twice today's static baseline, so a fast consumer that keeps the
/// controller in slow-start cannot grow memory beyond a predictable cap.
const ADAPTIVE_MAX_FACTOR: usize = 2;

/// Number of submitted items between controller ticks.
///
/// Comfortably above the controller's `MIN_SAMPLES` floor so each tick reads a
/// stable block-rate window. The parallel path only engages past the 64-item
/// threshold, so a real transfer always ticks several times.
const CONTROLLER_TICK_WINDOW: u32 = 32;

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
        Self::build(capacity, false)
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
        Self::build(capacity, true)
    }

    /// Builds a pipeline sized at `capacity`, choosing the adaptive dynamic
    /// queue by default and falling back to the deterministic static queue when
    /// `OC_RSYNC_ADAPTIVE_QUEUE` disables adaptation (or, defensively, if the
    /// dynamic-queue bounds are somehow rejected).
    fn build(capacity: usize, bypass: bool) -> Self {
        if adaptive_queue_enabled() {
            if let Some(pipeline) = Self::try_adaptive(capacity, bypass) {
                return pipeline;
            }
        }
        Self::with_static_capacity(capacity, bypass)
    }

    /// Attempts to build the adaptive pipeline: a [`bounded_dynamic`] admission
    /// queue governed by an [`AdaptiveQueueController`], initialised at
    /// `capacity` (today's static baseline) and free to move within
    /// `[ADAPTIVE_MIN_DEPTH, capacity * ADAPTIVE_MAX_FACTOR]`.
    ///
    /// Returns `None` on the practically impossible bounds error so the caller
    /// degrades cleanly to the static path rather than failing the transfer.
    ///
    /// [`bounded_dynamic`]: work_queue::bounded_dynamic
    fn try_adaptive(capacity: usize, bypass: bool) -> Option<Self> {
        let min = ADAPTIVE_MIN_DEPTH.min(capacity);
        let max = capacity
            .saturating_mul(ADAPTIVE_MAX_FACTOR)
            .clamp(capacity, work_queue::MAX_CAPACITY);
        let queue = work_queue::bounded_dynamic(capacity, min, max).ok()?;
        // Construct the controller before splitting the queue: it clones the
        // shared semaphore and reads the `[min, max]` clamp from the sender.
        let controller = AdaptiveQueueController::new(&queue);
        let DynamicWorkQueue {
            sender, receiver, ..
        } = queue;
        // Size the reorder window to the admission ceiling so a controller that
        // grows depth to `max` never forces the reorder buffer past capacity.
        let consumer = if bypass {
            DeltaConsumer::spawn_bypass(receiver)
        } else {
            DeltaConsumer::spawn(receiver, max)
        };
        Some(Self {
            next_sequence: 0,
            work_tx: Some(sender),
            consumer: Some(consumer),
            controller: Some(controller),
            submits_since_tick: 0,
        })
    }

    /// Builds the deterministic static pipeline: a fixed `capacity` admission
    /// bound with no controller. Used when adaptation is disabled via
    /// `OC_RSYNC_ADAPTIVE_QUEUE`.
    fn with_static_capacity(capacity: usize, bypass: bool) -> Self {
        let (work_tx, work_rx) = work_queue::bounded_with_capacity(capacity);
        let consumer = if bypass {
            DeltaConsumer::spawn_bypass(work_rx)
        } else {
            DeltaConsumer::spawn(work_rx, capacity)
        };
        Self {
            next_sequence: 0,
            work_tx: Some(work_tx),
            consumer: Some(consumer),
            controller: None,
            submits_since_tick: 0,
        }
    }

    /// Advances the adaptive controller once per [`CONTROLLER_TICK_WINDOW`]
    /// submits. A no-op on the static path.
    ///
    /// Ticking from the single producer is deliberate: the producer's own
    /// blocking on admission is the backpressure signal, recorded in the
    /// semaphore counters and folded into the depth by
    /// [`AdaptiveQueueController::tick`] on the next submit after it unblocks.
    fn maybe_tick(&mut self) {
        if self.controller.is_none() {
            return;
        }
        self.submits_since_tick += 1;
        if self.submits_since_tick >= CONTROLLER_TICK_WINDOW {
            self.submits_since_tick = 0;
            if let Some(controller) = self.controller.as_mut() {
                controller.tick();
            }
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
            .map_err(|_| io::Error::other("parallel pipeline consumer thread has shut down"))?;
        self.maybe_tick();
        Ok(())
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
