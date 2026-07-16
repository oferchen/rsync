//! Core bounded channel types for the work queue.
//!
//! Defines the [`WorkQueueSender`], [`WorkQueueReceiver`], and [`SendError`]
//! types plus the [`bounded`] / [`bounded_with_capacity`] constructors. Parallel
//! consumption is implemented in [`super::drain`], iterator support in
//! [`super::iter`], and capacity policy in [`super::capacity`].

use std::sync::Arc;

use crossbeam_channel::{self as channel, Receiver, Sender};

use super::adaptive_semaphore::{AdaptiveSemaphore, ResizeError};
use super::capacity::CAPACITY_MULTIPLIER;
use super::capacity_source::CapacitySource;
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
    /// Where admission capacity comes from.
    ///
    /// [`CapacitySource::Fixed`] preserves the original behaviour (the channel
    /// bound is the only gate); [`CapacitySource::Dynamic`] gates admission
    /// through a resizable [`AdaptiveSemaphore`].
    pub(super) capacity: CapacitySource,
}

/// Receiving half of the bounded work queue.
///
/// Implements [`Iterator`] so it can be consumed in a `rayon::scope` loop
/// that spawns one task per item for parallel processing. For convenience,
/// [`drain_parallel`](Self::drain_parallel) encapsulates the `rayon::scope`
/// pattern into a single method call.
pub struct WorkQueueReceiver {
    pub(super) rx: Receiver<DeltaWork>,
    /// Admission semaphore to return a permit to when a work item finishes
    /// draining, completing the dynamic queue's acquire/release protocol.
    ///
    /// [`WorkQueueSender::send`] acquires exactly one permit per item on the
    /// [`CapacitySource::Dynamic`] path; the consumer must return exactly one
    /// permit per drained item or the single producer eventually blocks
    /// forever on admission. `None` for a [`CapacitySource::Fixed`] queue,
    /// whose backpressure comes solely from the bounded channel and therefore
    /// needs no explicit release.
    pub(super) release: Option<Arc<AdaptiveSemaphore>>,
}

/// RAII guard that returns exactly one admission permit to a dynamic work
/// queue's [`AdaptiveSemaphore`] when dropped.
///
/// Completes the dynamic queue's acquire/release protocol: for every item
/// [`WorkQueueSender::send`] admits, the consumer moves one guard into the
/// rayon task that processes that item. Releasing on `Drop` makes the pairing
/// exception-safe - a panic in the delta-dispatch closure, an early return, or
/// a dropped result channel still returns the permit as the task unwinds, so a
/// failed item can never leak admission capacity and wedge the producer. A
/// `None` inner is a no-op, so the fixed-capacity path pays nothing.
///
/// The guard drops at the end of the task closure, i.e. after the result has
/// been forwarded downstream. Holding the permit across that forward is
/// deliberate: while a worker is blocked handing its result to a saturated
/// consumer, admission stays closed, which is exactly the backpressure signal
/// the [`AdaptiveQueueController`](super::controller::AdaptiveQueueController)
/// reads to shrink the queue.
pub(super) struct PermitGuard(pub(super) Option<Arc<AdaptiveSemaphore>>);

