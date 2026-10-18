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

use std::ops::Deref;
use std::time::Instant;

mod drain;
mod insert;
mod state;

#[cfg(test)]
mod tests;

/// Default window size for the bounded reorder buffer.
pub const DEFAULT_WINDOW_SIZE: u64 = 64;

/// Maximum ring buffer pre-allocation cap to guard against pathological window sizes.
pub(crate) const MAX_RING_CAPACITY: usize = 1 << 20;

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
/// Backed by a flat circular ring buffer (`Vec<Option<T>>`) allocated lazily
/// on the first out-of-order arrival. In-order arrivals bypass the ring
/// entirely and return [`DrainedItems::Single`] on the stack with zero heap
/// allocations.
///
/// Tracks stall-duration and queue-depth metrics for pipeline tuning.
/// Use [`metrics`](Self::metrics) to retrieve a snapshot of counters.
///
/// # Invariant
///
/// All buffered items have sequence numbers `k` in
/// `next_expected < k < next_expected + window_size`.
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
    /// Flat ring buffer holding out-of-order items indexed relative to `head`.
    /// Allocated lazily on the first out-of-order insertion.
    pub(crate) slots: Vec<Option<T>>,
    /// Ring index corresponding to `next_expected`.
    pub(crate) head: usize,
    /// Number of out-of-order items currently occupied in `slots`.
    pub(crate) buffered: usize,
    /// High-water mark of buffered depth across the buffer's lifetime.
    pub(crate) peak_depth: u64,
    /// Number of distinct stall episodes.
    pub(crate) stall_count: u64,
    /// Cumulative nanoseconds spent in stall episodes.
    pub(crate) total_stall_nanos: u64,
    /// Total items delivered in order.
    pub(crate) items_delivered: u64,
    /// Start instant of the current stall episode, if any.
    pub(crate) stall_start: Option<Instant>,
    /// Clock source for stall timing.
    pub(crate) clock: ClockFn,
    /// When `true`, items pass through without sequence-based reordering.
    ///
    /// In bypass mode, [`insert`](Self::insert) returns items immediately
    /// via [`DrainedItems::Single`] with zero heap allocations.
    pub(crate) bypass: bool,
}

/// Items drained from the buffer in sequence order.
///
/// Returned by [`BoundedReorderBuffer::insert`] when insertion completes
/// a contiguous run starting at `next_expected`.
///
/// Avoids heap allocation on the common cases (`Empty` when an item is
/// buffered out of order, and `Single` when an item arrives in order with
/// no trailing run), only allocating a `Vec<T>` when a gap fill drains
/// multiple contiguous items at once.
#[derive(Debug, Clone)]
pub enum DrainedItems<T> {
    /// No items were ready to drain.
    Empty,
    /// A single item was drained inline on the stack without heap allocation.
    Single(T),
    /// Multiple contiguous items were drained when a head gap closed.
    Batch(Vec<T>),
}

impl<T> DrainedItems<T> {
    /// Returns the drained items as a contiguous slice.
    #[inline]
    #[must_use]
    pub fn as_slice(&self) -> &[T] {
        match self {
            Self::Empty => &[],
            Self::Single(item) => std::slice::from_ref(item),
            Self::Batch(items) => items.as_slice(),
        }
    }

    /// Converts the drained items into a `Vec<T>`.
    #[inline]
    #[must_use]
    pub fn into_vec(self) -> Vec<T> {
        match self {
            Self::Empty => Vec::new(),
            Self::Single(item) => vec![item],
            Self::Batch(items) => items,
        }
    }
}

impl<T> Default for DrainedItems<T> {
    #[inline]
    fn default() -> Self {
        Self::Empty
    }
}

impl<T> Deref for DrainedItems<T> {
    type Target = [T];

    #[inline]
    fn deref(&self) -> &[T] {
        self.as_slice()
    }
}

impl<T> From<DrainedItems<T>> for Vec<T> {
    #[inline]
    fn from(value: DrainedItems<T>) -> Self {
        value.into_vec()
    }
}

impl<T: PartialEq> PartialEq for DrainedItems<T> {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        self.as_slice() == other.as_slice()
    }
}

impl<T: PartialEq> Eq for DrainedItems<T> {}

impl<T: PartialEq> PartialEq<Vec<T>> for DrainedItems<T> {
    #[inline]
    fn eq(&self, other: &Vec<T>) -> bool {
        self.as_slice() == other.as_slice()
    }
}

impl<T: PartialEq> PartialEq<&[T]> for DrainedItems<T> {
    #[inline]
    fn eq(&self, other: &&[T]) -> bool {
        self.as_slice() == *other
    }
}

impl<T: PartialEq, const N: usize> PartialEq<[T; N]> for DrainedItems<T> {
    #[inline]
    fn eq(&self, other: &[T; N]) -> bool {
        self.as_slice() == other.as_slice()
    }
}

