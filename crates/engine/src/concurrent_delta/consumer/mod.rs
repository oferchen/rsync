//! Ordered consumer for the concurrent delta pipeline.
//!
//! [`DeltaConsumer`] bridges the parallel dispatch phase
//! ([`WorkQueueReceiver`]) with the
//! ordered consumption phase (receiver pipeline). It spawns a consumer
//! thread that drains [`DeltaWork`](super::types::DeltaWork) items from the
//! work queue via
//! [`drain_parallel`](super::work_queue::WorkQueueReceiver::drain_parallel),
//! feeds each [`DeltaResult`] into a
//! [`ReorderBuffer`](super::reorder::ReorderBuffer), and exposes an iterator
//! that yields results strictly in sequence order.
//!
//! # Architecture
//!
//! ```text
//! WorkQueueReceiver
//!     |
//!     v  drain_parallel_into(dispatch, stream_tx)
//! rayon workers (parallel)
//!     |
//!     v  crossbeam_channel::Sender<DeltaResult> (streaming, bounded)
//! delta-reorder thread
//!     |
//!     v  ReorderBuffer (incremental insert + drain)
//!     |
//!     v  mpsc channel (in sequence order)
//! DeltaConsumer::iter()
//!     |
//!     v  consumer (receiver pipeline)
//! ```
//!
//! Two background threads provide pipeline overlap:
//! - **delta-drain**: Runs `drain_parallel_into` inside `rayon::scope`,
//!   streaming each completed result through a bounded channel.
//! - **delta-reorder**: Receives streamed results incrementally, inserts
//!   them into the reorder buffer, and forwards contiguous in-order runs
//!   to the output channel.
//!
//! This architecture allows delta computation and disk writes to overlap -
//! the reorder thread processes results while workers continue computing
//! deltas for remaining files. The bounded stream channel provides
//! backpressure when the reorder thread falls behind.
//!
//! # Upstream Reference
//!
//! Upstream rsync's `recv_files()` in `receiver.c` processes files
//! sequentially. This consumer restores that ordering after parallel
//! dispatch so downstream processing (checksum verification, temp-file
//! commit, metadata application) sees files in file-list order.
//!
//! # Module Layout
//!
//! | Submodule | Role |
//! |-----------|------|
//! | `spawn` | Private spawn machinery: `ReorderMode` selector and the shared `spawn_inner` plumbing |
//! | `loops` | Background-thread reorder loops for the bare and spillable backends |
//! | `tests` | Integration tests (only built under `#[cfg(test)]`) |

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;

use super::config::ConcurrentDeltaConfig;
use super::reorder::Metrics as ReorderMetrics;
use super::types::DeltaResult;
use super::work_queue::WorkQueueReceiver;

mod loops;
mod spawn;

#[cfg(test)]
mod tests;

use spawn::{ReorderMode, spawn_inner};

/// Snapshot of consumer-side counters surfaced for diagnostics.
///
/// Mirrors [`SpillStats`](super::spill::SpillStats) for operators that only
/// hold a [`DeltaConsumer`] handle. All counters are cumulative across the
/// consumer's lifetime; values are zero when the spill layer is not engaged.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DeltaConsumerStats {
    /// Cumulative count of items written to the spill tempfile.
    ///
    /// Always zero when the consumer was spawned without a spill threshold.
    pub spill_events: u64,
    /// Cumulative count of `spill_excess` calls that wrote at least one
    /// record. Granularity-invariant: a single call increments this counter
    /// by exactly one even when [`SpillGranularity::PerItem`](super::spill::SpillGranularity::PerItem)
    /// produces many on-disk records. Always zero when the consumer was
    /// spawned without a spill threshold. ROB-2 (#3667) surfaces this
    /// through the consumer-side stats so operators can observe normal-
    /// operation spill rate without instrumenting the spillable buffer
    /// directly.
    pub spill_activations: u64,
    /// Cumulative count of ordering-fallback inserts performed by the
    /// underlying [`ReorderBuffer`](super::reorder::ReorderBuffer) when the
    /// ring saturated while `next_expected` was still missing.
    ///
    /// Non-zero values flag periods where the consumer broke its capacity
    /// bound to keep the pipeline alive. Operators should treat sustained
    /// growth here as a signal to raise the reorder capacity or enable the
    /// spill backend. This is an OC-rsync diagnostic extension - upstream
    /// rsync has no equivalent because its delta loop is sequential.
    pub force_inserts: u64,
}

