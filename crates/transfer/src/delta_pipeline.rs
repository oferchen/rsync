//! Delta pipeline trait and implementations for receiver file processing.
//!
//! Defines the [`ReceiverDeltaPipeline`] trait that abstracts how file delta work
//! items are dispatched and results collected. Two implementations exist:
//!
//! - [`SequentialDeltaPipeline`] - Processes files one at a time in the caller's
//!   thread (matches upstream `recv_files()` behavior).
//! - [`ParallelDeltaPipeline`] - Dispatches work items through a bounded
//!   [`WorkQueueSender`] to rayon workers, collecting results via a crossbeam
//!   channel and reordering them with [`ReorderBuffer`].
//!
//! # Upstream Reference
//!
//! Upstream rsync's `receiver.c:recv_files()` processes files sequentially.
//! The parallel pipeline preserves this ordering guarantee via the reorder buffer
//! while enabling concurrent I/O across multiple files.

use engine::concurrent_delta::reorder::ReorderBuffer;
use engine::concurrent_delta::types::{DeltaResult, DeltaWork};
use engine::concurrent_delta::work_queue::{self, SendError, WorkQueueSender};

use std::sync::Arc;
use std::thread;

/// Abstracts delta work dispatch and result collection for the receiver.
///
/// Implementations control whether file processing happens sequentially or in
/// parallel. The receiver loop calls [`submit_work`](Self::submit_work) for each
/// file and [`poll_result`](Self::poll_result) to retrieve completed results in
/// order. [`flush`](Self::flush) signals that no more work will be submitted and
/// drains any remaining buffered results.
pub trait ReceiverDeltaPipeline: Send {
    /// Submits a work item for processing.
    ///
    /// For sequential pipelines, this processes the item immediately and buffers
    /// the result. For parallel pipelines, this dispatches to a worker pool.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if the pipeline has been shut down or the work
    /// cannot be accepted.
    fn submit_work(&mut self, work: DeltaWork) -> std::io::Result<()>;

    /// Retrieves the next completed result in submission order.
    ///
    /// Returns `None` if no result is ready yet (the next expected sequence
    /// has not completed). Callers should call this after each `submit_work`
    /// to drain any newly-ready results.
    fn poll_result(&mut self) -> Option<DeltaResult>;

    /// Signals that no more work will be submitted and drains remaining results.
    ///
    /// After calling `flush`, [`poll_result`](Self::poll_result) yields all
    /// remaining results in order until exhausted. Returns the count of results
    /// drained during the flush operation.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if draining fails.
    fn flush(&mut self) -> std::io::Result<Vec<DeltaResult>>;
}

/// Sequential delta pipeline that processes files in the caller's thread.
///
/// Each call to [`submit_work`](ReceiverDeltaPipeline::submit_work) invokes the
/// provided processing function immediately and buffers the result for retrieval
/// via [`poll_result`](ReceiverDeltaPipeline::poll_result).
///
/// This is the default pipeline matching upstream rsync's sequential behavior.
pub struct SequentialDeltaPipeline<F> {
    /// Processing function applied to each work item.
    process_fn: F,
    /// Results waiting to be polled, stored in submission order.
    results: std::collections::VecDeque<DeltaResult>,
    /// Next sequence number to assign.
    next_sequence: u64,
}

impl<F> SequentialDeltaPipeline<F>
where
    F: FnMut(&DeltaWork) -> DeltaResult + Send,
{
    /// Creates a new sequential pipeline with the given processing function.
    pub fn new(process_fn: F) -> Self {
        Self {
            process_fn,
            results: std::collections::VecDeque::new(),
            next_sequence: 0,
        }
    }
}

