//! Ordered consumer for the concurrent delta pipeline.
//!
//! [`DeltaConsumer`] bridges the parallel dispatch phase ([`WorkQueue`]) with
//! the ordered consumption phase (receiver pipeline). It spawns a consumer
//! thread that drains [`DeltaWork`] items from the work queue via
//! [`drain_parallel`], feeds each [`DeltaResult`] into a [`ReorderBuffer`],
//! and exposes an iterator that yields results strictly in sequence order.
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

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::{self, JoinHandle};
use std::time::Instant;

use super::config::ConcurrentDeltaConfig;
use super::reorder::{Metrics as ReorderMetrics, ReorderBuffer};
use super::spill::{SpillError, SpillableReorderBuffer};
use super::strategy;
use super::types::DeltaResult;
use super::work_queue::WorkQueueReceiver;

/// Selects the reorder backend driven by [`DeltaConsumer::spawn_inner`].
///
/// Encoded as an enum rather than two booleans so future modes (e.g.
/// hybrid memory + spill with adaptive sizing) can extend the variant set
/// without churning the call sites.
enum ReorderMode {
    /// Bypass mode: passthrough FIFO, no sequence reordering.
    Bypass,
    /// Bare in-memory ring with the historical doubling fallback on overflow.
    Bare { capacity: usize },
    /// Bounded-memory ring with spill-to-tempfile when the byte threshold is
    /// exceeded. `dir` is `None` for the default `SpooledTempFile` backend.
    Spillable {
        capacity: usize,
        threshold: usize,
        dir: Option<PathBuf>,
    },
}

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
}

/// Ordered consumer that drains a [`WorkQueueReceiver`] in parallel and
/// yields [`DeltaResult`] items in sequence order.
///
/// Created via [`DeltaConsumer::spawn`], which launches a background thread
/// that runs [`WorkQueueReceiver::drain_parallel`] to process work items
/// concurrently, then feeds results through a [`ReorderBuffer`] for in-order
/// delivery over an internal channel.
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
    result_rx: mpsc::Receiver<DeltaResult>,
    /// Handle to the background consumer thread.
    handle: Option<JoinHandle<()>>,
    /// Live snapshot of the reorder buffer metrics, updated by the
    /// background thread after every force_insert and every non-empty
    /// drain. Cheap to poll from the caller via [`Self::metrics`].
    metrics: Arc<Mutex<ReorderMetrics>>,
    /// Shared counter incremented by the reorder thread on each spill-to-disk
    /// event. Exposed via [`DeltaConsumer::stats`].
    spill_events: Arc<AtomicU64>,
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
    ///   [`ReorderBuffer`], and forwards the contiguous in-order run to the
    ///   consumer's output channel.
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
        Self::spawn_inner(
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
    /// [`ReorderBuffer`] operates in passthrough mode. Items are forwarded
    /// to the consumer in the order they complete rather than submission
    /// order. This eliminates reorder overhead when strict file-list
    /// ordering is unnecessary - for example, when `--delay-updates` is
    /// off and files are committed immediately.
    #[must_use]
    pub fn spawn_bypass(rx: WorkQueueReceiver) -> Self {
        Self::spawn_inner(rx, ReorderMode::Bypass)
    }

    /// Spawns background threads honouring a runtime
    /// [`ConcurrentDeltaConfig`].
    ///
    /// When `cfg.spill_threshold_bytes` is `Some`, the reorder thread is
    /// backed by a [`SpillableReorderBuffer`] instead of the bare ring
    /// buffer. Items past the in-memory byte threshold spill to a tempfile;
    /// they are reloaded transparently as the delivery cursor reaches them.
    /// When the threshold is `None`, behaviour matches [`spawn`](Self::spawn).
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
        let mode = match cfg.spill_threshold_bytes {
            Some(threshold) => ReorderMode::Spillable {
                capacity: reorder_capacity,
                threshold: usize::try_from(threshold).unwrap_or(usize::MAX),
                dir: cfg.spill_dir,
            },
            None => ReorderMode::Bare {
                capacity: reorder_capacity,
            },
        };
        Self::spawn_inner(rx, mode)
    }

    /// Internal spawn implementation shared by all factory paths.
    fn spawn_inner(rx: WorkQueueReceiver, mode: ReorderMode) -> Self {
        let (result_tx, result_rx) = mpsc::channel();
        let spill_events = Arc::new(AtomicU64::new(0));

        // Bounded channel between drain and reorder threads. Capacity matches
        // reorder buffer so workers can stay ahead without unbounded buffering.
        let stream_capacity = match &mode {
            ReorderMode::Bypass => rayon::current_num_threads() * 2,
            ReorderMode::Bare { capacity } | ReorderMode::Spillable { capacity, .. } => {
                (*capacity).max(rayon::current_num_threads() * 2)
            }
        };
        let (stream_tx, stream_rx) = crossbeam_channel::bounded::<DeltaResult>(stream_capacity);

        // Thread 1: runs rayon::scope, streaming results as workers complete.
        let drain_handle = thread::Builder::new()
            .name("delta-drain".to_string())
            .spawn(move || {
                rx.drain_parallel_into(|work| strategy::dispatch(&work), stream_tx);
            })
            .expect("failed to spawn delta-drain thread");

        // Shared metrics snapshot updated by the reorder thread; callers can
        // poll it through `DeltaConsumer::metrics` without blocking the
        // delta pipeline.
        let metrics = Arc::new(Mutex::new(ReorderMetrics::default()));
        let metrics_thread = Arc::clone(&metrics);

        // Thread 2: receives streamed results, reorders (or passes through),
        // and forwards to the consumer channel.
        let spill_events_thread = Arc::clone(&spill_events);
        let handle = thread::Builder::new()
            .name("delta-reorder".to_string())
            .spawn(move || {
                match mode {
                    ReorderMode::Bypass => {
                        run_bare_loop(
                            stream_rx,
                            &result_tx,
                            ReorderBuffer::passthrough(),
                            &metrics_thread,
                        );
                    }
                    ReorderMode::Bare { capacity } => {
                        run_bare_loop(
                            stream_rx,
                            &result_tx,
                            ReorderBuffer::new(capacity),
                            &metrics_thread,
                        );
                    }
                    ReorderMode::Spillable {
                        capacity,
                        threshold,
                        dir,
                    } => match build_spillable(capacity, threshold, dir) {
                        Ok(buf) => {
                            run_spillable_loop(stream_rx, &result_tx, buf, &spill_events_thread);
                        }
                        Err(e) => {
                            // Construction failed (e.g., spill dir cannot be
                            // created). Surface as a single failed result so
                            // the receiver maps to exit code 11 and aborts.
                            let _ = result_tx.send(DeltaResult::failed(
                                0u32,
                                format!("spill backend unavailable: {e}"),
                            ));
                        }
                    },
                }

                // Wait for drain thread to finish (propagates panics).
                let _ = drain_handle.join();
            })
            .expect("failed to spawn delta-reorder thread");

        Self {
            result_rx,
            handle: Some(handle),
            metrics,
            spill_events,
        }
    }

    /// Returns a snapshot of consumer-side diagnostic counters.
    ///
    /// Currently exposes the cumulative spill-to-disk event count from the
    /// background reorder thread. Safe to call from any thread while the
    /// consumer is running; the counters are updated lock-free.
    #[must_use]
    pub fn stats(&self) -> DeltaConsumerStats {
        DeltaConsumerStats {
            spill_events: self.spill_events.load(Ordering::Relaxed),
        }
    }

    /// Returns a snapshot of the underlying [`ReorderBuffer`] metrics.
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

