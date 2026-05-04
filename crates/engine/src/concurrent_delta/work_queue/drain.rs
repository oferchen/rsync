//! Parallel drain implementations for [`WorkQueueReceiver`].
//!
//! Provides [`drain_parallel`](WorkQueueReceiver::drain_parallel) for collecting
//! results into a `Vec` and [`drain_parallel_into`](WorkQueueReceiver::drain_parallel_into)
//! for streaming results through a channel as workers complete. Both share a
//! `rayon::scope` based dispatch pattern with per-thread sharded buffers to
//! minimise contention across the rayon thread pool.

use crossbeam_channel::Sender;

use super::bounded::WorkQueueReceiver;
use crate::concurrent_delta::DeltaWork;

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
    /// [`ReorderBuffer`]: crate::concurrent_delta::reorder::ReorderBuffer
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
    /// let indices: Vec<u32> = rx.drain_parallel(|w| w.ndx().get());
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
    /// The bounded `Sender` provides backpressure: if the consumer falls behind,
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
    ///
    /// let (work_tx, work_rx) = work_queue::bounded();
    /// let (result_tx, result_rx) = crossbeam_channel::bounded(16);
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
    ///     work_rx.drain_parallel_into(|w| w.ndx().get(), result_tx);
    /// });
    ///
    /// // Consumer thread - processes results incrementally
    /// for ndx in result_rx {
    ///     // Process each result as it arrives
    /// }
    /// ```
    pub fn drain_parallel_into<F, R>(self, f: F, tx: Sender<R>)
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
