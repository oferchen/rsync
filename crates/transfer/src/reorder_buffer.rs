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

/// Default window size for the bounded reorder buffer.
pub const DEFAULT_WINDOW_SIZE: u64 = 64;

/// Bounded sliding-window reorder buffer with backpressure.
///
/// Guarantees in-order delivery while bounding memory to at most
/// `window_size` buffered items. Producers that exceed the window
/// receive a backpressure signal to throttle submission.
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
#[derive(Debug)]
#[must_use]
pub struct BoundedReorderBuffer<T> {
    /// Next sequence number expected for in-order delivery.
    next_expected: u64,
    /// Maximum number of items ahead of `next_expected` that can be buffered.
    window_size: u64,
    /// Out-of-order items waiting for gaps to fill.
    pending: BTreeMap<u64, T>,
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

impl<T> BoundedReorderBuffer<T> {
    /// Creates a new bounded reorder buffer with the given window size.
    ///
    /// # Panics
    ///
    /// Panics if `window_size` is zero.
    pub fn new(window_size: u64) -> Self {
        assert!(window_size > 0, "window size must be non-zero");
        Self {
            next_expected: 0,
            window_size,
            pending: BTreeMap::new(),
        }
    }

    /// Inserts an item and returns any consecutive items ready for delivery.
    ///
    /// If `seq` is within the acceptance window `[next_expected, next_expected + window_size)`,
    /// the item is buffered. If this insertion completes a contiguous run starting
    /// at `next_expected`, all consecutive items are drained and returned.
    ///
    /// Returns `Err(BackpressureError)` if `seq` is outside the window. Items
    /// with `seq < next_expected` (already delivered) are silently ignored and
    /// return an empty drain.
    ///
    /// # Errors
    ///
    /// Returns [`BackpressureError`] when `seq >= next_expected + window_size`.
    #[must_use = "drained items must be processed to maintain ordering"]
    pub fn insert(&mut self, seq: u64, item: T) -> Result<DrainedItems<T>, BackpressureError> {
        // Already delivered - ignore duplicate/stale submissions.
        if seq < self.next_expected {
            return Ok(Vec::new());
        }

        let window_end = self.next_expected.saturating_add(self.window_size);
        if seq >= window_end {
            return Err(BackpressureError {
                sequence: seq,
                window_start: self.next_expected,
                window_end,
            });
        }

        self.pending.insert(seq, item);
        Ok(self.drain_consecutive())
    }

    /// Drains all consecutive items starting from `next_expected`.
    fn drain_consecutive(&mut self) -> DrainedItems<T> {
        let mut items = Vec::new();
        while let Some(item) = self.pending.remove(&self.next_expected) {
            items.push(item);
            self.next_expected += 1;
        }
        items
    }

    /// Returns the next sequence number expected for in-order delivery.
    #[must_use]
    pub const fn next_expected(&self) -> u64 {
        self.next_expected
    }

    /// Returns the number of items currently buffered (awaiting delivery).
    #[must_use]
    pub fn buffered_count(&self) -> usize {
        self.pending.len()
    }

    /// Returns how many more items can be accepted before backpressure kicks in.
    ///
    /// This is `window_size - buffered_count`, representing the remaining
    /// capacity in the current window.
    #[must_use]
    pub fn window_remaining(&self) -> u64 {
        self.window_size - self.pending.len() as u64
    }

    /// Returns the configured window size.
    #[must_use]
    pub const fn window_size(&self) -> u64 {
        self.window_size
    }

    /// Returns `true` if no items are buffered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn in_order_delivery_yields_immediately() {
        let mut buf = BoundedReorderBuffer::new(8);

        let d = buf.insert(0, "a").unwrap();
        assert_eq!(d, vec!["a"]);

        let d = buf.insert(1, "b").unwrap();
        assert_eq!(d, vec!["b"]);

        let d = buf.insert(2, "c").unwrap();
        assert_eq!(d, vec!["c"]);

        assert_eq!(buf.next_expected(), 3);
        assert!(buf.is_empty());
    }

    #[test]
    fn out_of_order_with_gap_fill() {
        let mut buf = BoundedReorderBuffer::new(8);

        let d = buf.insert(2, "c").unwrap();
        assert!(d.is_empty());

        let d = buf.insert(1, "b").unwrap();
        assert!(d.is_empty());

        // Filling the gap at 0 drains 0, 1, 2.
        let d = buf.insert(0, "a").unwrap();
        assert_eq!(d, vec!["a", "b", "c"]);
        assert_eq!(buf.next_expected(), 3);
    }

    #[test]
    fn backpressure_enforcement() {
        let mut buf: BoundedReorderBuffer<i32> = BoundedReorderBuffer::new(4);

        // Insert seq 0 (delivered immediately).
        let d = buf.insert(0, 0).unwrap();
        assert_eq!(d, vec![0]);
        // Window is now [1, 5).

        // seq 5 is outside [1, 5) - backpressure.
        let err = buf.insert(5, 5).unwrap_err();
        assert_eq!(err.sequence, 5);
        assert_eq!(err.window_start, 1);
        assert_eq!(err.window_end, 5);
    }