/// Constructs a [`SpillableReorderBuffer`] backed by the configured backend.
fn build_spillable(
    capacity: usize,
    threshold: usize,
    dir: Option<PathBuf>,
) -> std::io::Result<SpillableReorderBuffer<DeltaResult>> {
    match dir {
        Some(d) => SpillableReorderBuffer::with_spill_dir(capacity, threshold, d),
        None => Ok(SpillableReorderBuffer::new(capacity, threshold)),
    }
}

/// Reorder loop for the bare [`ReorderBuffer`] backend (passthrough or
/// ring-buffer mode). Mirrors the historical control flow: drain ready items
/// to free space, force-insert if the buffer is full and the head is missing.
/// Publishes a metrics snapshot after every `force_insert` and every non-empty
/// drain so callers can poll [`DeltaConsumer::metrics`] without locking the
/// pipeline.
fn run_bare_loop(
    stream_rx: crossbeam_channel::Receiver<DeltaResult>,
    result_tx: &mpsc::Sender<DeltaResult>,
    mut reorder: ReorderBuffer<DeltaResult>,
    metrics: &Arc<Mutex<ReorderMetrics>>,
) {
    let mut last_drain_at: Option<Instant> = None;
    let publish = |reorder: &ReorderBuffer<DeltaResult>| {
        if let Ok(mut guard) = metrics.lock() {
            *guard = reorder.metrics();
        }
    };

    for result in stream_rx {
        while reorder.insert(result.sequence(), result.clone()).is_err() {
            match drain_and_record(&mut reorder, result_tx, &mut last_drain_at) {
                DrainOutcome::Disconnected => return,
                DrainOutcome::Empty => {
                    // Buffer full but next_expected is not buffered.
                    // Force insert to break the deadlock.
                    reorder.force_insert(result.sequence(), result.clone());
                    publish(&reorder);
                    break;
                }
                DrainOutcome::Forwarded(_) => {
                    publish(&reorder);
                }
            }
        }

        match drain_and_record(&mut reorder, result_tx, &mut last_drain_at) {
            DrainOutcome::Disconnected => return,
            DrainOutcome::Empty => {}
            DrainOutcome::Forwarded(_) => publish(&reorder),
        }
    }

    match drain_and_record(&mut reorder, result_tx, &mut last_drain_at) {
        DrainOutcome::Disconnected => return,
        DrainOutcome::Empty => {}
        DrainOutcome::Forwarded(_) => publish(&reorder),
    }
    // Ensure the final snapshot reflects steady-state counters even if the
    // last operation was a non-recording drain.
    publish(&reorder);
}