/// Ordered consumer that drains a [`WorkQueueReceiver`] in parallel and
/// yields [`DeltaResult`] items in sequence order.
///
/// Created via [`DeltaConsumer::spawn`], which launches a background thread
/// that runs [`WorkQueueReceiver::drain_parallel`] to process work items
/// concurrently, then feeds results through a
/// [`ReorderBuffer`](super::reorder::ReorderBuffer) for in-order delivery
/// over an internal channel.
///
/// # Lifecycle
///
/// 1. Call [`spawn`](Self::spawn) with a `WorkQueueReceiver` and reorder capacity.
/// 2. Iterate over results via [`iter`](Self::iter) or [`into_iter`](Self::into_iter).
/// 3. The iterator yields `None` (terminates) once all results have been
///    delivered and the background thread has finished.
/// 4. Call [`join`](Self::join) to wait for the background thread and
///    propagate any panics.
///
/// # Example
///
/// ```rust,no_run
/// use engine::concurrent_delta::work_queue;
/// use engine::concurrent_delta::consumer::DeltaConsumer;
/// use engine::concurrent_delta::DeltaWork;
/// use std::path::PathBuf;
///
/// let (tx, rx) = work_queue::bounded();
///
/// // Producer thread
/// std::thread::spawn(move || {
///     for i in 0..100u32 {
///         let work = DeltaWork::whole_file(i, PathBuf::from("/dst"), 64)
///             .with_sequence(u64::from(i));
///         tx.send(work).unwrap();
///     }
/// });
///
/// let consumer = DeltaConsumer::spawn(rx, 128);
/// for result in consumer.iter() {
///     assert!(result.is_success());
/// }
/// consumer.join().unwrap();
/// ```
pub struct DeltaConsumer {
    /// Receives in-order results from the background thread.
    pub(super) result_rx: mpsc::Receiver<DeltaResult>,
    /// Handle to the background consumer thread.
    pub(super) handle: Option<JoinHandle<()>>,
    /// Live snapshot of the reorder buffer metrics, updated by the
    /// background thread after every force_insert and every non-empty
    /// drain. Cheap to poll from the caller via [`Self::metrics`].
    pub(super) metrics: Arc<Mutex<ReorderMetrics>>,
    /// Shared counter incremented by the reorder thread on each spill-to-disk
    /// event. Exposed via [`DeltaConsumer::stats`].
    pub(super) spill_events: Arc<AtomicU64>,
    /// Shared counter incremented by the reorder thread on each
    /// `spill_excess` call that wrote at least one record. ROB-2 (#3667)
    /// surfaces this granularity-invariant counter through
    /// [`DeltaConsumer::stats`] so operators see normal-operation spill
    /// pressure without compensating for `PerItem` vs `WholeBatch`
    /// record fan-out.
    pub(super) spill_activations: Arc<AtomicU64>,
    /// Shared handle aliasing the underlying [`ReorderBuffer`](super::reorder::ReorderBuffer)
    /// `force_insert` counter. The reorder buffer updates this atomic
    /// directly, so [`DeltaConsumer::stats`] reflects the latest value
    /// without locking the metrics `Mutex`.
    pub(super) force_inserts: Arc<AtomicU64>,
}

impl DeltaConsumer {
    /// Spawns background threads that drain the work queue in parallel
    /// and deliver results in sequence order with pipeline overlap.
    ///
    /// Two threads are spawned:
    /// - **delta-drain**: Runs [`WorkQueueReceiver::drain_parallel_into`] to
    ///   process work items via the rayon thread pool, streaming each result
    ///   through an internal channel as soon as its worker completes.
    /// - **delta-reorder**: Receives streamed results, inserts them into a
    ///   [`ReorderBuffer`](super::reorder::ReorderBuffer), and forwards the
    ///   contiguous in-order run to the consumer's output channel.
    ///
    /// This architecture enables pipeline overlap: delta computation continues
    /// while previously completed results are reordered and written to disk.
    /// The bounded stream channel provides backpressure - if reordering falls
    /// behind, delta workers block rather than accumulating unbounded results.
    ///
    /// `reorder_capacity` sets the maximum number of out-of-order results
    /// the reorder buffer will hold. A good default is the total number of
    /// expected items, or at least `2 * rayon::current_num_threads()`.
    ///
    /// # Panics
    ///
    /// Panics if `reorder_capacity` is zero and `bypass_reorder` is `false`.
    #[must_use]
    pub fn spawn(rx: WorkQueueReceiver, reorder_capacity: usize) -> Self {
        spawn_inner(
            rx,
            ReorderMode::Bare {
                capacity: reorder_capacity,
            },
        )
    }

    /// Spawns background threads that drain the work queue in parallel
    /// and deliver results in arrival order, bypassing reordering.
    ///
    /// Identical to [`spawn`](Self::spawn) except the internal
    /// [`ReorderBuffer`](super::reorder::ReorderBuffer) operates in
    /// passthrough mode. Items are forwarded to the consumer in the order
    /// they complete rather than submission order. This eliminates reorder
    /// overhead when strict file-list ordering is unnecessary - for example,
    /// when `--delay-updates` is off and files are committed immediately.
    #[must_use]
    pub fn spawn_bypass(rx: WorkQueueReceiver) -> Self {
        spawn_inner(rx, ReorderMode::Bypass)
    }

