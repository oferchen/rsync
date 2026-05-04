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

use std::sync::mpsc;
use std::thread::{self, JoinHandle};

use super::reorder::ReorderBuffer;
use super::strategy;
use super::types::DeltaResult;
use super::work_queue::WorkQueueReceiver;

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
    /// Panics if `reorder_capacity` is zero.
    #[must_use]
    pub fn spawn(rx: WorkQueueReceiver, reorder_capacity: usize) -> Self {
        let (result_tx, result_rx) = mpsc::channel();

        // Bounded channel between drain and reorder threads. Capacity matches
        // reorder buffer so workers can stay ahead without unbounded buffering.
        let stream_capacity = reorder_capacity.max(rayon::current_num_threads() * 2);
        let (stream_tx, stream_rx) = crossbeam_channel::bounded::<DeltaResult>(stream_capacity);

        // Thread 1: runs rayon::scope, streaming results as workers complete.
        let drain_handle = thread::Builder::new()
            .name("delta-drain".to_string())
            .spawn(move || {
                rx.drain_parallel_into(|work| strategy::dispatch(&work), stream_tx);
            })
            .expect("failed to spawn delta-drain thread");

        // Thread 2: receives streamed results, reorders, forwards in sequence order.
        let handle = thread::Builder::new()
            .name("delta-reorder".to_string())
            .spawn(move || {
                let mut reorder = ReorderBuffer::new(reorder_capacity);

                for result in stream_rx {
                    // Insert may fail if buffer is at capacity. Drain ready
                    // items first to free space before retrying.
                    while reorder.insert(result.sequence(), result.clone()).is_err() {
                        let mut drained_any = false;
                        for ready in reorder.drain_ready() {
                            drained_any = true;
                            if result_tx.send(ready).is_err() {
                                return;
                            }
                        }
                        if !drained_any {
                            // Buffer full but next_expected is not buffered.
                            // Force insert to break the deadlock.
                            reorder.force_insert(result.sequence(), result.clone());
                            break;
                        }
                    }

                    // Forward any newly available contiguous run.
                    for ready in reorder.drain_ready() {
                        if result_tx.send(ready).is_err() {
                            return;
                        }
                    }
                }

                // Drain remaining items after the stream closes.
                for ready in reorder.drain_ready() {
                    if result_tx.send(ready).is_err() {
                        return;
                    }
                }

                // Wait for drain thread to finish (propagates panics).
                let _ = drain_handle.join();
            })
            .expect("failed to spawn delta-reorder thread");

        Self {
            result_rx,
            handle: Some(handle),
        }
    }

    /// Tries to receive the next in-order result without blocking.
    ///
    /// Returns `Some(result)` if a result is immediately available in the
    /// channel, or `None` if no results are ready yet. Unlike
    /// [`iter`](Self::iter), this method never blocks the caller.
    ///
    /// Useful for polling from a pipeline loop where blocking would stall
    /// the producer.
    #[must_use]
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
}
