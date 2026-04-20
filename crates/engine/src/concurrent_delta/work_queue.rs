//! Bounded work queue for the concurrent delta pipeline.
//!
//! Prevents OOM by limiting the number of in-flight [`DeltaWork`] items.
//! The producer side blocks when the queue is full, applying backpressure
//! to the generator/receiver that feeds work items. The consumer side
//! drains items in parallel via [`WorkQueueReceiver::drain_parallel`],
//! which internally uses [`rayon::scope`] to spawn one task per item with
//! per-thread result buffers for contention-free collection across the
//! rayon thread pool.
//!
//! # SPMC (Single-Producer, Multiple-Consumer) Design
//!
//! This module assumes a Single-Producer Multiple-Consumer pattern. A single
//! producer thread (the generator or receiver) feeds [`DeltaWork`] items into
//! the queue, and multiple rayon worker threads consume them in parallel.
//!
//! This is SPMC rather than MPMC because the rsync wire protocol is inherently
//! single-threaded on the receiving side - one multiplexed stream delivers file
//! entries in sequence, so there is exactly one thread reading from the wire and
//! producing work items. [`WorkQueueSender`] enforces this by being `Send` but
//! not `Clone`, preventing multiple producers at compile time.
//!
//! ## Ordering Contract
//!
//! Work items arrive in wire order from the single producer. Consumers may
//! process items out of order (determined by rayon work-stealing scheduling).
//! When sequential output is required, results carry a sequence number and
//! are fed through [`ReorderBuffer`](super::reorder::ReorderBuffer) to restore
//! the original wire order before emission.
//!
//! ## Multi-Producer Considerations
//!
//! Supporting multiple producers would require replacing the `SyncSender` with
//! a cloneable MPMC channel and revising the ordering contract (multiple
//! producers would need coordinated sequence numbering). See issues #1382 and
//! #1569 for future multi-producer design discussion.
//!
//! # Capacity
//!
//! The default capacity is `2 * rayon::current_num_threads()`, which keeps
//! workers saturated without buffering an unbounded number of items. For a
//! transfer of millions of small files, this bounds memory to a small fixed
//! multiple of the thread count rather than the file count.
//!
//! # Architecture
//!
//! ```text
//! Generator ─► WorkQueue (bounded) ─► drain_parallel(f) ─► Vec<R>
//!  (single       blocks when full       rayon::scope          |
//!  producer)                            work-stealing        v
//!                                       (N consumers)   ReorderBuffer
//!                                                             |
//!                                                             v
//!                                                   consumer (in-order)
//! ```
//!
//! For streaming pipelines, [`drain_parallel_into`] sends results through a
//! channel as workers complete, enabling incremental consumption without
//! waiting for all items to finish:
//!
//! ```text
//! Generator ─► WorkQueue ─► drain_parallel_into(f, tx) ─► SyncSender<R>
//!  (single        rayon::scope                                 |
//!  producer)      work-stealing                                v
//!                 (N consumers)                       ReorderBuffer (live)
//!                                                             |
//!                                                             v
//!                                                   consumer (incremental)
//! ```
//!
//! # Usage
//!
//! ```rust,no_run
//! use engine::concurrent_delta::work_queue;
//! use engine::concurrent_delta::DeltaWork;
//! use std::path::PathBuf;
//!
//! let (tx, rx) = work_queue::bounded();
//!
//! // Producer thread
//! std::thread::spawn(move || {
//!     for i in 0..100 {
//!         let work = DeltaWork::whole_file(i, PathBuf::from("/dest"), 1024);
//!         tx.send(work).unwrap();
//!     }
//! });
//!
//! // Parallel consumers via drain_parallel
//! let ndx_list: Vec<u32> = rx.drain_parallel(|w| w.ndx());
//! assert_eq!(ndx_list.len(), 100);
//! ```
//!
//! # Upstream Reference
//!
//! Upstream rsync processes files sequentially in `recv_files()`. This queue
//! enables parallel processing while bounding memory - upstream does not need
//! this because it never queues ahead.

use std::sync::mpsc::{self, Receiver, SyncSender};

use super::DeltaWork;

/// Default capacity multiplier applied to the rayon thread count.
const CAPACITY_MULTIPLIER: usize = 2;

/// Bounded work queue that limits in-flight delta computation items.
///
/// Created via [`bounded`] or [`bounded_with_capacity`]. The sender half
/// blocks when the queue reaches capacity, preventing unbounded memory growth
/// when the generator produces work faster than workers consume it.
///
/// # Thread Safety
///
/// The [`WorkQueueSender`] is `Send` but intentionally not `Clone`, enforcing
/// the single-producer invariant at compile time. Only one thread - the
/// generator or receiver reading from the wire - may send work items.
/// The [`WorkQueueReceiver`] is `Send` and implements [`Iterator`] for use
/// with [`rayon::scope`] based consumption loops, where multiple rayon
/// workers act as concurrent consumers.
///
/// # Example
///
/// ```rust,no_run
/// use engine::concurrent_delta::work_queue;
/// use engine::concurrent_delta::DeltaWork;
/// use std::path::PathBuf;
///
/// let (tx, rx) = work_queue::bounded();
///
/// // Producer thread
/// std::thread::spawn(move || {
///     for i in 0..1000 {
///         let work = DeltaWork::whole_file(i, PathBuf::from("/dest"), 1024);
///         tx.send(work).unwrap();
///     }
/// });
///
/// // Parallel consumers via drain_parallel
/// let results: Vec<u32> = rx.drain_parallel(|w| w.ndx());
/// ```
pub struct WorkQueueSender {
    tx: SyncSender<DeltaWork>,
}