impl<F> ReceiverDeltaPipeline for SequentialDeltaPipeline<F>
where
    F: FnMut(&DeltaWork) -> DeltaResult + Send,
{
    fn submit_work(&mut self, mut work: DeltaWork) -> std::io::Result<()> {
        work.set_sequence(self.next_sequence);
        let result = (self.process_fn)(&work).with_sequence(self.next_sequence);
        self.next_sequence += 1;
        self.results.push_back(result);
        Ok(())
    }

    fn poll_result(&mut self) -> Option<DeltaResult> {
        self.results.pop_front()
    }

    fn flush(&mut self) -> std::io::Result<Vec<DeltaResult>> {
        Ok(self.results.drain(..).collect())
    }
}

/// Parallel delta pipeline that dispatches work to rayon workers via a bounded queue.
///
/// Submits [`DeltaWork`] items through a [`WorkQueueSender`] to a background
/// consumer thread running `rayon::scope`. Results arrive out of order through a
/// crossbeam channel and are reordered by [`ReorderBuffer`] before delivery.
///
/// # Backpressure
///
/// The bounded work queue (capacity = 2x rayon thread count) blocks the producer
/// when workers cannot keep up, preventing unbounded memory growth for transfers
/// with millions of files.
///
/// # Ordering Guarantee
///
/// Despite parallel execution, results are delivered in strict submission order
/// via the [`ReorderBuffer`]. This preserves the invariant that post-processing
/// (checksum verification, metadata commit) sees files in file-list order.
pub struct ParallelDeltaPipeline {
    /// Sender half of the bounded work queue.
    sender: Option<WorkQueueSender>,
    /// Shared ring buffer where workers push completed results.
    result_queue: Arc<crossbeam_queue::ArrayQueue<DeltaResult>>,
    /// Reorder buffer for delivering results in sequence order.
    reorder: ReorderBuffer<DeltaResult>,
    /// Next sequence number to assign to submitted work items.
    next_sequence: u64,
    /// Handle to the background consumer thread.
    worker_handle: Option<thread::JoinHandle<()>>,
}

/// Configuration for the parallel delta pipeline.
#[derive(Debug, Clone)]
pub struct ParallelPipelineConfig {
    /// Maximum number of items in the work queue before backpressure kicks in.
    /// Defaults to `2 * rayon::current_num_threads()`.
    pub queue_capacity: usize,
    /// Maximum number of items the reorder buffer can hold before rejecting inserts.
    /// Should be at least `queue_capacity + rayon::current_num_threads()` to avoid
    /// deadlock when all workers complete simultaneously.
    pub reorder_capacity: usize,
    /// Capacity of the result ring buffer between workers and the consumer.
    pub result_buffer_capacity: usize,
}

impl Default for ParallelPipelineConfig {
    fn default() -> Self {
        let threads = rayon::current_num_threads();
        let queue_cap = threads * 2;
        // Reorder must hold at least all in-flight items (queue + active workers).
        let reorder_cap = queue_cap + threads + 16;
        Self {
            queue_capacity: queue_cap,
            reorder_capacity: reorder_cap,
            result_buffer_capacity: reorder_cap,
        }
    }
}

impl ParallelDeltaPipeline {
    /// Creates a new parallel pipeline with default configuration.
    ///
    /// Spawns a background thread that consumes from the work queue using
    /// `rayon::scope` for parallel processing, pushing results into a shared
    /// ring buffer.
    ///
    /// The `process_fn` is called once per work item in a rayon worker thread.
    /// It must be `Send + Sync + 'static` since it crosses thread boundaries.
    pub fn new<F>(process_fn: F) -> Self
    where
        F: Fn(&DeltaWork) -> DeltaResult + Send + Sync + 'static,
    {
        Self::with_config(process_fn, ParallelPipelineConfig::default())
    }