/// Owning iterator over [`DrainedItems`].
#[derive(Debug)]
pub enum DrainedItemsIntoIter<T> {
    /// Empty iterator.
    Empty,
    /// Single-element inline iterator.
    Single(Option<T>),
    /// Multi-element vector iterator.
    Batch(std::vec::IntoIter<T>),
}

impl<T> Iterator for DrainedItemsIntoIter<T> {
    type Item = T;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::Empty => None,
            Self::Single(opt) => opt.take(),
            Self::Batch(iter) => iter.next(),
        }
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        let len = match self {
            Self::Empty => 0,
            Self::Single(Some(_)) => 1,
            Self::Single(None) => 0,
            Self::Batch(iter) => iter.len(),
        };
        (len, Some(len))
    }
}

impl<T> ExactSizeIterator for DrainedItemsIntoIter<T> {}

impl<T> IntoIterator for DrainedItems<T> {
    type Item = T;
    type IntoIter = DrainedItemsIntoIter<T>;

    #[inline]
    fn into_iter(self) -> Self::IntoIter {
        match self {
            Self::Empty => DrainedItemsIntoIter::Empty,
            Self::Single(item) => DrainedItemsIntoIter::Single(Some(item)),
            Self::Batch(items) => DrainedItemsIntoIter::Batch(items.into_iter()),
        }
    }
}

impl<'a, T> IntoIterator for &'a DrainedItems<T> {
    type Item = &'a T;
    type IntoIter = std::slice::Iter<'a, T>;

    #[inline]
    fn into_iter(self) -> Self::IntoIter {
        self.as_slice().iter()
    }
}

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
            .field("buffered", &self.buffered)
            .field("peak_depth", &self.peak_depth)
            .field("stall_count", &self.stall_count)
            .field("total_stall_nanos", &self.total_stall_nanos)
            .field("items_delivered", &self.items_delivered)
            .field("stall_active", &self.stall_start.is_some())
            .field("bypass", &self.bypass)
            .finish()
    }
}

#[cfg(test)]
mod zero_copy_stress_tests {
    use super::*;
    use engine::local_copy::buffer_pool::BufferPool;
    use rayon::prelude::*;
    use std::sync::Arc;

    #[test]
    fn skewed_out_of_order_rayon_results_flush_in_sequence_with_zero_payload_reallocation() {
        const TOTAL_ITEMS: usize = 512;
        let pool = Arc::new(BufferPool::with_buffer_size(16, 64 * 1024));

        // Simulate skewed out-of-order Rayon worker completions where each worker:
        // 1. Leases a 64 KB scratch buffer from BufferPool
        // 2. Computes a skewed right-sized payload (64 B .. 16 KB)
        // 3. Immediately drops the 64 KB BufferGuard inside the worker
        // 4. Emits a compact Box<[u8]> handle along with its original heap address
        let mut worker_outputs: Vec<(u64, usize, Box<[u8]>)> = (0..TOTAL_ITEMS)
            .into_par_iter()
            .map(|seq| {
                let mut scratch = BufferPool::acquire_from(Arc::clone(&pool));
                let payload_len = if seq % 37 == 0 {
                    16 * 1024
                } else {
                    64 + (seq * 13) % 512
                };
                for (i, b) in scratch.as_mut_slice()[..payload_len].iter_mut().enumerate() {
                    *b = ((seq + i) & 0xFF) as u8;
                }
                let right_sized: Box<[u8]> = scratch[..payload_len].to_vec().into_boxed_slice();
                let original_ptr = right_sized.as_ptr() as usize;
                // Return 64 KB scratch buffer immediately inside worker before reorder stage
                drop(scratch);
                (seq as u64, original_ptr, right_sized)
            })
            .collect();

        // Scramble into deterministic out-of-order arrival (blocks of 32 reversed)
        for chunk in worker_outputs.chunks_mut(32) {
            chunk.reverse();
        }

        let mut reorder: BoundedReorderBuffer<(usize, Box<[u8]>)> =
            BoundedReorderBuffer::new(TOTAL_ITEMS as u64);
        let mut drained_seq = 0u64;

        for (seq, orig_ptr, payload) in worker_outputs {
            let ready = reorder
                .insert(seq, (orig_ptr, payload))
                .expect("window covers all items");
            for (expected_ptr, drained_payload) in ready {
                // Prove zero payload reallocation/copy inside the reorder buffer:
                // the Box<[u8]> pointer drained from the reorder buffer must be
                // identical to the pointer produced by the Rayon worker.
                assert_eq!(
                    drained_payload.as_ptr() as usize,
                    expected_ptr,
                    "payload for seq {drained_seq} was reallocated or copied in reorder stage"
                );
                let expected_len = if (drained_seq as usize) % 37 == 0 {
                    16 * 1024
                } else {
                    64 + ((drained_seq as usize) * 13) % 512
                };
                assert_eq!(drained_payload.len(), expected_len);
                assert_eq!(drained_payload[0], (drained_seq & 0xFF) as u8);
                drained_seq += 1;
            }
        }

        assert_eq!(drained_seq, TOTAL_ITEMS as u64);
        assert!(reorder.is_empty());
    }
}
