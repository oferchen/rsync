//! Pluggable delta dispatch pipeline for the receiver transfer loop.
//!
//! Defines [`ReceiverDeltaPipeline`], a trait that abstracts how the receiver
//! dispatches per-file delta work items and collects their results. This
//! follows the Dependency Inversion principle - the receiver depends on this
//! abstraction rather than a concrete pipeline implementation.
//!
//! # Implementations
//!
//! - [`SequentialDeltaPipeline`] - processes items immediately in the calling
//!   thread with no concurrency. This matches upstream rsync's sequential
//!   `recv_files()` loop in `receiver.c` and is the default.
//! - [`ParallelDeltaPipeline`] - dispatches work items to rayon workers via
//!   a [`WorkQueueSender`](engine::concurrent_delta::work_queue::WorkQueueSender),
//!   collects results through an `mpsc` channel, and reorders them with
//!   [`ReorderBuffer`](engine::concurrent_delta::ReorderBuffer) to preserve
//!   submission order.
//! - [`ThresholdDeltaPipeline`] - auto-selects between sequential and parallel
//!   mode based on the number of submitted items. Below the threshold (default
//!   [`DEFAULT_PARALLEL_THRESHOLD`] = 64), items are processed sequentially.
//!   At or above the threshold, a [`ParallelDeltaPipeline`] is created and all
//!   buffered items are flushed into it.
//!
//! # Upstream Reference
//!
//! Upstream `receiver.c:recv_files()` processes files one at a time in a tight
//! loop. This trait preserves that interface while allowing the dispatch
//! strategy to be swapped for parallel execution without changing the receiver.

use std::io;

use engine::concurrent_delta::{DeltaResult, DeltaWork};

mod parallel;
mod sequential;
mod threshold;

#[cfg(test)]
mod tests;

pub use parallel::ParallelDeltaPipeline;
pub use sequential::SequentialDeltaPipeline;
pub use threshold::ThresholdDeltaPipeline;

/// Default threshold for switching from sequential to parallel dispatch.
///
/// Matches the receiver's default stat threshold from `ParallelThresholds`
/// (64 files). Below this count, the overhead of thread spawning and channel
/// communication exceeds the benefit of parallelism.
pub const DEFAULT_PARALLEL_THRESHOLD: usize = 64;

/// Abstraction over the delta dispatch loop in the receiver.
///
/// The receiver submits [`DeltaWork`] items as it reads file entries from the
/// wire, and polls for [`DeltaResult`] values to drive post-processing
/// (checksum verification, temp-file commit, metadata application).
///
/// Implementations may process work synchronously in [`submit_work`] or
/// dispatch it to background workers and return results asynchronously
/// via [`poll_result`].
///
/// [`submit_work`]: ReceiverDeltaPipeline::submit_work
/// [`poll_result`]: ReceiverDeltaPipeline::poll_result
pub trait ReceiverDeltaPipeline: Send + std::fmt::Debug {
    /// Submits a file for delta processing.
    ///
    /// The implementation may process the work item immediately (sequential)
    /// or enqueue it for background processing (parallel). In either case,
    /// the result becomes available via [`poll_result`](Self::poll_result).
    ///
    /// # Errors
    ///
    /// Returns an I/O error if the work item cannot be accepted - for example,
    /// when a background work queue has been shut down.
    fn submit_work(&mut self, work: DeltaWork) -> io::Result<()>;

    /// Tries to retrieve the next in-order result.
    ///
    /// Returns `Some(result)` if the next result in submission order is
    /// available, or `None` if no results are ready yet. Callers should
    /// poll after each [`submit_work`](Self::submit_work) call and again
    /// during flush to drain all pending results.
    fn poll_result(&mut self) -> Option<DeltaResult>;

    /// Drains all remaining results in submission order.
    ///
    /// Consumes the pipeline and returns any buffered or in-flight results.
    /// After this call, no further work can be submitted.
    fn flush(self: Box<Self>) -> Vec<DeltaResult>;
}