    /// Creates a new parallel pipeline with explicit configuration.
    ///
    /// See [`ParallelPipelineConfig`] for tuning parameters.
    pub fn with_config<F>(process_fn: F, config: ParallelPipelineConfig) -> Self
    where
        F: Fn(&DeltaWork) -> DeltaResult + Send + Sync + 'static,
    {
        let (tx, rx) = work_queue::bounded_with_capacity(config.queue_capacity);

        // Shared ring buffer for results. Workers push; main thread pops.
        let result_queue =
            Arc::new(crossbeam_queue::ArrayQueue::new(config.result_buffer_capacity));
        let worker_result_queue = Arc::clone(&result_queue);

        let worker_handle = thread::spawn(move || {
            rayon::scope(|s| {
                for work in rx.into_iter() {
                    let f = &process_fn;
                    let queue = &worker_result_queue;
                    s.spawn(move |_| {
                        let seq = work.sequence();
                        let result = f(&work).with_sequence(seq);
                        // Push to ring buffer. In the unlikely event the buffer is
                        // full, spin briefly - the consumer drains frequently.
                        while queue.push(result.clone()).is_err() {
                            std::thread::yield_now();
                        }
                    });
                }
            });
        });

        Self {
            sender: Some(tx),
            result_queue,
            reorder: ReorderBuffer::new(config.reorder_capacity),
            next_sequence: 0,
            worker_handle: Some(worker_handle),
        }
    }

    /// Drains available results from the worker thread into the reorder buffer.
    fn drain_incoming(&mut self) {
        while let Some(result) = self.result_queue.pop() {
            let seq = result.sequence();
            // Capacity exceeded should not happen with properly sized config,
            // but handle gracefully by spinning until space is available.
            while self.reorder.insert(seq, result.clone()).is_err() {
                // Yield ready results to free reorder buffer space.
                if self.reorder.next_in_order().is_some() {
                    // This path should not trigger in normal operation since
                    // we only call drain_incoming from poll_result which
                    // processes results. For safety, break the loop.
                    break;
                }
                std::thread::yield_now();
            }
        }
    }
}