    /// Spawns background threads honouring a runtime
    /// [`ConcurrentDeltaConfig`].
    ///
    /// When `cfg.spill_policy.threshold_bytes` is `Some`, the reorder thread
    /// is backed by a [`SpillableReorderBuffer`](super::spill::SpillableReorderBuffer)
    /// instead of the bare ring buffer. Items past the in-memory byte
    /// threshold spill to a tempfile; they are reloaded transparently as the
    /// delivery cursor reaches them. When the threshold is `None`, behaviour
    /// matches [`spawn`](Self::spawn).
    ///
    /// `reorder_capacity` is the in-memory ring window. A good default is the
    /// total number of expected items, or at least
    /// `2 * rayon::current_num_threads()`.
    ///
    /// # Panics
    ///
    /// Panics if `reorder_capacity` is zero.
    #[must_use]
    pub fn spawn_with_config(
        rx: WorkQueueReceiver,
        reorder_capacity: usize,
        cfg: ConcurrentDeltaConfig,
    ) -> Self {
        spawn_inner(rx, ReorderMode::from_config(reorder_capacity, cfg))
    }

    /// Returns a snapshot of consumer-side diagnostic counters.
    ///
    /// Exposes the cumulative spill-to-disk event count and the cumulative
    /// `force_insert` count from the background reorder thread. Safe to
    /// call from any thread while the consumer is running; the counters
    /// are updated lock-free.
    #[must_use]
    pub fn stats(&self) -> DeltaConsumerStats {
        DeltaConsumerStats {
            spill_events: self.spill_events.load(Ordering::Relaxed),
            spill_activations: self.spill_activations.load(Ordering::Relaxed),
            force_inserts: self.force_inserts.load(Ordering::Relaxed),
        }
    }

    /// Returns a snapshot of the underlying [`ReorderBuffer`](super::reorder::ReorderBuffer)
    /// metrics.
    ///
    /// The snapshot is updated by the background thread after every
    /// `force_insert` and every non-empty drain iteration. Callers may
    /// poll this method at any cadence; it never blocks the delta
    /// pipeline.
    ///
    /// On the unlikely path where the metrics lock is poisoned (a panic
    /// in the consumer thread mid-update), a zero-initialised snapshot is
    /// returned. The panic itself is propagated through
    /// [`Self::join`].
    #[must_use]
    pub fn metrics(&self) -> ReorderMetrics {
        self.metrics.lock().map(|g| *g).unwrap_or_default()
    }

    /// Tries to receive the next in-order result without blocking.
    ///
    /// Returns `Some(result)` if a result is immediately available in the
    /// channel, or `None` if no results are ready yet. Unlike
    /// [`iter`](Self::iter), this method never blocks the caller.
    ///
    /// Useful for polling from a pipeline loop where blocking would stall
    /// the producer.
    pub fn try_recv(&self) -> Option<DeltaResult> {
        self.result_rx.try_recv().ok()
    }

    /// Returns an iterator that yields results in sequence order.
    ///
    /// The iterator blocks waiting for the next result and terminates when
    /// all results have been delivered (the background thread finishes and
    /// the internal channel closes).
    #[must_use]
    pub fn iter(&self) -> DeltaConsumerIter<'_> {
        DeltaConsumerIter {
            rx: &self.result_rx,
        }
    }

    /// Waits for the background thread to finish.
    ///
    /// Returns `Ok(())` if the thread completed normally, or `Err` if it
    /// panicked. Should be called after the iterator is fully consumed to
    /// ensure clean shutdown and panic propagation.
    ///
    /// # Errors
    ///
    /// Returns the panic payload if the background thread panicked.
    pub fn join(mut self) -> Result<(), Box<dyn std::any::Any + Send>> {
        if let Some(handle) = self.handle.take() {
            handle.join()
        } else {
            Ok(())
        }
    }
}

impl IntoIterator for DeltaConsumer {
    type Item = DeltaResult;
    type IntoIter = DeltaConsumerIntoIter;

    fn into_iter(self) -> DeltaConsumerIntoIter {
        DeltaConsumerIntoIter {
            rx: self.result_rx,
            _handle: self.handle,
        }
    }
}

/// Borrowing iterator over in-order [`DeltaResult`] items from a [`DeltaConsumer`].
///
/// Created by [`DeltaConsumer::iter`]. Blocks on each call to `next()` until
/// the next in-order result is available or the channel closes.
pub struct DeltaConsumerIter<'a> {
    rx: &'a mpsc::Receiver<DeltaResult>,
}

impl Iterator for DeltaConsumerIter<'_> {
    type Item = DeltaResult;

    fn next(&mut self) -> Option<DeltaResult> {
        self.rx.recv().ok()
    }
}

/// Owning iterator over in-order [`DeltaResult`] items from a [`DeltaConsumer`].
///
/// Created by [`DeltaConsumer::into_iter`]. Takes ownership of the consumer,
/// ensuring the background thread handle is kept alive for the iterator's
/// lifetime.
pub struct DeltaConsumerIntoIter {
    rx: mpsc::Receiver<DeltaResult>,
    /// Kept alive to prevent the background thread from being detached
    /// before the iterator is consumed.
    _handle: Option<JoinHandle<()>>,
}

impl Iterator for DeltaConsumerIntoIter {
    type Item = DeltaResult;

    fn next(&mut self) -> Option<DeltaResult> {
        self.rx.recv().ok()
    }
}