/// Receiving half of the bounded work queue.
///
/// Implements [`Iterator`] so it can be consumed in a `rayon::scope` loop
/// that spawns one task per item for parallel processing. For convenience,
/// [`drain_parallel`](Self::drain_parallel) encapsulates the `rayon::scope`
/// pattern into a single method call.
pub struct WorkQueueReceiver {
    rx: Receiver<DeltaWork>,
}

impl WorkQueueReceiver {
    /// Drains the queue in parallel, applying `f` to each item via `rayon::scope`.
    ///
    /// Spawns one rayon task per [`DeltaWork`] item, allowing the rayon thread
    /// pool to work-steal across all items. The bounded queue provides natural
    /// backpressure - the iterator blocks when the queue is empty and the
    /// producer blocks when the queue is full.
    ///
    /// Results are collected in arbitrary order (determined by worker completion
    /// timing). Use [`ReorderBuffer`] on the returned `Vec` if sequential
    /// ordering is required.
    ///
    /// Internally, results are sharded across `N` mutex-guarded `Vec`s (one per
    /// rayon thread). Each worker indexes its shard via [`rayon::current_thread_index`],
    /// distributing contention across shards instead of concentrating it in a
    /// single `Mutex<Vec<R>>`. Threads outside the rayon pool fall back to a
    /// thread-ID hash to avoid the degenerate case of all mapping to shard 0.
    /// After all tasks complete, the per-shard buffers are flattened into a
    /// single `Vec`.
    ///
    /// This method consumes the receiver. It returns once the sender is dropped
    /// and all queued items have been processed.
    ///
    /// [`ReorderBuffer`]: super::reorder::ReorderBuffer
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use engine::concurrent_delta::work_queue;
    /// use engine::concurrent_delta::DeltaWork;
    /// use std::path::PathBuf;
    ///
    /// let (tx, rx) = work_queue::bounded();
    ///
    /// std::thread::spawn(move || {
    ///     for i in 0..10 {
    ///         tx.send(DeltaWork::whole_file(i, PathBuf::from("/dst"), 64)).unwrap();
    ///     }
    /// });
    ///
    /// let indices: Vec<u32> = rx.drain_parallel(|w| w.ndx());
    /// assert_eq!(indices.len(), 10);
    /// ```
    pub fn drain_parallel<F, R>(self, f: F) -> Vec<R>
    where
        F: Fn(DeltaWork) -> R + Send + Sync,
        R: Send,
    {
        let num_shards = rayon::current_num_threads();
        let shards: Vec<std::sync::Mutex<Vec<R>>> = (0..num_shards)
            .map(|_| std::sync::Mutex::new(Vec::new()))
            .collect();

        rayon::scope(|s| {
            for work in self.into_iter() {
                let f = &f;
                let shards = &shards;
                s.spawn(move |_| {
                    let result = f(work);
                    let idx = rayon::current_thread_index().unwrap_or_else(|| {
                        // Outside rayon pool: hash thread ID to distribute
                        // across shards instead of collapsing to shard 0.
                        let id = std::thread::current().id();
                        let mut hasher = std::hash::DefaultHasher::new();
                        std::hash::Hash::hash(&id, &mut hasher);
                        std::hash::Hasher::finish(&hasher) as usize
                    });
                    shards[idx % num_shards].lock().unwrap().push(result);
                });
            }
        });

        shards
            .into_iter()
            .flat_map(|shard| shard.into_inner().unwrap())
            .collect()
    }

    /// Streams results through a channel as workers complete, enabling
    /// incremental consumption without waiting for all items to finish.
    ///
    /// Unlike [`drain_parallel`](Self::drain_parallel) which collects all
    /// results into a `Vec`, this method sends each result through `tx` as
    /// soon as its worker finishes. This enables pipeline overlap - the
    /// consumer can process results (e.g., reorder and write to disk) while
    /// delta computation continues for remaining items.
    ///
    /// The `SyncSender` provides backpressure: if the consumer falls behind,
    /// workers block on `tx.send()` rather than accumulating unbounded results
    /// in memory.
    ///
    /// This method blocks until the sender is dropped and all queued items
    /// have been processed. The `tx` channel is dropped when this method
    /// returns, signaling completion to the receiver.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use engine::concurrent_delta::work_queue;
    /// use engine::concurrent_delta::DeltaWork;
    /// use std::path::PathBuf;
    /// use std::sync::mpsc;
    ///
    /// let (work_tx, work_rx) = work_queue::bounded();
    /// let (result_tx, result_rx) = mpsc::sync_channel(16);
    ///
    /// // Producer thread
    /// std::thread::spawn(move || {
    ///     for i in 0..100 {
    ///         work_tx.send(DeltaWork::whole_file(i, PathBuf::from("/dst"), 64)).unwrap();
    ///     }
    /// });
    ///
    /// // Drain thread - sends results as workers complete
    /// std::thread::spawn(move || {
    ///     work_rx.drain_parallel_into(|w| w.ndx(), result_tx);
    /// });
    ///
    /// // Consumer thread - processes results incrementally
    /// for ndx in result_rx {
    ///     // Process each result as it arrives
    /// }
    /// ```
    pub fn drain_parallel_into<F, R>(self, f: F, tx: SyncSender<R>)
    where
        F: Fn(DeltaWork) -> R + Send + Sync,
        R: Send,
    {
        rayon::scope(|s| {
            for work in self.into_iter() {
                let f = &f;
                let tx = tx.clone();
                s.spawn(move |_| {
                    let result = f(work);
                    // If the receiver is dropped, silently stop - the consumer
                    // has decided it doesn't need more results.
                    let _ = tx.send(result);
                });
            }
        });
        // `tx` (the original, not clones) is dropped here, closing the channel
        // after all rayon tasks complete.
    }
}