/// Reorder loop for the bounded-memory [`SpillableReorderBuffer`] backend.
///
/// The spill layer handles overflow internally: when the byte threshold is
/// exceeded, the buffer serialises the oldest (highest-sequence) items to a
/// tempfile and reloads them transparently on drain. The legacy
/// "force_insert as deadlock breaker" branch is gone - capacity exhaustion
/// becomes a spill rather than unbounded ring growth.
///
/// Spill-side I/O failures (ENOSPC, missing temp directory, encoder error)
/// are mapped to a [`DeltaResult::failed`] for the offending sequence, which
/// the receiver maps to upstream rsync exit code 11 (`FileIo`) so the
/// transfer aborts with the same semantics as a direct I/O failure.
fn run_spillable_loop(
    stream_rx: crossbeam_channel::Receiver<DeltaResult>,
    result_tx: &mpsc::Sender<DeltaResult>,
    mut reorder: SpillableReorderBuffer<DeltaResult>,
    spill_events: &Arc<AtomicU64>,
) {
    let mut prev_spill = reorder.spill_stats().spill_events;

    for result in stream_rx {
        let ndx = result.ndx();
        loop {
            match reorder.insert(result.sequence(), result.clone()) {
                Ok(()) => break,
                Err(SpillError::Capacity(_)) => {
                    // Drain ready items first to free a ring slot.
                    let mut drained_any = false;
                    match reorder.drain_ready() {
                        Ok(items) => {
                            for ready in items {
                                drained_any = true;
                                if result_tx.send(ready).is_err() {
                                    return;
                                }
                            }
                        }
                        Err(e) => {
                            let _ = result_tx.send(DeltaResult::failed(
                                ndx,
                                format!("spill reload failed: {e}"),
                            ));
                            return;
                        }
                    }
                    if !drained_any {
                        // The head is missing and the ring is full. Force the
                        // insert; the spill layer keeps memory bounded by
                        // displacing higher-sequence items to disk.
                        if let Err(e) = reorder.force_insert(result.sequence(), result.clone()) {
                            let _ = result_tx.send(DeltaResult::failed(
                                ndx,
                                format!("spill force_insert failed: {e}"),
                            ));
                            return;
                        }
                        publish_spill_events(&reorder, spill_events, &mut prev_spill);
                        break;
                    }
                }
                Err(SpillError::Io(e)) => {
                    let _ = result_tx
                        .send(DeltaResult::failed(ndx, format!("spill write failed: {e}")));
                    return;
                }
            }
        }
        publish_spill_events(&reorder, spill_events, &mut prev_spill);

        match reorder.drain_ready() {
            Ok(items) => {
                for ready in items {
                    if result_tx.send(ready).is_err() {
                        return;
                    }
                }
            }
            Err(e) => {
                let _ = result_tx.send(DeltaResult::failed(
                    ndx,
                    format!("spill reload failed: {e}"),
                ));
                return;
            }
        }
    }

    // Stream closed - drain whatever is left, including spilled entries.
    loop {
        match reorder.drain_ready() {
            Ok(items) if items.is_empty() => break,
            Ok(items) => {
                for ready in items {
                    if result_tx.send(ready).is_err() {
                        return;
                    }
                }
            }
            Err(e) => {
                let _ = result_tx.send(DeltaResult::failed(
                    0u32,
                    format!("spill reload failed: {e}"),
                ));
                return;
            }
        }
    }
    publish_spill_events(&reorder, spill_events, &mut prev_spill);
}

/// Republishes the cumulative spill-to-disk event counter so callers can
/// observe progress via [`DeltaConsumer::stats`].
fn publish_spill_events(
    reorder: &SpillableReorderBuffer<DeltaResult>,
    spill_events: &Arc<AtomicU64>,
    prev: &mut u64,
) {
    let current = reorder.spill_stats().spill_events;
    if current != *prev {
        spill_events.store(current, Ordering::Relaxed);
        *prev = current;
    }
}