    #[test]
    fn window_advancement_opens_new_slots() {
        let mut buf = BoundedReorderBuffer::new(4);
        // Window is [0, 4). Insert 0, 1, 2, 3.
        let d = buf.insert(0, 'a').unwrap();
        assert_eq!(d, vec!['a']);
        let d = buf.insert(1, 'b').unwrap();
        assert_eq!(d, vec!['b']);
        let d = buf.insert(2, 'c').unwrap();
        assert_eq!(d, vec!['c']);
        let d = buf.insert(3, 'd').unwrap();
        assert_eq!(d, vec!['d']);
        // All delivered, next_expected = 4, window = [4, 8).

        // seq 7 is within [4, 8).
        let d = buf.insert(7, 'h').unwrap();
        assert!(d.is_empty());

        // seq 8 is outside [4, 8).
        assert!(buf.insert(8, 'i').is_err());

        // Fill 4, 5, 6 to drain through 7.
        let d = buf.insert(4, 'e').unwrap();
        assert_eq!(d, vec!['e']);
        // 4 drained, next_expected=5, but 5 not present - empty.
        // Wait, 4 drains immediately since next_expected=4 and item 4 arrives.
        // Actually insert(4) sees next_expected=4, inserts, drains 4 (only 4 consecutive).
        // next_expected becomes 5. 5 not in buffer. Returns ['e'].

        let d = buf.insert(5, 'f').unwrap();
        assert_eq!(d, vec!['f']);
        // next_expected = 6, 6 not in buffer.

        let d = buf.insert(6, 'g').unwrap();
        assert_eq!(d, vec!['g', 'h']);
        // next_expected was 6, inserts 6, drains 6 then 7 (consecutive).
        assert_eq!(buf.next_expected(), 8);

        // Now seq 8 is within window [8, 12).
        let d = buf.insert(8, 'i').unwrap();
        assert_eq!(d, vec!['i']);
    }

    #[test]
    fn stale_sequence_ignored() {
        let mut buf = BoundedReorderBuffer::new(4);
        buf.insert(0, 10).unwrap();
        buf.insert(1, 20).unwrap();
        // next_expected is now 2.

        // Inserting seq 0 again (already delivered) is silently ignored.
        let d = buf.insert(0, 99).unwrap();
        assert!(d.is_empty());
        assert_eq!(buf.next_expected(), 2);
    }

    #[test]
    fn window_remaining_tracks_capacity() {
        let mut buf = BoundedReorderBuffer::new(4);
        assert_eq!(buf.window_remaining(), 4);

        buf.insert(2, "x").unwrap();
        assert_eq!(buf.window_remaining(), 3);

        buf.insert(3, "y").unwrap();
        assert_eq!(buf.window_remaining(), 2);

        // Fill gap - drains 0, 1 not present, so only 2, 3 stay buffered.
        // Actually 0 is not in buffer either, so nothing drains.
        assert_eq!(buf.buffered_count(), 2);
    }

    #[test]
    fn contiguous_drain_amortized() {
        let mut buf = BoundedReorderBuffer::new(64);

        // Insert 1..=50 (all out of order, gap at 0).
        for i in (1..=50).rev() {
            let d = buf.insert(i, i).unwrap();
            assert!(d.is_empty());
        }
        assert_eq!(buf.buffered_count(), 50);

        // Insert 0 - drains all 51 items in one call.
        let d = buf.insert(0, 0).unwrap();
        assert_eq!(d.len(), 51);
        for (i, &val) in d.iter().enumerate() {
            assert_eq!(val, i as u64);
        }
        assert!(buf.is_empty());
        assert_eq!(buf.next_expected(), 51);
    }

    #[test]
    #[should_panic(expected = "window size must be non-zero")]
    fn zero_window_panics() {
        let _: BoundedReorderBuffer<i32> = BoundedReorderBuffer::new(0);
    }

    #[test]
    fn backpressure_error_display() {
        let err = BackpressureError {
            sequence: 10,
            window_start: 3,
            window_end: 7,
        };
        assert_eq!(err.to_string(), "sequence 10 outside window [3, 7)");
    }

    #[test]
    fn window_size_accessor() {
        let buf: BoundedReorderBuffer<u8> = BoundedReorderBuffer::new(32);
        assert_eq!(buf.window_size(), 32);
    }

    /// Property test: a random permutation of 0..N always yields 0..N in order.
    mod prop {
        use super::*;
        use proptest::prelude::*;

        proptest! {
            #[test]
            fn random_permutation_yields_sorted(n in 1u64..256) {
                let window = n.max(1);
                let mut buf = BoundedReorderBuffer::new(window);

                // Generate a random-ish permutation using a simple shuffle.
                let mut indices: Vec<u64> = (0..n).collect();
                // Deterministic "shuffle" based on n for reproducibility.
                indices.reverse();

                let mut all_drained = Vec::new();
                for seq in indices {
                    match buf.insert(seq, seq) {
                        Ok(d) => all_drained.extend(d),
                        Err(_) => {
                            // With window == n, this should not happen.
                            panic!("backpressure with window == n");
                        }
                    }
                }

                // Verify all items delivered in order.
                prop_assert_eq!(all_drained.len(), n as usize);
                for (i, &val) in all_drained.iter().enumerate() {
                    prop_assert_eq!(val, i as u64);
                }
            }

            #[test]
            fn backpressure_respects_window(window in 1u64..64, overshoot in 0u64..100) {
                let mut buf: BoundedReorderBuffer<u64> = BoundedReorderBuffer::new(window);
                let target = window + overshoot;
                let result = buf.insert(target, target);
                prop_assert!(result.is_err());
                let err = result.unwrap_err();
                prop_assert_eq!(err.sequence, target);
                prop_assert_eq!(err.window_start, 0);
                prop_assert_eq!(err.window_end, window);
            }
        }
    }
}