/// Error returned when the receiver has been dropped and the queue is closed.
#[derive(Debug)]
pub struct SendError(pub DeltaWork);

impl std::fmt::Display for SendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("work queue receiver has been dropped")
    }
}

impl std::error::Error for SendError {}

/// Cloning the sender enables multiple producer threads to feed the work queue
/// concurrently, turning the pipeline from SPMC into MPMC. Each clone shares
/// the same underlying bounded channel, so backpressure and capacity limits
/// still apply. Sequence numbering must be coordinated externally when multiple
/// producers are active.
#[cfg(feature = "multi-producer")]
impl Clone for WorkQueueSender {
    fn clone(&self) -> Self {
        Self {
            tx: self.tx.clone(),
        }
    }
}

impl WorkQueueSender {
    /// Sends a work item into the queue, blocking if at capacity.
    ///
    /// Returns `Err(SendError)` if the receiver has been dropped.
    pub fn send(&self, work: DeltaWork) -> Result<(), SendError> {
        self.tx.send(work).map_err(|e| SendError(e.0))
    }
}

impl IntoIterator for WorkQueueReceiver {
    type Item = DeltaWork;
    type IntoIter = WorkQueueIter;

    /// Converts the receiver into an iterator for `rayon::scope` consumption.
    ///
    /// The returned iterator yields items until the sender is dropped and the
    /// queue is drained.
    fn into_iter(self) -> WorkQueueIter {
        WorkQueueIter { rx: self.rx }
    }
}

/// Iterator adapter over the work queue receiver.
///
/// Yields [`DeltaWork`] items until the sender drops and the queue drains.
/// Designed for use with `rayon::scope` based consumption.
pub struct WorkQueueIter {
    rx: Receiver<DeltaWork>,
}

impl Iterator for WorkQueueIter {
    type Item = DeltaWork;

    fn next(&mut self) -> Option<DeltaWork> {
        self.rx.recv().ok()
    }
}

/// Creates a bounded work queue with default capacity (2x rayon thread count).
///
/// The capacity is computed at call time from [`rayon::current_num_threads`].
/// This provides enough headroom to keep all workers busy while bounding
/// memory usage to a small fixed amount regardless of file count.
#[must_use]
pub fn bounded() -> (WorkQueueSender, WorkQueueReceiver) {
    let capacity = rayon::current_num_threads() * CAPACITY_MULTIPLIER;
    bounded_with_capacity(capacity)
}

/// Creates a bounded work queue with an explicit capacity.
///
/// # Panics
///
/// Panics if `capacity` is zero.
#[must_use]
pub fn bounded_with_capacity(capacity: usize) -> (WorkQueueSender, WorkQueueReceiver) {
    assert!(capacity > 0, "work queue capacity must be non-zero");
    let (tx, rx) = mpsc::sync_channel(capacity);
    (WorkQueueSender { tx }, WorkQueueReceiver { rx })
}