/// Outcome of one [`drain_and_record`] call.
enum DrainOutcome {
    /// No items were ready to drain.
    Empty,
    /// One or more items were forwarded to the consumer channel.
    Forwarded(usize),
    /// The output channel is closed; the caller must abort.
    Disconnected,
}

/// Drains the contiguous in-order run from `reorder`, forwards each item to
/// `result_tx`, and records the batch size plus the wall-clock pause since
/// the previous non-empty drain.
///
/// Bypasses the metrics path on empty drains so the histograms reflect only
/// the work actually performed by the consumer thread.
fn drain_and_record(
    reorder: &mut ReorderBuffer<DeltaResult>,
    result_tx: &mpsc::Sender<DeltaResult>,
    last_drain_at: &mut Option<Instant>,
) -> DrainOutcome {
    let mut count = 0usize;
    while let Some(ready) = reorder.next_in_order() {
        if result_tx.send(ready).is_err() {
            return DrainOutcome::Disconnected;
        }
        count += 1;
    }
    if count == 0 {
        return DrainOutcome::Empty;
    }
    let now = Instant::now();
    if let Some(prev) = *last_drain_at {
        reorder.record_drain_pause(now.saturating_duration_since(prev));
    }
    reorder.record_drain_batch(count);
    *last_drain_at = Some(now);
    DrainOutcome::Forwarded(count)
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

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::concurrent_delta::DeltaWork;
    use crate::concurrent_delta::work_queue;

    /// Helper: sends `count` whole-file work items with sequential sequence numbers.
    fn spawn_producer(count: u32) -> (work_queue::WorkQueueSender, work_queue::WorkQueueReceiver) {
        work_queue::bounded_with_capacity(count.max(1) as usize)
    }

    fn send_items(tx: &work_queue::WorkQueueSender, count: u32) {
        for i in 0..count {
            let work = DeltaWork::whole_file(i, PathBuf::from(format!("/dst/{i}")), 64)
                .with_sequence(u64::from(i));
            tx.send(work).unwrap();
        }
    }

    #[test]
    fn delivers_results_in_sequence_order() {
        let (tx, rx) = spawn_producer(50);
        let producer = std::thread::spawn(move || send_items(&tx, 50));

        let consumer = DeltaConsumer::spawn(rx, 64);
        let results: Vec<DeltaResult> = consumer.iter().collect();
        producer.join().unwrap();

        assert_eq!(results.len(), 50);
        for (i, r) in results.iter().enumerate() {
            assert_eq!(r.sequence(), i as u64, "out of order at position {i}");
            assert!(r.is_success());
        }
    }

    #[test]
    fn into_iter_yields_all_results() {
        let (tx, rx) = spawn_producer(30);
        let producer = std::thread::spawn(move || send_items(&tx, 30));

        let consumer = DeltaConsumer::spawn(rx, 64);
        let results: Vec<DeltaResult> = consumer.into_iter().collect();
        producer.join().unwrap();

        assert_eq!(results.len(), 30);
        for (i, r) in results.iter().enumerate() {
            assert_eq!(r.sequence(), i as u64);
        }
    }

    #[test]
    fn empty_queue_yields_no_results() {
        let (tx, rx) = spawn_producer(1);
        drop(tx); // Close immediately - no items sent.

        let consumer = DeltaConsumer::spawn(rx, 8);
        let results: Vec<DeltaResult> = consumer.iter().collect();

        assert!(results.is_empty());
    }

    #[test]
    fn single_item() {
        let (tx, rx) = spawn_producer(1);
        tx.send(DeltaWork::whole_file(42, PathBuf::from("/dst/single"), 128).with_sequence(0))
            .unwrap();
        drop(tx);

        let consumer = DeltaConsumer::spawn(rx, 4);
        let results: Vec<DeltaResult> = consumer.iter().collect();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].ndx().get(), 42);
        assert_eq!(results[0].sequence(), 0);
        assert_eq!(results[0].bytes_written(), 128);
    }

    #[test]
    fn join_succeeds_after_drain() {
        let (tx, rx) = spawn_producer(10);
        let producer = std::thread::spawn(move || send_items(&tx, 10));

        let consumer = DeltaConsumer::spawn(rx, 16);
        let results: Vec<DeltaResult> = consumer.iter().collect();
        producer.join().unwrap();

        assert_eq!(results.len(), 10);
    }

    #[test]
    fn join_after_into_iter() {
        let (tx, rx) = spawn_producer(5);
        let producer = std::thread::spawn(move || send_items(&tx, 5));

        let consumer = DeltaConsumer::spawn(rx, 16);
        for r in consumer.iter() {
            assert!(r.is_success());
        }
        consumer.join().unwrap();
        producer.join().unwrap();
    }

    #[test]
    fn large_batch_in_order() {
        let count = 500u32;
        let (tx, rx) = work_queue::bounded_with_capacity(32);

        let producer = std::thread::spawn(move || {
            for i in 0..count {
                let work =
                    DeltaWork::whole_file(i, PathBuf::from("/dst"), 64).with_sequence(u64::from(i));
                tx.send(work).unwrap();
            }
        });

        let consumer = DeltaConsumer::spawn(rx, count as usize);
        let results: Vec<DeltaResult> = consumer.into_iter().collect();
        producer.join().unwrap();

        assert_eq!(results.len(), count as usize);
        for (i, r) in results.iter().enumerate() {
            assert_eq!(
                r.sequence(),
                i as u64,
                "sequence mismatch at position {i}: expected {i}, got {}",
                r.sequence()
            );
        }
    }

    #[test]
    fn delta_work_items_processed_correctly() {
        let (tx, rx) = work_queue::bounded_with_capacity(8);

        let producer = std::thread::spawn(move || {
            // Mix of whole-file and delta items.
            tx.send(DeltaWork::whole_file(0, PathBuf::from("/dst/a"), 1024).with_sequence(0))
                .unwrap();
            tx.send(
                DeltaWork::delta(
                    1,
                    PathBuf::from("/dst/b"),
                    PathBuf::from("/basis/b"),
                    2048,
                    800,
                    1248,
                )
                .with_sequence(1),
            )
            .unwrap();
            tx.send(DeltaWork::whole_file(2, PathBuf::from("/dst/c"), 512).with_sequence(2))
                .unwrap();
        });

        let consumer = DeltaConsumer::spawn(rx, 8);
        let results: Vec<DeltaResult> = consumer.into_iter().collect();
        producer.join().unwrap();

        assert_eq!(results.len(), 3);

        // First: whole-file, all literal.
        assert_eq!(results[0].ndx().get(), 0);
        assert_eq!(results[0].literal_bytes(), 1024);
        assert_eq!(results[0].matched_bytes(), 0);

        // Second: delta, mixed literal/matched.
        assert_eq!(results[1].ndx().get(), 1);
        assert_eq!(results[1].literal_bytes(), 800);
        assert_eq!(results[1].matched_bytes(), 1248);

        // Third: whole-file, all literal.
        assert_eq!(results[2].ndx().get(), 2);
        assert_eq!(results[2].literal_bytes(), 512);
        assert_eq!(results[2].matched_bytes(), 0);
    }

    #[test]
    fn small_reorder_capacity_still_delivers_all() {
        // Reorder capacity smaller than total items - the consumer must
        // drain ready items to free capacity before inserting more.
        let count = 20u32;
        let (tx, rx) = work_queue::bounded_with_capacity(4);

        let producer = std::thread::spawn(move || {
            for i in 0..count {
                let work =
                    DeltaWork::whole_file(i, PathBuf::from("/dst"), 64).with_sequence(u64::from(i));
                tx.send(work).unwrap();
            }
        });

        let consumer = DeltaConsumer::spawn(rx, 4);
        let results: Vec<DeltaResult> = consumer.into_iter().collect();
        producer.join().unwrap();

        assert_eq!(results.len(), count as usize);
        for (i, r) in results.iter().enumerate() {
            assert_eq!(r.sequence(), i as u64);
        }
    }

    #[test]
    fn drop_consumer_before_drain_does_not_hang() {
        let (tx, rx) = work_queue::bounded_with_capacity(8);

        let producer = std::thread::spawn(move || {
            for i in 0..5u32 {
                let work =
                    DeltaWork::whole_file(i, PathBuf::from("/dst"), 64).with_sequence(u64::from(i));
                // Send may fail if consumer is dropped - that's ok.
                let _ = tx.send(work);
            }
        });

        let consumer = DeltaConsumer::spawn(rx, 16);
        drop(consumer);
        producer.join().unwrap();
    }

    #[test]
    fn ndx_values_preserved_through_pipeline() {
        let (tx, rx) = work_queue::bounded_with_capacity(8);

        let producer = std::thread::spawn(move || {
            // Use non-sequential NDX values to verify they survive the pipeline.
            let ndx_values = [100, 42, 7, 999, 0];
            for (seq, &ndx) in ndx_values.iter().enumerate() {
                let work =
                    DeltaWork::whole_file(ndx, PathBuf::from("/dst"), 64).with_sequence(seq as u64);
                tx.send(work).unwrap();
            }
        });

        let consumer = DeltaConsumer::spawn(rx, 8);
        let results: Vec<DeltaResult> = consumer.into_iter().collect();
        producer.join().unwrap();

        assert_eq!(results.len(), 5);
        // Results are in sequence order, so NDX values follow submission order.
        assert_eq!(results[0].ndx().get(), 100);
        assert_eq!(results[1].ndx().get(), 42);
        assert_eq!(results[2].ndx().get(), 7);
        assert_eq!(results[3].ndx().get(), 999);
        assert_eq!(results[4].ndx().get(), 0);
    }

    #[test]
    fn try_recv_returns_none_when_no_results_ready() {
        let (tx, rx) = work_queue::bounded_with_capacity(8);
        let consumer = DeltaConsumer::spawn(rx, 16);

        assert!(consumer.try_recv().is_none());

        // Send items so the consumer thread can finish.
        send_items(&tx, 3);
        drop(tx);

        let results: Vec<DeltaResult> = consumer.iter().collect();
        assert_eq!(results.len(), 3);
    }

    #[test]
    fn try_recv_returns_results_when_available() {
        let (tx, rx) = work_queue::bounded_with_capacity(8);

        send_items(&tx, 5);
        drop(tx);

        let consumer = DeltaConsumer::spawn(rx, 16);

        let mut results = Vec::new();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            match consumer.try_recv() {
                Some(r) => results.push(r),
                None => {
                    if results.len() == 5 {
                        break;
                    }
                    assert!(
                        std::time::Instant::now() < deadline,
                        "timed out waiting for results"
                    );
                    std::thread::yield_now();
                }
            }
        }

        assert_eq!(results.len(), 5);
        for (i, r) in results.iter().enumerate() {
            assert_eq!(r.sequence(), i as u64);
        }
    }

    #[test]
    fn try_recv_on_empty_queue_returns_none() {
        let (tx, rx) = work_queue::bounded_with_capacity(4);
        drop(tx);

        let consumer = DeltaConsumer::spawn(rx, 8);

        // Give the consumer thread a moment to finish.
        std::thread::sleep(std::time::Duration::from_millis(50));

        assert!(consumer.try_recv().is_none());
        consumer.join().unwrap();
    }

    #[test]
    fn bypass_delivers_all_results() {
        let (tx, rx) = spawn_producer(50);
        let producer = std::thread::spawn(move || send_items(&tx, 50));

        let consumer = DeltaConsumer::spawn_bypass(rx);
        let results: Vec<DeltaResult> = consumer.iter().collect();
        producer.join().unwrap();

        assert_eq!(results.len(), 50);
        // All items delivered - verify by collecting ndx values.
        let mut ndx_values: Vec<u32> = results.iter().map(|r| r.ndx().get()).collect();
        ndx_values.sort_unstable();
        let expected: Vec<u32> = (0..50).collect();
        assert_eq!(ndx_values, expected);
    }

    #[test]
    fn bypass_empty_queue_yields_no_results() {
        let (tx, rx) = spawn_producer(1);
        drop(tx);

        let consumer = DeltaConsumer::spawn_bypass(rx);
        let results: Vec<DeltaResult> = consumer.iter().collect();
        assert!(results.is_empty());
    }

    #[test]
    fn bypass_single_item() {
        let (tx, rx) = spawn_producer(1);
        tx.send(DeltaWork::whole_file(42, PathBuf::from("/dst/single"), 128).with_sequence(0))
            .unwrap();
        drop(tx);

        let consumer = DeltaConsumer::spawn_bypass(rx);
        let results: Vec<DeltaResult> = consumer.iter().collect();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].ndx().get(), 42);
        assert_eq!(results[0].bytes_written(), 128);
    }

    #[test]
    fn bypass_join_succeeds() {
        let (tx, rx) = spawn_producer(10);
        let producer = std::thread::spawn(move || send_items(&tx, 10));

        let consumer = DeltaConsumer::spawn_bypass(rx);
        let results: Vec<DeltaResult> = consumer.iter().collect();
        producer.join().unwrap();

        assert_eq!(results.len(), 10);
        consumer.join().unwrap();
    }

    #[test]
    fn bypass_large_batch_delivers_all() {
        let count = 500u32;
        let (tx, rx) = work_queue::bounded_with_capacity(32);

        let producer = std::thread::spawn(move || {
            for i in 0..count {
                let work =
                    DeltaWork::whole_file(i, PathBuf::from("/dst"), 64).with_sequence(u64::from(i));
                tx.send(work).unwrap();
            }
        });

        let consumer = DeltaConsumer::spawn_bypass(rx);
        let results: Vec<DeltaResult> = consumer.into_iter().collect();
        producer.join().unwrap();

        assert_eq!(results.len(), count as usize);
        // Verify all items present (order may differ from submission).
        let mut ndx_values: Vec<u32> = results.iter().map(|r| r.ndx().get()).collect();
        ndx_values.sort_unstable();
        let expected: Vec<u32> = (0..count).collect();
        assert_eq!(ndx_values, expected);
    }

    #[test]
    fn metrics_snapshot_starts_zeroed() {
        let (_tx, rx) = work_queue::bounded_with_capacity(4);
        let consumer = DeltaConsumer::spawn(rx, 8);
        let m = consumer.metrics();
        assert_eq!(m.force_insert_count, 0);
        assert_eq!(m.drain_batch_size_histogram.total_samples(), 0);
        assert_eq!(m.drain_pause_histogram.total_samples(), 0);
    }

    /// Verifies the `force_insert` counter increments end-to-end when
    /// synthetic backpressure forces the consumer to break its capacity
    /// bound. Reproduces the small-capacity HoL pattern from the design
    /// doc: a producer that submits sequences out of order leaves the
    /// `next_expected` slot empty while later sequences fill the ring.
    #[test]
    fn metrics_force_insert_counter_increments_under_backpressure() {
        // Capacity 2 with 12 in-flight, where the bounded queue serialises
        // submissions, forces every later result to race past the empty
        // next_expected slot. The producer holds back seq 0 until the rest
        // have queued so the reorder ring overflows.
        let count = 12u32;
        let (tx, rx) = work_queue::bounded_with_capacity(count as usize);
        let producer = std::thread::spawn(move || {
            // Submit sequences 1..count first to fill the rayon pipeline,
            // then submit seq 0 to release the gap. The reorder ring is
            // size 2 so it cannot absorb the late sequences; force_insert
            // fires to keep the pipeline alive.
            for i in 1..count {
                let work =
                    DeltaWork::whole_file(i, PathBuf::from("/dst"), 64).with_sequence(u64::from(i));
                tx.send(work).unwrap();
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
            tx.send(DeltaWork::whole_file(0, PathBuf::from("/dst"), 64).with_sequence(0))
                .unwrap();
        });

        let consumer = DeltaConsumer::spawn(rx, 2);
        let results: Vec<DeltaResult> = consumer.iter().collect();
        producer.join().unwrap();
        assert_eq!(results.len(), count as usize);
        let snap = consumer.metrics();
        assert!(
            snap.force_insert_count > 0,
            "expected force_insert_count > 0 under backpressure, got {:?}",
            snap,
        );
        consumer.join().unwrap();
    }

    /// Verifies the drain-batch histogram accumulates buckets when the
    /// consumer delivers a contiguous run in a single drain iteration.
    #[test]
    fn metrics_drain_batch_histogram_accumulates() {
        let count = 16u32;
        let (tx, rx) = spawn_producer(count);
        send_items(&tx, count);
        drop(tx);

        let consumer = DeltaConsumer::spawn(rx, count as usize);
        let results: Vec<DeltaResult> = consumer.iter().collect();
        assert_eq!(results.len(), count as usize);
        let snap = consumer.metrics();
        let hist = snap.drain_batch_size_histogram;
        assert!(
            hist.total_samples() > 0,
            "expected at least one drain-batch sample, got {hist:?}",
        );
        // Sum of all bucket counts equals the number of drain iterations
        // that produced at least one item.
        let total_drained: u64 = hist
            .buckets()
            .iter()
            .enumerate()
            .map(|(idx, &count)| {
                // Lower bound of bucket idx is 2^idx (except >=1024 cap).
                let lo = 1u64 << idx.min(10);
                lo.saturating_mul(count)
            })
            .sum();
        assert!(
            total_drained >= u64::from(count),
            "histogram lower-bound sum {total_drained} must cover delivered count {count}",
        );
        consumer.join().unwrap();
    }

    // ---- SpillableReorderBuffer wiring tests (task #1884) ----

    /// Drives a 1000-item workload through the spill-enabled consumer with
    /// a deliberately delayed head-of-line item so the reorder buffer fills
    /// up before any contiguous run can be drained. A tight 1 KiB byte
    /// budget guarantees the spill machinery engages while delivery remains
    /// strictly in submission order.
    #[test]
    fn spillable_consumer_preserves_order_under_pressure() {
        const COUNT: u32 = 1000;
        // Tight budget vs ~52-byte DeltaResult: ~19 items fit before spill.
        const THRESHOLD: u64 = 1024;

        let (tx, rx) = work_queue::bounded_with_capacity(COUNT as usize);

        // Send sequences 1..COUNT first so the reorder buffer fills with
        // out-of-order items, then send seq 0 last so the head is missing
        // until the very end. Memory pressure exceeds the threshold long
        // before delivery becomes possible, forcing repeated spills.
        let producer = std::thread::spawn(move || {
            for seq in 1..COUNT {
                let work = DeltaWork::whole_file(seq, PathBuf::from("/dst"), 64)
                    .with_sequence(u64::from(seq));
                tx.send(work).unwrap();
            }
            // Small pause to let the reorder thread build up the buffer
            // before the head-of-line item unblocks the drain.
            std::thread::sleep(std::time::Duration::from_millis(50));
            tx.send(DeltaWork::whole_file(0u32, PathBuf::from("/dst"), 64).with_sequence(0))
                .unwrap();
        });

        let cfg = ConcurrentDeltaConfig::with_spill_threshold(THRESHOLD);
        let consumer = DeltaConsumer::spawn_with_config(rx, COUNT as usize, cfg);
        let results: Vec<DeltaResult> = consumer.iter().collect();
        let stats = consumer.stats();
        producer.join().unwrap();

        assert_eq!(results.len(), COUNT as usize, "all items must be delivered");
        for (i, r) in results.iter().enumerate() {
            assert_eq!(
                r.sequence(),
                i as u64,
                "out of order at position {i}: got seq {}",
                r.sequence()
            );
            assert!(r.is_success(), "result at {i} should be success");
        }
        assert!(
            stats.spill_events > 0,
            "1 KiB budget against 1000 items must trigger spills, got {}",
            stats.spill_events
        );
    }

    /// Baseline comparison: the spill-enabled and non-spill paths must deliver
    /// the same sequence of result payloads byte-for-byte. The spill layer is
    /// a local-only memory bound, never a wire-protocol change.
    #[test]
    fn spillable_consumer_matches_bare_output_byte_for_byte() {
        use crate::concurrent_delta::SpillCodec;
        const COUNT: u32 = 256;

        fn run(cfg: Option<ConcurrentDeltaConfig>) -> Vec<DeltaResult> {
            let (tx, rx) = work_queue::bounded_with_capacity(COUNT as usize);
            let producer = std::thread::spawn(move || {
                for seq in (0..COUNT).rev() {
                    let work = DeltaWork::whole_file(seq, PathBuf::from("/dst"), 64)
                        .with_sequence(u64::from(seq));
                    tx.send(work).unwrap();
                }
            });
            let consumer = match cfg {
                Some(c) => DeltaConsumer::spawn_with_config(rx, COUNT as usize, c),
                None => DeltaConsumer::spawn(rx, COUNT as usize),
            };
            let out: Vec<DeltaResult> = consumer.iter().collect();
            producer.join().unwrap();
            out
        }

        let baseline = run(None);
        let spilled = run(Some(ConcurrentDeltaConfig::with_spill_threshold(8 * 1024)));

        assert_eq!(baseline.len(), spilled.len(), "result counts must match");
        for (i, (a, b)) in baseline.iter().zip(spilled.iter()).enumerate() {
            assert_eq!(a.sequence(), b.sequence(), "sequence mismatch at {i}");
            assert_eq!(a.ndx().get(), b.ndx().get(), "ndx mismatch at {i}");
            assert_eq!(a.bytes_written(), b.bytes_written(), "bytes_written at {i}");
            assert_eq!(a.literal_bytes(), b.literal_bytes(), "literal at {i}");
            assert_eq!(a.matched_bytes(), b.matched_bytes(), "matched at {i}");
            assert_eq!(a.is_success(), b.is_success(), "status at {i}");

            // SpillCodec round-trips the binary encoding the spill layer uses;
            // identical encodings prove the payloads are byte-equivalent.
            let mut buf_a = Vec::new();
            let mut buf_b = Vec::new();
            a.encode(&mut buf_a).unwrap();
            b.encode(&mut buf_b).unwrap();
            assert_eq!(buf_a, buf_b, "encoded payload differs at {i}");
        }
    }

    #[test]
    fn spawn_with_config_off_matches_spawn() {
        let cfg = ConcurrentDeltaConfig::off();

        let (tx_a, rx_a) = spawn_producer(20);
        let prod_a = std::thread::spawn(move || send_items(&tx_a, 20));
        let baseline = DeltaConsumer::spawn(rx_a, 32).iter().collect::<Vec<_>>();
        prod_a.join().unwrap();

        let (tx_b, rx_b) = spawn_producer(20);
        let prod_b = std::thread::spawn(move || send_items(&tx_b, 20));
        let configured = DeltaConsumer::spawn_with_config(rx_b, 32, cfg)
            .iter()
            .collect::<Vec<_>>();
        prod_b.join().unwrap();

        assert_eq!(baseline.len(), configured.len());
        for (a, b) in baseline.iter().zip(configured.iter()) {
            assert_eq!(a.sequence(), b.sequence());
            assert_eq!(a.ndx().get(), b.ndx().get());
        }
    }

    #[test]
    fn stats_zero_when_spill_disabled() {
        let (tx, rx) = spawn_producer(10);
        let producer = std::thread::spawn(move || send_items(&tx, 10));
        let consumer = DeltaConsumer::spawn(rx, 16);
        let _: Vec<DeltaResult> = consumer.iter().collect();
        producer.join().unwrap();
        assert_eq!(consumer.stats().spill_events, 0);
    }
}
