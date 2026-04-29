//! Core bounded channel types for the work queue.
//!
//! Defines the [`WorkQueueSender`], [`WorkQueueReceiver`], and [`SendError`]
//! types plus the [`bounded`] / [`bounded_with_capacity`] constructors. Parallel
//! consumption is implemented in [`super::drain`], iterator support in
//! [`super::iter`], and capacity policy in [`super::capacity`].

use crossbeam_channel::{self as channel, Receiver, Sender};

use super::capacity::CAPACITY_MULTIPLIER;
use crate::concurrent_delta::DeltaWork;

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
/// let results: Vec<u32> = rx.drain_parallel(|w| w.ndx().get());
/// ```
pub struct WorkQueueSender {
    pub(super) tx: Sender<DeltaWork>,
}

/// Receiving half of the bounded work queue.
///
/// Implements [`Iterator`] so it can be consumed in a `rayon::scope` loop
/// that spawns one task per item for parallel processing. For convenience,
/// [`drain_parallel`](Self::drain_parallel) encapsulates the `rayon::scope`
/// pattern into a single method call.
pub struct WorkQueueReceiver {
    pub(super) rx: Receiver<DeltaWork>,
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
    let (tx, rx) = channel::bounded(capacity);
    (WorkQueueSender { tx }, WorkQueueReceiver { rx })
}
