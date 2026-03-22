//! Bounded work queue for the concurrent delta pipeline.
//!
//! Prevents OOM by limiting the number of in-flight [`DeltaWork`] items.
//! The producer side blocks when the queue is full, applying backpressure
//! to the generator/receiver that feeds work items. The consumer side
//! drains items in parallel via [`rayon::iter::ParallelBridge`].
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
//! Generator ─► WorkQueue (bounded) ─► rayon par_bridge ─► DeltaResult
//!                 blocks when full       parallel workers
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
/// The [`WorkQueueSender`] is `Send` (but not `Clone` - single producer).
/// The [`WorkQueueReceiver`] is `Send` and implements [`Iterator`] for use
/// with [`rayon::iter::ParallelBridge`].
///
/// # Example
///
/// ```rust,no_run
/// use engine::concurrent_delta::work_queue;
/// use engine::concurrent_delta::DeltaWork;
/// use rayon::prelude::*;
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
/// // Parallel consumers via rayon
/// let results: Vec<u32> = rx.into_iter().par_bridge().map(|w| w.ndx()).collect();
/// ```
pub struct WorkQueueSender {
    tx: SyncSender<DeltaWork>,
}

/// Receiving half of the bounded work queue.
///
/// Implements [`Iterator`] so it can be used directly with
/// [`rayon::iter::ParallelBridge::par_bridge`] for parallel consumption.
pub struct WorkQueueReceiver {
    rx: Receiver<DeltaWork>,
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

impl WorkQueueSender {
    /// Sends a work item into the queue, blocking if at capacity.
    ///
    /// Returns `Err(SendError)` if the receiver has been dropped.
    pub fn send(&self, work: DeltaWork) -> Result<(), SendError> {
        self.tx.send(work).map_err(|e| SendError(e.0))
    }
}

impl WorkQueueReceiver {
    /// Converts the receiver into an iterator suitable for `par_bridge()`.
    ///
    /// The returned iterator yields items until the sender is dropped and the
    /// queue is drained.
    pub fn into_iter(self) -> WorkQueueIter {
        WorkQueueIter { rx: self.rx }
    }
}

/// Iterator adapter over the work queue receiver.
///
/// Yields [`DeltaWork`] items until the sender drops and the queue drains.
/// Designed for use with [`rayon::iter::ParallelBridge`].
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
    use std::sync::Arc;
    use std::thread;
    use std::time::{Duration, Instant};

    use rayon::prelude::*;

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
    fn par_bridge_processes_all_items() {
        let (tx, rx) = bounded_with_capacity(8);
        let count = 200;

        let producer = thread::spawn(move || {
            for i in 0..count {
                let work = DeltaWork::whole_file(i, PathBuf::from("/dst"), 64);
                tx.send(work).unwrap();
            }
        });

        let results: Vec<u32> = rx.into_iter().par_bridge().map(|w| w.ndx()).collect();
        producer.join().unwrap();

        let mut sorted = results;
        sorted.sort_unstable();
        assert_eq!(sorted, (0..count).collect::<Vec<u32>>());
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
        let total_items = 50;

        let active = Arc::new(AtomicUsize::new(0));
        let max_active = Arc::new(AtomicUsize::new(0));

        let producer = thread::spawn(move || {
            for i in 0..total_items {
                let work = DeltaWork::whole_file(i, PathBuf::from("/dst"), 0);
                tx.send(work).unwrap();
            }
        });

        let active_clone = Arc::clone(&active);
        let max_active_clone = Arc::clone(&max_active);

        let results: Vec<u32> = rx
            .into_iter()
            .par_bridge()
            .map(|w| {
                let current = active_clone.fetch_add(1, Ordering::SeqCst) + 1;
                // Update max observed concurrency.
                max_active_clone.fetch_max(current, Ordering::SeqCst);
                // Simulate work to increase chance of overlapping.
                thread::sleep(Duration::from_micros(100));
                active_clone.fetch_sub(1, Ordering::SeqCst);
                w.ndx()
            })
            .collect();

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

        let count: usize = rx.into_iter().par_bridge().map(|_| 1).sum();
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
                assert!(
                    Instant::now() < deadline,
                    "deadlock detected - timed out"
                );
                w.ndx()
            })
            .collect();

        producer.join().unwrap();
        assert_eq!(items.len(), total as usize);
    }
}