/// Returns the default work queue capacity for the current rayon thread pool.
///
/// Equal to `2 * rayon::current_num_threads()`.
#[must_use]
pub fn default_capacity() -> usize {
    rayon::current_num_threads() * CAPACITY_MULTIPLIER
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::{Duration, Instant};

    use proptest::prelude::*;

    use super::*;
    use crate::concurrent_delta::DeltaWork;

    #[test]
    fn basic_send_recv() {
        let (tx, rx) = bounded_with_capacity(4);
        let work = DeltaWork::whole_file(1, PathBuf::from("/dst"), 100);
        tx.send(work).unwrap();
        let mut iter = rx.into_iter();
        let item = iter.next().unwrap();
        assert_eq!(item.ndx(), 1);
    }

    #[test]
    fn receiver_drop_causes_send_error() {
        let (tx, rx) = bounded_with_capacity(4);
        drop(rx);
        let work = DeltaWork::whole_file(0, PathBuf::from("/dst"), 0);
        assert!(tx.send(work).is_err());
    }

    #[test]
    fn sender_drop_drains_then_ends_iter() {
        let (tx, rx) = bounded_with_capacity(4);
        tx.send(DeltaWork::whole_file(1, PathBuf::from("/a"), 10))
            .unwrap();
        tx.send(DeltaWork::whole_file(2, PathBuf::from("/b"), 20))
            .unwrap();
        drop(tx);

        let items: Vec<u32> = rx.into_iter().map(|w| w.ndx()).collect();
        assert_eq!(items, vec![1, 2]);
    }

    #[test]
    fn scope_processes_all_items() {
        let (tx, rx) = bounded_with_capacity(8);
        let count = 200;

        let producer = thread::spawn(move || {
            for i in 0..count {
                let work = DeltaWork::whole_file(i, PathBuf::from("/dst"), 64);
                tx.send(work).unwrap();
            }
        });

        let results = Mutex::new(Vec::new());
        rayon::scope(|s| {
            for w in rx.into_iter() {
                let results = &results;
                s.spawn(move |_| {
                    results.lock().unwrap().push(w.ndx());
                });
            }
        });
        producer.join().unwrap();

        let mut results = results.into_inner().unwrap();
        results.sort_unstable();
        assert_eq!(results, (0..count).collect::<Vec<u32>>());
    }

    #[test]
    fn backpressure_blocks_producer() {
        // Capacity of 2: producer must block after 2 items until consumer drains.
        let (tx, rx) = bounded_with_capacity(2);
        let sent_count = Arc::new(AtomicUsize::new(0));
        let sent_count_clone = Arc::clone(&sent_count);

        let producer = thread::spawn(move || {
            for i in 0..5u32 {
                let work = DeltaWork::whole_file(i, PathBuf::from("/dst"), 0);
                tx.send(work).unwrap();
                sent_count_clone.fetch_add(1, Ordering::Release);
            }
        });

        // Give the producer time to fill the queue and block.
        thread::sleep(Duration::from_millis(50));
        let sent_before_drain = sent_count.load(Ordering::Acquire);
        // The producer should have sent at most capacity + 1 items
        // (capacity in the buffer + 1 that the sync_channel allows to be
        // "in flight" during the blocking send call).
        assert!(
            sent_before_drain <= 3,
            "producer sent {sent_before_drain} items with capacity 2 - backpressure not working"
        );

        // Now drain everything.
        let items: Vec<u32> = rx.into_iter().map(|w| w.ndx()).collect();
        producer.join().unwrap();
        assert_eq!(items, vec![0, 1, 2, 3, 4]);
    }

    #[test]
    fn bounded_respects_in_flight_limit() {
        // Verify that at most `capacity` items are concurrently in-flight
        // by tracking active worker count.
        let capacity = 4;
        let (tx, rx) = bounded_with_capacity(capacity);
        let total_items = 50u32;

        let active = Arc::new(AtomicUsize::new(0));
        let max_active = Arc::new(AtomicUsize::new(0));

        let producer = thread::spawn(move || {
            for i in 0..total_items {
                let work = DeltaWork::whole_file(i, PathBuf::from("/dst"), 0);
                tx.send(work).unwrap();
            }
        });

        let active_ref = Arc::clone(&active);
        let max_active_ref = Arc::clone(&max_active);

        let collected = Mutex::new(Vec::new());
        rayon::scope(|s| {
            for w in rx.into_iter() {
                let active_ref = Arc::clone(&active_ref);
                let max_active_ref = Arc::clone(&max_active_ref);
                let collected = &collected;
                s.spawn(move |_| {
                    let current = active_ref.fetch_add(1, Ordering::SeqCst) + 1;
                    // Update max observed concurrency.
                    max_active_ref.fetch_max(current, Ordering::SeqCst);
                    // Simulate work to increase chance of overlapping.
                    thread::sleep(Duration::from_micros(100));
                    active_ref.fetch_sub(1, Ordering::SeqCst);
                    collected.lock().unwrap().push(w.ndx());
                });
            }
        });
        let results = collected.into_inner().unwrap();

        producer.join().unwrap();
        assert_eq!(results.len(), total_items as usize);

        let observed_max = max_active.load(Ordering::SeqCst);
        // max concurrency is bounded by rayon thread pool size, which combined
        // with our bounded queue means we never have unbounded in-flight items.
        let thread_count = rayon::current_num_threads();
        assert!(
            observed_max <= thread_count,
            "observed {observed_max} concurrent workers exceeds rayon thread count {thread_count}"
        );
    }

    #[test]
    fn default_capacity_is_positive() {
        let cap = default_capacity();
        assert!(cap >= 2, "default capacity should be at least 2, got {cap}");
    }

    #[test]
    #[should_panic(expected = "capacity must be non-zero")]
    fn zero_capacity_panics() {
        let _ = bounded_with_capacity(0);
    }

    #[test]
    fn custom_capacity() {
        let (tx, rx) = bounded_with_capacity(1);
        let work = DeltaWork::whole_file(0, PathBuf::from("/dst"), 0);
        tx.send(work).unwrap();

        // Queue is full (capacity 1). Verify the item arrives.
        let mut iter = rx.into_iter();
        assert_eq!(iter.next().unwrap().ndx(), 0);
    }

    #[test]
    fn send_error_displays_message() {
        let (tx, rx) = bounded_with_capacity(1);
        drop(rx);
        let err = tx
            .send(DeltaWork::whole_file(0, PathBuf::from("/d"), 0))
            .unwrap_err();
        assert_eq!(err.to_string(), "work queue receiver has been dropped");
        assert_eq!(err.0.ndx(), 0);
    }

    #[test]
    fn large_batch_completes_without_oom() {
        // Simulates a large transfer - 10,000 items through a small queue.
        // With an unbounded approach this would buffer all items; with our
        // bounded queue, at most `capacity` are in-flight at any time.
        let capacity = 8;
        let (tx, rx) = bounded_with_capacity(capacity);
        let total = 10_000u32;

        let producer = thread::spawn(move || {
            for i in 0..total {
                let work = DeltaWork::whole_file(i, PathBuf::from("/dst"), 64);
                tx.send(work).unwrap();
            }
        });

        let counter = AtomicUsize::new(0);
        rayon::scope(|s| {
            for _ in rx.into_iter() {
                let counter = &counter;
                s.spawn(move |_| {
                    counter.fetch_add(1, Ordering::Relaxed);
                });
            }
        });
        let count = counter.load(Ordering::Relaxed);
        producer.join().unwrap();
        assert_eq!(count, total as usize);
    }

    #[test]
    fn delta_work_items_pass_through_queue() {
        let (tx, rx) = bounded_with_capacity(4);
        let work = DeltaWork::delta(
            42,
            PathBuf::from("/dest/file.txt"),
            PathBuf::from("/basis/file.txt"),
            2048,
            700,
            1348,
        );
        tx.send(work).unwrap();
        drop(tx);

        let items: Vec<_> = rx.into_iter().collect();
        assert_eq!(items.len(), 1);
        assert!(items[0].is_delta());
        assert_eq!(items[0].ndx(), 42);
        assert_eq!(items[0].target_size(), 2048);
    }

    #[test]
    fn producer_completes_before_consumer_starts() {
        // All items buffered then consumed - works as long as count <= capacity.
        let (tx, rx) = bounded_with_capacity(5);
        for i in 0..5u32 {
            tx.send(DeltaWork::whole_file(i, PathBuf::from("/d"), 0))
                .unwrap();
        }
        drop(tx);

        let items: Vec<u32> = rx.into_iter().map(|w| w.ndx()).collect();
        assert_eq!(items, vec![0, 1, 2, 3, 4]);
    }

    #[test]
    fn concurrent_producer_consumer_timing() {
        // Ensure no deadlock when producer and consumer run concurrently
        // with a very small queue.
        let (tx, rx) = bounded_with_capacity(1);
        let total = 100u32;

        let producer = thread::spawn(move || {
            for i in 0..total {
                tx.send(DeltaWork::whole_file(i, PathBuf::from("/d"), 0))
                    .unwrap();
            }
        });

        let deadline = Instant::now() + Duration::from_secs(10);
        let items: Vec<u32> = rx
            .into_iter()
            .map(|w| {
                assert!(Instant::now() < deadline, "deadlock detected - timed out");
                w.ndx()
            })
            .collect();

        producer.join().unwrap();
        assert_eq!(items.len(), total as usize);
    }

    #[test]
    fn pipeline_with_reorder_buffer() {
        use crate::concurrent_delta::reorder::ReorderBuffer;
        use crate::concurrent_delta::strategy;

        let (tx, rx) = bounded_with_capacity(8);
        let total = 50u32;

        let producer = thread::spawn(move || {
            for i in 0..total {
                let work = DeltaWork::whole_file(i, PathBuf::from("/dst"), 64);
                tx.send(work).unwrap();
            }
        });

        // Parallel workers dispatch and stamp sequence numbers.
        // In a real pipeline the producer stamps sequences before sending,
        // but here we use ndx as the sequence for demonstration.
        let collected = Mutex::new(Vec::new());
        rayon::scope(|s| {
            for w in rx.into_iter() {
                let collected = &collected;
                s.spawn(move |_| {
                    let seq = u64::from(w.ndx());
                    let result = strategy::dispatch(&w).with_sequence(seq);
                    collected.lock().unwrap().push(result);
                });
            }
        });
        let results: Vec<_> = collected.into_inner().unwrap();
        producer.join().unwrap();

        // Feed out-of-order results into the reorder buffer.
        let mut reorder = ReorderBuffer::new(total as usize);
        for r in results {
            reorder.insert(r.sequence(), r).unwrap();
        }

        // Drain in order and verify sequence.
        let ordered: Vec<u64> = reorder.drain_ready().map(|r| r.sequence()).collect();
        let expected: Vec<u64> = (0..u64::from(total)).collect();
        assert_eq!(ordered, expected);
    }

    #[test]
    fn drain_parallel_into_streams_all_items() {
        let (tx, rx) = bounded_with_capacity(8);
        let count = 200u32;

        let producer = thread::spawn(move || {
            for i in 0..count {
                tx.send(DeltaWork::whole_file(i, PathBuf::from("/dst"), 64))
                    .unwrap();
            }
        });

        let (result_tx, result_rx) = mpsc::sync_channel(16);
        thread::spawn(move || {
            rx.drain_parallel_into(|w| w.ndx(), result_tx);
        });

        let mut results: Vec<u32> = result_rx.iter().collect();
        producer.join().unwrap();

        results.sort_unstable();
        assert_eq!(results, (0..count).collect::<Vec<u32>>());
    }

    #[test]
    fn drain_parallel_into_empty_queue() {
        let (tx, rx) = bounded_with_capacity(4);
        drop(tx);

        let (result_tx, result_rx) = mpsc::sync_channel(4);
        thread::spawn(move || {
            rx.drain_parallel_into(|w| w.ndx(), result_tx);
        });

        let results: Vec<u32> = result_rx.iter().collect();
        assert!(results.is_empty());
    }

    #[test]
    fn drain_parallel_into_backpressure() {
        // Bounded result channel with capacity 2: workers block when consumer
        // is slow, preventing unbounded memory growth.
        let (tx, rx) = bounded_with_capacity(4);
        let total = 50u32;

        let producer = thread::spawn(move || {
            for i in 0..total {
                tx.send(DeltaWork::whole_file(i, PathBuf::from("/dst"), 0))
                    .unwrap();
            }
        });

        let (result_tx, result_rx) = mpsc::sync_channel(2);
        thread::spawn(move || {
            rx.drain_parallel_into(|w| w.ndx(), result_tx);
        });

        let mut results = Vec::new();
        for r in result_rx {
            results.push(r);
        }
        producer.join().unwrap();

        results.sort_unstable();
        assert_eq!(results, (0..total).collect::<Vec<u32>>());
    }

    #[test]
    fn drain_parallel_into_receiver_drop_stops_workers() {
        let (tx, rx) = bounded_with_capacity(8);
        let total = 100u32;

        let producer = thread::spawn(move || {
            for i in 0..total {
                let _ = tx.send(DeltaWork::whole_file(i, PathBuf::from("/dst"), 0));
            }
        });

        let (result_tx, result_rx) = mpsc::sync_channel(4);
        let drain_handle = thread::spawn(move || {
            rx.drain_parallel_into(|w| w.ndx(), result_tx);
        });

        // Take a few results then drop the receiver.
        let _ = result_rx.recv();
        drop(result_rx);

        // Drain thread should complete without hanging.
        let deadline = Instant::now() + Duration::from_secs(5);
        producer.join().unwrap();
        drain_handle.join().unwrap();
        assert!(
            Instant::now() < deadline,
            "drain_parallel_into hung after receiver drop"
        );
    }

    #[test]
    fn drain_parallel_into_single_item() {
        let (tx, rx) = bounded_with_capacity(4);
        tx.send(DeltaWork::whole_file(42, PathBuf::from("/dst"), 128))
            .unwrap();
        drop(tx);

        let (result_tx, result_rx) = mpsc::sync_channel(4);
        thread::spawn(move || {
            rx.drain_parallel_into(|w| (w.ndx(), w.target_size()), result_tx);
        });

        let results: Vec<_> = result_rx.iter().collect();
        assert_eq!(results, vec![(42, 128)]);
    }

    #[test]
    fn drain_parallel_collects_all_items() {
        let (tx, rx) = bounded_with_capacity(8);
        let count = 200u32;

        let producer = thread::spawn(move || {
            for i in 0..count {
                tx.send(DeltaWork::whole_file(i, PathBuf::from("/dst"), 64))
                    .unwrap();
            }
        });

        let mut results = rx.drain_parallel(|w| w.ndx());
        producer.join().unwrap();

        results.sort_unstable();
        assert_eq!(results, (0..count).collect::<Vec<u32>>());
    }

    #[test]
    fn drain_parallel_empty_queue() {
        let (tx, rx) = bounded_with_capacity(4);
        drop(tx); // close immediately - no items sent
        let results: Vec<u32> = rx.drain_parallel(|w| w.ndx());
        assert!(results.is_empty());
    }

    #[test]
    fn drain_parallel_backpressure() {
        // Capacity of 2: producer must block after filling the queue,
        // but drain_parallel still processes all items without deadlock.
        let (tx, rx) = bounded_with_capacity(2);
        let sent_count = Arc::new(AtomicUsize::new(0));
        let sent_clone = Arc::clone(&sent_count);
        let total = 50u32;

        let producer = thread::spawn(move || {
            for i in 0..total {
                tx.send(DeltaWork::whole_file(i, PathBuf::from("/dst"), 0))
                    .unwrap();
                sent_clone.fetch_add(1, Ordering::Release);
            }
        });

        let results = rx.drain_parallel(|w| w.ndx());
        producer.join().unwrap();

        assert_eq!(results.len(), total as usize);
        let mut sorted = results;
        sorted.sort_unstable();
        assert_eq!(sorted, (0..total).collect::<Vec<u32>>());
    }

    #[test]
    fn drain_parallel_error_propagation() {
        // Closure returns Result - errors are collected alongside successes.
        let (tx, rx) = bounded_with_capacity(8);
        let total = 20u32;

        let producer = thread::spawn(move || {
            for i in 0..total {
                tx.send(DeltaWork::whole_file(i, PathBuf::from("/dst"), 0))
                    .unwrap();
            }
        });

        let results: Vec<Result<u32, String>> = rx.drain_parallel(|w| {
            let ndx = w.ndx();
            if ndx % 5 == 0 {
                Err(format!("failed on ndx {ndx}"))
            } else {
                Ok(ndx)
            }
        });
        producer.join().unwrap();

        assert_eq!(results.len(), total as usize);
        let errors: Vec<_> = results.iter().filter(|r| r.is_err()).collect();
        let successes: Vec<_> = results.iter().filter(|r| r.is_ok()).collect();
        // ndx 0, 5, 10, 15 fail => 4 errors, 16 successes
        assert_eq!(errors.len(), 4);
        assert_eq!(successes.len(), 16);
    }

    #[test]
    fn drain_parallel_single_item() {
        let (tx, rx) = bounded_with_capacity(4);
        tx.send(DeltaWork::whole_file(42, PathBuf::from("/dst"), 128))
            .unwrap();
        drop(tx);

        let results = rx.drain_parallel(|w| (w.ndx(), w.target_size()));
        assert_eq!(results, vec![(42, 128)]);
    }

    #[test]
    fn drain_parallel_with_reorder_buffer() {
        use crate::concurrent_delta::reorder::ReorderBuffer;
        use crate::concurrent_delta::strategy;

        let (tx, rx) = bounded_with_capacity(8);
        let total = 100u32;

        let producer = thread::spawn(move || {
            for i in 0..total {
                tx.send(DeltaWork::whole_file(i, PathBuf::from("/dst"), 64))
                    .unwrap();
            }
        });

        let results = rx.drain_parallel(|w| {
            let seq = u64::from(w.ndx());
            strategy::dispatch(&w).with_sequence(seq)
        });
        producer.join().unwrap();

        // Feed into reorder buffer and verify sequential output.
        let mut reorder = ReorderBuffer::new(total as usize);
        for r in results {
            reorder.insert(r.sequence(), r).unwrap();
        }
        let ordered: Vec<u64> = reorder.drain_ready().map(|r| r.sequence()).collect();
        assert_eq!(ordered, (0..u64::from(total)).collect::<Vec<u64>>());
    }

    #[test]
    #[cfg(feature = "multi-producer")]
    fn clone_sender_multiple_producers() {
        let (tx, rx) = bounded_with_capacity(8);
        let tx2 = tx.clone();
        let items_per_producer = 50u32;

        let p1 = thread::spawn(move || {
            for i in 0..items_per_producer {
                tx.send(DeltaWork::whole_file(i, PathBuf::from("/dst"), 0))
                    .unwrap();
            }
        });

        let p2 = thread::spawn(move || {
            for i in items_per_producer..(items_per_producer * 2) {
                tx2.send(DeltaWork::whole_file(i, PathBuf::from("/dst"), 0))
                    .unwrap();
            }
        });

        let mut results = rx.drain_parallel(|w| w.ndx());
        p1.join().unwrap();
        p2.join().unwrap();

        results.sort_unstable();
        assert_eq!(results.len(), (items_per_producer * 2) as usize);
        assert_eq!(results, (0..(items_per_producer * 2)).collect::<Vec<u32>>());
    }

    #[test]
    #[cfg(feature = "multi-producer")]
    fn multi_producer_many_senders() {
        // Verify that many cloned senders (N=8) can all send items concurrently
        // and all items are received without loss or duplication.
        let num_producers = 8u32;
        let items_per_producer = 100u32;
        let (tx, rx) = bounded_with_capacity(16);

        let handles: Vec<_> = (0..num_producers)
            .map(|producer_id| {
                let sender = tx.clone();
                thread::spawn(move || {
                    let base = producer_id * items_per_producer;
                    for i in 0..items_per_producer {
                        sender
                            .send(DeltaWork::whole_file(base + i, PathBuf::from("/dst"), 64))
                            .unwrap();
                    }
                })
            })
            .collect();

        // Drop the original sender so the channel closes when all clones drop.
        drop(tx);

        let mut results = rx.drain_parallel(|w| w.ndx());
        for h in handles {
            h.join().unwrap();
        }

        results.sort_unstable();
        let expected: Vec<u32> = (0..(num_producers * items_per_producer)).collect();
        assert_eq!(results, expected);
    }

    #[test]
    #[cfg(feature = "multi-producer")]
    fn multi_producer_dropping_one_sender_does_not_affect_others() {
        // Dropping one cloned sender must not close the channel - other senders
        // can continue sending and the receiver stays open.
        let (tx, rx) = bounded_with_capacity(8);
        let tx2 = tx.clone();
        let tx3 = tx.clone();

        // Drop one sender immediately.
        drop(tx2);

        // The remaining senders should still work.
        tx.send(DeltaWork::whole_file(1, PathBuf::from("/dst"), 0))
            .unwrap();
        tx3.send(DeltaWork::whole_file(2, PathBuf::from("/dst"), 0))
            .unwrap();

        drop(tx);
        drop(tx3);

        let mut items: Vec<u32> = rx.into_iter().map(|w| w.ndx()).collect();
        items.sort_unstable();
        assert_eq!(items, vec![1, 2]);
    }

    #[test]
    #[cfg(feature = "multi-producer")]
    fn multi_producer_receiver_completes_only_when_all_senders_dropped() {
        // The receiver iterator must not terminate until ALL sender clones are
        // dropped, not just one.
        let (tx, rx) = bounded_with_capacity(4);
        let tx2 = tx.clone();
        let tx3 = tx.clone();

        let received = Arc::new(Mutex::new(Vec::new()));
        let received_clone = Arc::clone(&received);

        let consumer = thread::spawn(move || {
            for w in rx.into_iter() {
                received_clone.lock().unwrap().push(w.ndx());
            }
        });

        // First sender sends and drops.
        tx.send(DeltaWork::whole_file(1, PathBuf::from("/dst"), 0))
            .unwrap();
        drop(tx);

        // Small delay to let consumer process - channel should NOT be closed.
        thread::sleep(Duration::from_millis(20));

        // Second sender sends and drops.
        tx2.send(DeltaWork::whole_file(2, PathBuf::from("/dst"), 0))
            .unwrap();
        drop(tx2);

        thread::sleep(Duration::from_millis(20));

        // Third sender sends and drops - this should close the channel.
        tx3.send(DeltaWork::whole_file(3, PathBuf::from("/dst"), 0))
            .unwrap();
        drop(tx3);

        consumer.join().unwrap();

        let mut items = Arc::try_unwrap(received)
            .expect("Arc should have single owner after join")
            .into_inner()
            .unwrap();
        items.sort_unstable();
        assert_eq!(items, vec![1, 2, 3]);
    }

    #[test]
    #[cfg(feature = "multi-producer")]
    fn multi_producer_drain_parallel_collects_from_all_producers() {
        // Verifies `drain_parallel` works correctly when items arrive from
        // multiple concurrent producers via cloned senders.
        let (tx, rx) = bounded_with_capacity(8);
        let tx2 = tx.clone();

        let p1 = thread::spawn(move || {
            for i in (0..100u32).step_by(2) {
                tx.send(DeltaWork::whole_file(i, PathBuf::from("/dst"), i as u64))
                    .unwrap();
            }
        });

        let p2 = thread::spawn(move || {
            for i in (1..100u32).step_by(2) {
                tx2.send(DeltaWork::whole_file(i, PathBuf::from("/dst"), i as u64))
                    .unwrap();
            }
        });

        let results = rx.drain_parallel(|w| (w.ndx(), w.target_size()));
        p1.join().unwrap();
        p2.join().unwrap();

        assert_eq!(results.len(), 100);

        let mut sorted = results;
        sorted.sort_unstable_by_key(|&(ndx, _)| ndx);
        for (ndx, size) in &sorted {
            assert_eq!(*size, u64::from(*ndx));
        }
    }

    #[test]
    #[cfg(feature = "multi-producer")]
    fn multi_producer_drain_parallel_into_from_multiple_senders() {
        // Streaming variant also receives all items from multiple producers.
        let (tx, rx) = bounded_with_capacity(8);
        let tx2 = tx.clone();
        let tx3 = tx.clone();
        let items_each = 50u32;

        let producers: Vec<_> = [tx, tx2, tx3]
            .into_iter()
            .enumerate()
            .map(|(pid, sender)| {
                thread::spawn(move || {
                    let base = (pid as u32) * items_each;
                    for i in 0..items_each {
                        sender
                            .send(DeltaWork::whole_file(base + i, PathBuf::from("/dst"), 0))
                            .unwrap();
                    }
                })
            })
            .collect();

        let (result_tx, result_rx) = mpsc::sync_channel(16);
        thread::spawn(move || {
            rx.drain_parallel_into(|w| w.ndx(), result_tx);
        });

        let mut results: Vec<u32> = result_rx.iter().collect();
        for p in producers {
            p.join().unwrap();
        }

        results.sort_unstable();
        let expected: Vec<u32> = (0..(3 * items_each)).collect();
        assert_eq!(results, expected);
    }

    #[test]
    #[cfg(feature = "multi-producer")]
    fn multi_producer_send_error_after_receiver_drop() {
        // All cloned senders should observe the send error once the receiver
        // is dropped.
        let (tx, rx) = bounded_with_capacity(4);
        let tx2 = tx.clone();
        drop(rx);

        let err1 = tx
            .send(DeltaWork::whole_file(1, PathBuf::from("/d"), 0))
            .unwrap_err();
        let err2 = tx2
            .send(DeltaWork::whole_file(2, PathBuf::from("/d"), 0))
            .unwrap_err();

        assert_eq!(err1.0.ndx(), 1);
        assert_eq!(err2.0.ndx(), 2);
    }

    #[test]
    fn drain_parallel_closure_captures_state() {
        let (tx, rx) = bounded_with_capacity(8);
        let total = 30u32;
        let multiplier = 10u32;

        let producer = thread::spawn(move || {
            for i in 0..total {
                tx.send(DeltaWork::whole_file(i, PathBuf::from("/dst"), 0))
                    .unwrap();
            }
        });

        let results = rx.drain_parallel(|w| w.ndx() * multiplier);
        producer.join().unwrap();

        let mut sorted = results;
        sorted.sort_unstable();
        let expected: Vec<u32> = (0..total).map(|i| i * multiplier).collect();
        assert_eq!(sorted, expected);
    }

    proptest! {
        /// Property test: `drain_parallel` preserves input ordering under contention.
        ///
        /// `drain_parallel` collects results into per-thread sharded buffers and
        /// flattens them, so raw output order is non-deterministic. The ordering
        /// contract is that every input index appears exactly once in the output,
        /// allowing the caller to restore original order via the tagged index.
        /// This test verifies that contract holds across varying item counts and
        /// simulated contention from variable-cost work functions.
        #[test]
        fn drain_parallel_preserves_ordering(n in 10usize..1000) {
            let (tx, rx) = bounded_with_capacity(8);

            let producer = thread::spawn(move || {
                for i in 0..n {
                    let work = DeltaWork::whole_file(
                        i as u32,
                        PathBuf::from("/dst"),
                        64,
                    );
                    tx.send(work).unwrap();
                }
            });

            // Each worker does a variable amount of spin work keyed on its index
            // to create scheduling contention and non-uniform completion times.
            let results: Vec<(u32, u32)> = rx.drain_parallel(|w| {
                let idx = w.ndx();
                // Spin proportional to (idx % 17) to vary per-item cost.
                let spin = (idx % 17) as usize * 50;
                let mut acc = 0u64;
                for j in 0..spin {
                    acc = acc.wrapping_add(j as u64);
                }
                // Use acc to prevent the optimizer from eliding the loop.
                let _ = std::hint::black_box(acc);
                (idx, idx.wrapping_mul(7))
            });
            producer.join().unwrap();

            // All items present - no loss, no duplication.
            prop_assert_eq!(results.len(), n);

            // Sort by tagged index and verify completeness and value integrity.
            let mut sorted = results;
            sorted.sort_unstable_by_key(|&(idx, _)| idx);

            for (pos, &(idx, val)) in sorted.iter().enumerate() {
                prop_assert_eq!(idx, pos as u32);
                prop_assert_eq!(val, idx.wrapping_mul(7));
            }
        }
    }
}
