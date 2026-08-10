//! Bounded sliding-window reorder buffer with backpressure.
//!
//! Implements a classic sliding-window protocol (analogous to TCP's receive
//! window) adapted for in-process reordering of out-of-order delta results.
//! The window bounds memory usage to O(W) items regardless of total transfer
//! size, making it safe for 100K+ file transfers.
//!
//! # Algorithm
//!
//! The buffer maintains a cursor (`next_expected`) and accepts items with
//! sequence numbers in the half-open range `[next_expected, next_expected + W)`.
//! Items outside this window are rejected with a backpressure signal, telling
//! the producer to slow down.
//!
//! When the item at `next_expected` arrives, all consecutive items are drained
//! in a single pass, advancing the window and freeing capacity for new inserts.
//!
//! # Upstream Reference
//!
//! upstream: receiver.c:recv_files() processes files in file-list order.
//! This buffer restores sequential ordering after parallel delta dispatch.

use std::collections::BTreeMap;
use std::time::Instant;

mod drain;
mod insert;
mod state;

#[cfg(test)]
mod tests;

/// Default window size for the bounded reorder buffer.
pub const DEFAULT_WINDOW_SIZE: u64 = 64;

/// Snapshot of reorder buffer stall and depth metrics.
///
/// Returned by [`BoundedReorderBuffer::metrics`]. All counters are monotonically
/// non-decreasing over the buffer's lifetime except `current_depth`, which
/// tracks the instantaneous queue depth.
///
/// The stall duration measures head-of-line blocking: how long items waited
/// because their predecessor had not yet arrived. High stall counts with
/// large mean durations indicate the parallel dispatch is bottlenecked on
/// a slow straggler, and the parallel threshold or window size may need tuning.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReorderBufferStats {
    /// Number of items currently buffered (awaiting delivery).
    pub current_depth: u64,
    /// High-water mark of buffered items across the buffer's lifetime.
    pub peak_depth: u64,
    /// Number of distinct stall episodes. A stall begins when a non-head
    /// item is inserted into an empty buffer (the head slot is missing),
    /// and ends when `next_expected` advances (the head arrives and drains).
    pub stall_count: u64,
    /// Cumulative wall-clock nanoseconds spent in stall episodes.
    pub total_stall_nanos: u64,
    /// Total number of items delivered in-order from the buffer.
    pub items_delivered: u64,
}

impl ReorderBufferStats {
    /// Returns the mean stall duration in nanoseconds.
    ///
    /// Returns zero when no stalls have occurred, avoiding division by zero.
    #[must_use]
    pub const fn mean_stall_nanos(&self) -> u64 {
        if self.stall_count == 0 {
            0
        } else {
            self.total_stall_nanos / self.stall_count
        }
    }
}

/// Clock function type used by the reorder buffer for stall timing.
///
/// Defaults to [`Instant::now`]. Tests can inject a deterministic clock
/// via [`BoundedReorderBuffer::with_clock`] for reproducible stall
/// duration assertions.
pub(crate) type ClockFn = Box<dyn Fn() -> Instant + Send>;

/// Bounded sliding-window reorder buffer with backpressure.
///
/// Guarantees in-order delivery while bounding memory to at most
/// `window_size` buffered items. Producers that exceed the window
/// receive a backpressure signal to throttle submission.
///
/// Tracks stall-duration and queue-depth metrics for pipeline tuning.
/// Use [`metrics`](Self::metrics) to retrieve a snapshot of counters.
///
/// # Invariant
///
/// All keys `k` in `pending` satisfy: `next_expected <= k < next_expected + window_size`.
///
/// # Examples
///
/// ```
/// use transfer::reorder_buffer::BoundedReorderBuffer;
///
/// let mut buf: BoundedReorderBuffer<&str> = BoundedReorderBuffer::new(4);
///
/// // Out-of-order arrival: 2, 1, 0
/// let drained = buf.insert(2, "c").unwrap();
/// assert!(drained.is_empty());
///
/// let drained = buf.insert(1, "b").unwrap();
/// assert!(drained.is_empty());
///
/// let drained = buf.insert(0, "a").unwrap();
/// assert_eq!(drained, vec!["a", "b", "c"]);
/// ```
#[must_use]
pub struct BoundedReorderBuffer<T> {
    /// Next sequence number expected for in-order delivery.
    pub(crate) next_expected: u64,
    /// Maximum number of items ahead of `next_expected` that can be buffered.
    pub(crate) window_size: u64,
    /// Out-of-order items waiting for gaps to fill.
    pub(crate) pending: BTreeMap<u64, T>,
    /// High-water mark of `pending.len()` across the buffer's lifetime.
    pub(crate) peak_depth: u64,
    /// Number of distinct stall episodes.
    pub(crate) stall_count: u64,
    /// Cumulative nanoseconds spent in stall episodes.
    pub(crate) total_stall_nanos: u64,
    /// Total items delivered via `drain_consecutive`.
    pub(crate) items_delivered: u64,
    /// Start instant of the current stall episode, if any.
    pub(crate) stall_start: Option<Instant>,
    /// Clock source for stall timing.
    pub(crate) clock: ClockFn,
    /// When `true`, items pass through without sequence-based reordering.
    ///
    /// In bypass mode, [`insert`](Self::insert) returns items immediately.
    /// Sequence numbers are ignored. This eliminates BTreeMap overhead when
    /// strict ordering is unnecessary (e.g., `--delay-updates` is off and
    /// files are committed immediately).
    pub(crate) bypass: bool,
}

/// Items drained from the buffer in sequence order.
///
/// Returned by [`BoundedReorderBuffer::insert`] when insertion completes
/// a contiguous run starting at `next_expected`.
pub type DrainedItems<T> = Vec<T>;

/// Error returned when the requested sequence number falls outside the
/// current acceptance window.
///
/// This signals backpressure: the producer should wait until the window
/// advances (i.e., earlier sequence numbers are inserted and drained)
/// before retrying.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackpressureError {
    /// The sequence number that was rejected.
    pub sequence: u64,
    /// The current window start (next expected sequence).
    pub window_start: u64,
    /// The current window end (exclusive).
    pub window_end: u64,
}

impl std::fmt::Display for BackpressureError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "sequence {} outside window [{}, {})",
            self.sequence, self.window_start, self.window_end
        )
    }
}

impl std::error::Error for BackpressureError {}

impl<T: std::fmt::Debug> std::fmt::Debug for BoundedReorderBuffer<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BoundedReorderBuffer")
            .field("next_expected", &self.next_expected)
            .field("window_size", &self.window_size)
            .field("pending", &self.pending)
            .field("peak_depth", &self.peak_depth)
            .field("stall_count", &self.stall_count)
            .field("total_stall_nanos", &self.total_stall_nanos)
            .field("items_delivered", &self.items_delivered)
            .field("stall_active", &self.stall_start.is_some())
            .field("bypass", &self.bypass)
            .finish()
    }
}