impl ReceiverDeltaPipeline for ParallelDeltaPipeline {
    fn submit_work(&mut self, mut work: DeltaWork) -> std::io::Result<()> {
        work.set_sequence(self.next_sequence);
        self.next_sequence += 1;

        let sender = self.sender.as_ref().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "parallel pipeline has been shut down",
            )
        })?;

        sender.send(work).map_err(|SendError(_)| {
            std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "parallel pipeline worker thread exited unexpectedly",
            )
        })?;

        Ok(())
    }

    fn poll_result(&mut self) -> Option<DeltaResult> {
        self.drain_incoming();
        self.reorder.next_in_order()
    }

    fn flush(&mut self) -> std::io::Result<Vec<DeltaResult>> {
        // Drop the sender to signal completion to the worker thread.
        self.sender.take();

        // Wait for the worker thread to finish processing all items.
        if let Some(handle) = self.worker_handle.take() {
            handle.join().map_err(|_| {
                std::io::Error::new(
                    std::io::ErrorKind::Other,
                    "parallel pipeline worker thread panicked",
                )
            })?;
        }

        // Drain all remaining results from the ring buffer into reorder.
        self.drain_incoming();

        // Collect all ordered results.
        let mut results = Vec::new();
        while let Some(result) = self.reorder.next_in_order() {
            results.push(result);
        }
        Ok(results)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::concurrent_delta::types::{DeltaResult, DeltaWork};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};

    // ==================== SequentialDeltaPipeline tests ====================

    #[test]
    fn sequential_submit_and_poll() {
        let mut pipeline = SequentialDeltaPipeline::new(|w| {
            DeltaResult::success(w.ndx(), w.target_size(), 0, 0)
        });

        pipeline
            .submit_work(DeltaWork::whole_file(1, PathBuf::from("/a"), 100))
            .unwrap();
        pipeline
            .submit_work(DeltaWork::whole_file(2, PathBuf::from("/b"), 200))
            .unwrap();

        let r1 = pipeline.poll_result().unwrap();
        assert_eq!(r1.ndx(), 1);
        assert_eq!(r1.bytes_written(), 100);
        assert_eq!(r1.sequence(), 0);

        let r2 = pipeline.poll_result().unwrap();
        assert_eq!(r2.ndx(), 2);
        assert_eq!(r2.bytes_written(), 200);
        assert_eq!(r2.sequence(), 1);

        assert!(pipeline.poll_result().is_none());
    }

    #[test]
    fn sequential_flush_returns_remaining() {
        let mut pipeline = SequentialDeltaPipeline::new(|w| {
            DeltaResult::success(w.ndx(), w.target_size(), 0, 0)
        });

        pipeline
            .submit_work(DeltaWork::whole_file(10, PathBuf::from("/x"), 500))
            .unwrap();
        pipeline
            .submit_work(DeltaWork::whole_file(20, PathBuf::from("/y"), 600))
            .unwrap();

        let results = pipeline.flush().unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].ndx(), 10);
        assert_eq!(results[1].ndx(), 20);
    }

    #[test]
    fn sequential_empty_poll_returns_none() {
        let mut pipeline =
            SequentialDeltaPipeline::new(|_| DeltaResult::success(0, 0, 0, 0));
        assert!(pipeline.poll_result().is_none());
    }

    #[test]
    fn sequential_flush_empty_returns_empty_vec() {
        let mut pipeline =
            SequentialDeltaPipeline::new(|_| DeltaResult::success(0, 0, 0, 0));
        let results = pipeline.flush().unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn sequential_sequences_are_monotonic() {
        let mut pipeline = SequentialDeltaPipeline::new(|w| {
            DeltaResult::success(w.ndx(), 0, 0, 0)
        });

        for i in 0..10 {
            pipeline
                .submit_work(DeltaWork::whole_file(i, PathBuf::from("/d"), 0))
                .unwrap();
        }

        for expected_seq in 0..10u64 {
            let r = pipeline.poll_result().unwrap();
            assert_eq!(r.sequence(), expected_seq);
        }
    }

    #[test]
    fn sequential_process_fn_receives_correct_work() {
        let mut pipeline = SequentialDeltaPipeline::new(|w| {
            if w.is_delta() {
                DeltaResult::success(w.ndx(), w.target_size(), 0, w.target_size())
            } else {
                DeltaResult::success(w.ndx(), w.target_size(), w.target_size(), 0)
            }
        });

        pipeline
            .submit_work(DeltaWork::whole_file(1, PathBuf::from("/a"), 100))
            .unwrap();
        pipeline
            .submit_work(DeltaWork::delta(
                2,
                PathBuf::from("/b"),
                PathBuf::from("/basis"),
                200,
            ))
            .unwrap();

        let r1 = pipeline.poll_result().unwrap();
        assert_eq!(r1.literal_bytes(), 100); // whole file
        assert_eq!(r1.matched_bytes(), 0);

        let r2 = pipeline.poll_result().unwrap();
        assert_eq!(r2.literal_bytes(), 0); // delta
        assert_eq!(r2.matched_bytes(), 200);
    }

    // ==================== ParallelDeltaPipeline tests ====================

    #[test]
    fn parallel_submit_and_poll_single() {
        let mut pipeline = ParallelDeltaPipeline::new(|w| {
            DeltaResult::success(w.ndx(), w.target_size(), 0, 0)
        });

        pipeline
            .submit_work(DeltaWork::whole_file(1, PathBuf::from("/a"), 100))
            .unwrap();

        let results = pipeline.flush().unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].ndx(), 1);
        assert_eq!(results[0].bytes_written(), 100);
        assert_eq!(results[0].sequence(), 0);
    }

    #[test]
    fn parallel_delivers_results_in_order() {
        let config = ParallelPipelineConfig {
            queue_capacity: 4,
            reorder_capacity: 32,
            result_buffer_capacity: 32,
        };
        let mut pipeline = ParallelDeltaPipeline::with_config(
            |w| {
                // Simulate variable processing time to cause out-of-order completion.
                let delay = if w.ndx() % 2 == 0 { 1 } else { 0 };
                std::thread::sleep(std::time::Duration::from_millis(delay));
                DeltaResult::success(w.ndx(), w.target_size(), 0, 0)
            },
            config,
        );

        let count = 20u32;
        for i in 0..count {
            pipeline
                .submit_work(DeltaWork::whole_file(i, PathBuf::from("/d"), u64::from(i)))
                .unwrap();
        }

        let results = pipeline.flush().unwrap();
        assert_eq!(results.len(), count as usize);

        // Verify strictly ordered by sequence.
        for (i, r) in results.iter().enumerate() {
            assert_eq!(r.sequence(), i as u64, "result {i} has wrong sequence");
            assert_eq!(r.ndx(), i as u32, "result {i} has wrong ndx");
        }
    }

    #[test]
    fn parallel_handles_many_items() {
        let config = ParallelPipelineConfig {
            queue_capacity: 8,
            reorder_capacity: 128,
            result_buffer_capacity: 128,
        };
        let mut pipeline = ParallelDeltaPipeline::with_config(
            |w| DeltaResult::success(w.ndx(), w.target_size(), 0, 0),
            config,
        );

        let total = 500u32;
        for i in 0..total {
            pipeline
                .submit_work(DeltaWork::whole_file(i, PathBuf::from("/d"), 64))
                .unwrap();
        }

        let results = pipeline.flush().unwrap();
        assert_eq!(results.len(), total as usize);

        for (i, r) in results.iter().enumerate() {
            assert_eq!(r.sequence(), i as u64);
            assert_eq!(r.ndx(), i as u32);
        }
    }

    #[test]
    fn parallel_poll_yields_ready_results_incrementally() {
        let config = ParallelPipelineConfig {
            queue_capacity: 4,
            reorder_capacity: 32,
            result_buffer_capacity: 32,
        };
        let mut pipeline = ParallelDeltaPipeline::with_config(
            |w| DeltaResult::success(w.ndx(), 0, 0, 0),
            config,
        );

        // Submit a small batch and poll for results.
        for i in 0..4u32 {
            pipeline
                .submit_work(DeltaWork::whole_file(i, PathBuf::from("/d"), 0))
                .unwrap();
        }

        // Wait a bit for workers to complete.
        std::thread::sleep(std::time::Duration::from_millis(50));

        let mut polled = Vec::new();
        while let Some(r) = pipeline.poll_result() {
            polled.push(r);
        }

        // Submit more and flush the rest.
        for i in 4..8u32 {
            pipeline
                .submit_work(DeltaWork::whole_file(i, PathBuf::from("/d"), 0))
                .unwrap();
        }

        let flushed = pipeline.flush().unwrap();
        polled.extend(flushed);

        assert_eq!(polled.len(), 8);
        for (i, r) in polled.iter().enumerate() {
            assert_eq!(r.sequence(), i as u64);
        }
    }

    #[test]
    fn parallel_process_fn_executes_concurrently() {
        let counter = Arc::new(AtomicU32::new(0));
        let counter_clone = Arc::clone(&counter);

        let config = ParallelPipelineConfig {
            queue_capacity: 8,
            reorder_capacity: 64,
            result_buffer_capacity: 64,
        };
        let mut pipeline = ParallelDeltaPipeline::with_config(
            move |w| {
                counter_clone.fetch_add(1, Ordering::Relaxed);
                DeltaResult::success(w.ndx(), 0, 0, 0)
            },
            config,
        );

        let total = 50u32;
        for i in 0..total {
            pipeline
                .submit_work(DeltaWork::whole_file(i, PathBuf::from("/d"), 0))
                .unwrap();
        }

        let results = pipeline.flush().unwrap();
        assert_eq!(results.len(), total as usize);
        assert_eq!(counter.load(Ordering::Relaxed), total);
    }

    #[test]
    fn parallel_submit_after_flush_returns_error() {
        let mut pipeline =
            ParallelDeltaPipeline::new(|w| DeltaResult::success(w.ndx(), 0, 0, 0));

        pipeline.flush().unwrap();

        let result = pipeline.submit_work(DeltaWork::whole_file(0, PathBuf::from("/d"), 0));
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::BrokenPipe);
    }

    #[test]
    fn parallel_empty_flush_returns_empty_vec() {
        let mut pipeline =
            ParallelDeltaPipeline::new(|w| DeltaResult::success(w.ndx(), 0, 0, 0));

        let results = pipeline.flush().unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn parallel_delta_work_kind_preserved() {
        let mut pipeline = ParallelDeltaPipeline::new(|w| {
            if w.is_delta() {
                DeltaResult::success(w.ndx(), 0, 0, w.target_size())
            } else {
                DeltaResult::success(w.ndx(), 0, w.target_size(), 0)
            }
        });

        pipeline
            .submit_work(DeltaWork::whole_file(0, PathBuf::from("/a"), 100))
            .unwrap();
        pipeline
            .submit_work(DeltaWork::delta(
                1,
                PathBuf::from("/b"),
                PathBuf::from("/basis"),
                200,
            ))
            .unwrap();

        let results = pipeline.flush().unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].literal_bytes(), 100); // whole file
        assert_eq!(results[0].matched_bytes(), 0);
        assert_eq!(results[1].literal_bytes(), 0); // delta
        assert_eq!(results[1].matched_bytes(), 200);
    }

    #[test]
    fn parallel_redo_results_delivered_in_order() {
        let mut pipeline = ParallelDeltaPipeline::new(|w| {
            if w.ndx() % 3 == 0 {
                DeltaResult::needs_redo(w.ndx(), "checksum mismatch".to_string())
            } else {
                DeltaResult::success(w.ndx(), w.target_size(), 0, 0)
            }
        });

        for i in 0..15u32 {
            pipeline
                .submit_work(DeltaWork::whole_file(i, PathBuf::from("/d"), u64::from(i)))
                .unwrap();
        }

        let results = pipeline.flush().unwrap();
        assert_eq!(results.len(), 15);

        for (i, r) in results.iter().enumerate() {
            assert_eq!(r.sequence(), i as u64);
            assert_eq!(r.ndx(), i as u32);
            if i % 3 == 0 {
                assert!(r.needs_retry());
            } else {
                assert!(r.is_success());
            }
        }
    }

    #[test]
    fn parallel_config_custom_capacities() {
        let config = ParallelPipelineConfig {
            queue_capacity: 2,
            reorder_capacity: 16,
            result_buffer_capacity: 16,
        };
        let mut pipeline = ParallelDeltaPipeline::with_config(
            |w| DeltaResult::success(w.ndx(), 0, 0, 0),
            config,
        );

        // Small queue capacity still works under backpressure.
        for i in 0..30u32 {
            pipeline
                .submit_work(DeltaWork::whole_file(i, PathBuf::from("/d"), 0))
                .unwrap();
        }

        let results = pipeline.flush().unwrap();
        assert_eq!(results.len(), 30);
    }

    #[test]
    fn parallel_default_config_is_reasonable() {
        let config = ParallelPipelineConfig::default();
        let threads = rayon::current_num_threads();
        assert_eq!(config.queue_capacity, threads * 2);
        assert!(config.reorder_capacity >= config.queue_capacity + threads);
        assert!(config.result_buffer_capacity > 0);
    }

    // ==================== Trait object tests ====================

    #[test]
    fn trait_object_sequential() {
        let mut pipeline: Box<dyn ReceiverDeltaPipeline> =
            Box::new(SequentialDeltaPipeline::new(|w| {
                DeltaResult::success(w.ndx(), 0, 0, 0)
            }));

        pipeline
            .submit_work(DeltaWork::whole_file(42, PathBuf::from("/d"), 0))
            .unwrap();
        let r = pipeline.poll_result().unwrap();
        assert_eq!(r.ndx(), 42);
    }

    #[test]
    fn trait_object_parallel() {
        let mut pipeline: Box<dyn ReceiverDeltaPipeline> =
            Box::new(ParallelDeltaPipeline::new(|w| {
                DeltaResult::success(w.ndx(), 0, 0, 0)
            }));

        pipeline
            .submit_work(DeltaWork::whole_file(99, PathBuf::from("/d"), 0))
            .unwrap();
        let results = pipeline.flush().unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].ndx(), 99);
    }
}
