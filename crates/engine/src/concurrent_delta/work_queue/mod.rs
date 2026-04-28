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
//! Supporting multiple producers would require revising the ordering contract (multiple
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
//! For streaming pipelines, [`WorkQueueReceiver::drain_parallel_into`] sends
//! results through a channel as workers complete, enabling incremental
//! consumption without waiting for all items to finish:
//!
//! ```text
//! Generator ─► WorkQueue ─► drain_parallel_into(f, tx) ─► Sender<R>
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
//! let ndx_list: Vec<u32> = rx.drain_parallel(|w| w.ndx().get());
//! assert_eq!(ndx_list.len(), 100);
//! ```
//!
//! # Upstream Reference
//!
//! Upstream rsync processes files sequentially in `recv_files()`. This queue
//! enables parallel processing while bounding memory - upstream does not need
//! this because it never queues ahead.

mod bounded;
mod capacity;
mod drain;
mod iter;
#[cfg(feature = "multi-producer")]
mod multi_producer;

pub use bounded::{SendError, WorkQueueReceiver, WorkQueueSender, bounded, bounded_with_capacity};
pub use capacity::{adaptive_queue_depth, default_capacity};
pub use iter::WorkQueueIter;

#[cfg(test)]
mod tests;