impl Drop for PermitGuard {
    fn drop(&mut self) {
        if let Some(semaphore) = &self.0 {
            semaphore.release();
        }
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

impl WorkQueueSender {
    /// Sends a work item into the queue, blocking if at capacity.
    ///
    /// Returns `Err(SendError)` if the receiver has been dropped.
    ///
    /// For a `CapacitySource::Fixed` queue (the default), admission is gated
    /// solely by the bounded channel, so this is identical to a plain channel
    /// send. For a dynamic queue, admission is first gated by the backing
    /// [`AdaptiveSemaphore`] before the item enters the (over-provisioned)
    /// channel.
    pub fn send(&self, work: DeltaWork) -> Result<(), SendError> {
        self.capacity.acquire();
        self.tx.send(work).map_err(|e| SendError(e.0))
    }

    /// Returns the current admission capacity ceiling.
    ///
    /// For a fixed-bound queue this is the channel capacity fixed at
    /// construction. For a dynamic queue this is the semaphore's current
    /// ceiling, which may sit anywhere in `[min, max]`.
    #[must_use]
    pub fn current_capacity(&self) -> usize {
        match &self.capacity {
            CapacitySource::Fixed => self.tx.capacity().unwrap_or(0),
            CapacitySource::Dynamic { semaphore, .. } => semaphore.current_cap(),
        }
    }

    /// Returns the configured `[min, max]` admission ceiling range.
    ///
    /// Returns `None` for a fixed-bound queue, whose capacity does not move. For
    /// a dynamic queue it returns the `(min, max)` the ceiling may be resized
    /// between, as set by [`bounded_dynamic`]. A controller reads these to clamp
    /// any grow/shrink decision to the configured range.
    #[must_use]
    pub fn capacity_bounds(&self) -> Option<(usize, usize)> {
        match &self.capacity {
            CapacitySource::Fixed => None,
            CapacitySource::Dynamic { min, max, .. } => Some((*min, *max)),
        }
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
    (
        WorkQueueSender {
            tx,
            capacity: CapacitySource::Fixed,
        },
        WorkQueueReceiver { rx, release: None },
    )
}

/// Handle to a dynamic work queue's resizable admission semaphore.
///
/// Bundles the [`WorkQueueSender`]/[`WorkQueueReceiver`] pair produced by
/// [`bounded_dynamic`] with a shared reference to the backing
/// [`AdaptiveSemaphore`]. An
/// [`AdaptiveQueueController`](super::controller::AdaptiveQueueController) grows
/// or shrinks the admission ceiling between `min` and `max` in response to the
/// observed block rate; the shared semaphore is also exposed directly so tests
/// can drive resizes without a controller.
pub struct DynamicWorkQueue {
    /// Producer half of the dynamic work queue.
    pub sender: WorkQueueSender,
    /// Consumer half of the dynamic work queue.
    pub receiver: WorkQueueReceiver,
    /// Shared admission semaphore whose ceiling may move within `[min, max]`.
    pub semaphore: Arc<AdaptiveSemaphore>,
}

/// Creates a work queue whose admission capacity is a dynamic source.
///
/// Admission is gated by an [`AdaptiveSemaphore`] starting at `initial` permits
/// and resizable at runtime between `min` and `max`. The underlying channel is
/// opened at `max` capacity so the semaphore, not the channel, is always the
/// binding constraint. Growing the semaphore ceiling immediately admits more
/// in-flight work; shrinking it withholds future admissions without revoking
/// permits already granted.
///
/// The returned [`WorkQueueReceiver`] carries a clone of the semaphore and
/// returns one permit per drained item (see `PermitGuard` and the drain
/// methods), so admission refills as work completes. An
/// [`AdaptiveQueueController`](super::controller::AdaptiveQueueController) can
/// then move the ceiling within `[min, max]` in response to the observed block
/// rate. The returned [`DynamicWorkQueue`] also exposes the semaphore directly
/// so tests can drive resizes without a controller. Fixed-bound queues built
/// via [`bounded`] / [`bounded_with_capacity`] are entirely unaffected.
///
/// # Errors
///
/// Returns [`ResizeError`] if `initial`, `min`, or `max` falls outside the
/// semaphore's permitted range, or if the range is inconsistent
/// (`min > max`, or `initial` outside `[min, max]`).
pub fn bounded_dynamic(
    initial: usize,
    min: usize,
    max: usize,
) -> Result<DynamicWorkQueue, ResizeError> {
    // `AdaptiveSemaphore::new` validates each bound against the global
    // [MIN_CAPACITY, MAX_CAPACITY] range; validate the relative ordering here so
    // an inconsistent (initial, min, max) triple is rejected up front.
    if min > max {
        return Err(ResizeError::AboveMax {
            requested: min,
            max,
        });
    }
    if initial < min {
        return Err(ResizeError::BelowMin {
            requested: initial,
            min,
        });
    }
    if initial > max {
        return Err(ResizeError::AboveMax {
            requested: initial,
            max,
        });
    }

    let semaphore = Arc::new(AdaptiveSemaphore::new(initial)?);
    // Open the channel at `max` so it never gates admission below the
    // semaphore's ceiling; the semaphore is the sole admission control.
    let (tx, rx) = channel::bounded(max);
    let sender = WorkQueueSender {
        tx,
        capacity: CapacitySource::Dynamic {
            semaphore: Arc::clone(&semaphore),
            min,
            max,
        },
    };
    Ok(DynamicWorkQueue {
        sender,
        receiver: WorkQueueReceiver {
            rx,
            release: Some(Arc::clone(&semaphore)),
        },
        semaphore,
    })
}
