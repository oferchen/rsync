//! Sequence-based reorder buffer for the concurrent delta pipeline.
//!
//! Workers in the concurrent delta pipeline complete in arbitrary order.
//! [`ReorderBuffer`] collects out-of-order results and yields them strictly
//! in sequence order, enabling the consumer to process results as if they
//! arrived sequentially.
//!
//! # Design
//!
//! Uses a [`BTreeMap`] internally for O(log n) insertion and O(1) extraction
//! of the minimum key. A configurable capacity bound prevents unbounded memory
//! growth when a slow item blocks delivery of many subsequent items.
//!
//! # Upstream Reference
//!
//! Upstream rsync processes files sequentially in `recv_files()`. This buffer
//! restores that sequential ordering after parallel dispatch, preserving the
//! invariant that post-processing (checksum verification, metadata commit)
//! sees files in file-list order.

use std::collections::BTreeMap;

/// Collects out-of-order items and yields them in sequence order.
///
/// Each item must carry a unique sequence number starting from 0. The buffer
/// holds items that arrived ahead of their turn and releases them as soon as
/// a contiguous run from `next_expected` becomes available.
///
/// # Capacity Bound
///
/// When the number of buffered (not-yet-yielded) items reaches `capacity`,
/// [`insert`](ReorderBuffer::insert) returns `Err(CapacityExceeded)`. The
/// caller can then apply backpressure or drain pending items before retrying.
///
/// # Examples
///
/// ```
/// use engine::concurrent_delta::reorder::ReorderBuffer;
///
/// let mut buf: ReorderBuffer<&str> = ReorderBuffer::new(64);
///
/// // Items arrive out of order.
/// assert!(buf.insert(1, "second").is_ok());
/// assert!(buf.next_in_order().is_none()); // waiting for seq 0
///
/// assert!(buf.insert(0, "first").is_ok());
/// assert_eq!(buf.next_in_order(), Some("first"));
/// assert_eq!(buf.next_in_order(), Some("second"));
/// assert!(buf.next_in_order().is_none());
/// ```
#[derive(Debug)]
pub struct ReorderBuffer<T> {
    /// Items waiting to be yielded, keyed by sequence number.
    pending: BTreeMap<u64, T>,
    /// Next sequence number the consumer expects.
    next_expected: u64,
    /// Maximum number of items allowed in `pending` before rejecting inserts.
    capacity: usize,
}

/// Error returned when the reorder buffer is at capacity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapacityExceeded;

impl std::fmt::Display for CapacityExceeded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("reorder buffer capacity exceeded")
    }
}

impl std::error::Error for CapacityExceeded {}

impl<T> ReorderBuffer<T> {
    /// Creates a new reorder buffer with the given capacity bound.
    ///
    /// # Panics
    ///
    /// Panics if `capacity` is zero.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        assert!(capacity > 0, "reorder buffer capacity must be non-zero");
        Self {
            pending: BTreeMap::new(),
            next_expected: 0,
            capacity,
        }
    }

    /// Inserts an item with the given sequence number.
    ///
    /// If the item's sequence equals `next_expected`, it can be retrieved
    /// immediately via [`next_in_order`](Self::next_in_order). Otherwise it
    /// is buffered until all preceding items have been yielded.
    ///
    /// Returns `Err(CapacityExceeded)` if the buffer already holds `capacity`
    /// items. The item is not consumed on error.
    ///
    /// # Errors
    ///
    /// Returns [`CapacityExceeded`] when the buffer is full.
    pub fn insert(&mut self, sequence: u64, item: T) -> Result<(), CapacityExceeded> {
        if self.pending.len() >= self.capacity {
            return Err(CapacityExceeded);
        }
        self.pending.insert(sequence, item);
        Ok(())
    }

    /// Returns the next in-order item if available.
    ///
    /// Yields the item with sequence number equal to `next_expected` and
    /// advances the expected counter. Returns `None` if that item has not
    /// yet been inserted.
    pub fn next_in_order(&mut self) -> Option<T> {
        let item = self.pending.remove(&self.next_expected)?;
        self.next_expected += 1;
        Some(item)
    }

    /// Drains all contiguous in-order items starting from `next_expected`.
    ///
    /// Returns an iterator that yields items as long as the next expected
    /// sequence number is present in the buffer. Useful for batch processing
    /// after inserting multiple items.
    pub fn drain_ready(&mut self) -> DrainReady<'_, T> {
        DrainReady { buffer: self }
    }

    /// Returns the next sequence number the buffer expects.
    #[must_use]
    pub const fn next_expected(&self) -> u64 {
        self.next_expected
    }

    /// Returns the number of items currently buffered (not yet yielded).
    #[must_use]
    pub fn buffered_count(&self) -> usize {
        self.pending.len()
    }

    /// Returns `true` if no items are buffered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    /// Returns the capacity bound.
    #[must_use]
    pub const fn capacity(&self) -> usize {
        self.capacity
    }
}

/// Iterator that drains contiguous in-order items from a [`ReorderBuffer`].
///
/// Created by [`ReorderBuffer::drain_ready`]. Yields items as long as the
/// next expected sequence number is present.
#[derive(Debug)]
pub struct DrainReady<'a, T> {
    buffer: &'a mut ReorderBuffer<T>,
}

impl<T> Iterator for DrainReady<'_, T> {
    type Item = T;

    fn next(&mut self) -> Option<T> {
        self.buffer.next_in_order()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn in_order_delivery() {
        let mut buf = ReorderBuffer::new(8);
        buf.insert(0, "a").unwrap();
        buf.insert(1, "b").unwrap();
        buf.insert(2, "c").unwrap();

        assert_eq!(buf.next_in_order(), Some("a"));
        assert_eq!(buf.next_in_order(), Some("b"));
        assert_eq!(buf.next_in_order(), Some("c"));
        assert_eq!(buf.next_in_order(), None);
    }

    #[test]
    fn out_of_order_reordering() {
        let mut buf = ReorderBuffer::new(8);
        // Arrive: 2, 0, 1
        buf.insert(2, "c").unwrap();
        assert_eq!(buf.next_in_order(), None); // waiting for 0

        buf.insert(0, "a").unwrap();
        assert_eq!(buf.next_in_order(), Some("a"));
        assert_eq!(buf.next_in_order(), None); // waiting for 1

        buf.insert(1, "b").unwrap();
        assert_eq!(buf.next_in_order(), Some("b"));
        assert_eq!(buf.next_in_order(), Some("c"));
        assert_eq!(buf.next_in_order(), None);
    }

    #[test]
    fn gap_handling() {
        let mut buf = ReorderBuffer::new(16);
        // Insert 0, 2, 4 - gaps at 1 and 3
        buf.insert(0, 'a').unwrap();
        buf.insert(2, 'c').unwrap();
        buf.insert(4, 'e').unwrap();

        assert_eq!(buf.next_in_order(), Some('a'));
        // Stuck at 1
        assert_eq!(buf.next_in_order(), None);
        assert_eq!(buf.buffered_count(), 2);

        // Fill gap at 1
        buf.insert(1, 'b').unwrap();
        assert_eq!(buf.next_in_order(), Some('b'));
        assert_eq!(buf.next_in_order(), Some('c'));
        // Stuck at 3
        assert_eq!(buf.next_in_order(), None);

        // Fill gap at 3
        buf.insert(3, 'd').unwrap();
        assert_eq!(buf.next_in_order(), Some('d'));
        assert_eq!(buf.next_in_order(), Some('e'));
        assert_eq!(buf.next_in_order(), None);
        assert!(buf.is_empty());
    }

    #[test]
    fn capacity_bounds_enforcement() {
        let mut buf = ReorderBuffer::new(2);
        buf.insert(5, "x").unwrap();
        buf.insert(3, "y").unwrap();
        // Buffer is full (2 items, capacity 2)
        assert_eq!(buf.insert(7, "z"), Err(CapacityExceeded));
        assert_eq!(buf.buffered_count(), 2);
    }

    #[test]
    fn capacity_frees_after_drain() {
        let mut buf = ReorderBuffer::new(2);
        buf.insert(0, 10).unwrap();
        buf.insert(1, 20).unwrap();
        assert_eq!(buf.insert(2, 30), Err(CapacityExceeded));

        // Drain the ready items
        assert_eq!(buf.next_in_order(), Some(10));
        // Now there is room
        buf.insert(2, 30).unwrap();
        assert_eq!(buf.next_in_order(), Some(20));
        assert_eq!(buf.next_in_order(), Some(30));
    }

    #[test]
    fn empty_buffer_behavior() {
        let buf: ReorderBuffer<i32> = ReorderBuffer::new(4);
        assert!(buf.is_empty());
        assert_eq!(buf.buffered_count(), 0);
        assert_eq!(buf.next_expected(), 0);
        assert_eq!(buf.capacity(), 4);
    }

    #[test]
    fn empty_buffer_next_returns_none() {
        let mut buf: ReorderBuffer<i32> = ReorderBuffer::new(4);
        assert_eq!(buf.next_in_order(), None);
    }

    #[test]
    #[should_panic(expected = "capacity must be non-zero")]
    fn zero_capacity_panics() {
        let _: ReorderBuffer<i32> = ReorderBuffer::new(0);
    }

    #[test]
    fn drain_ready_yields_contiguous_run() {
        let mut buf = ReorderBuffer::new(8);
        buf.insert(0, 'a').unwrap();
        buf.insert(1, 'b').unwrap();
        buf.insert(2, 'c').unwrap();
        buf.insert(4, 'e').unwrap(); // gap at 3

        let drained: Vec<char> = buf.drain_ready().collect();
        assert_eq!(drained, vec!['a', 'b', 'c']);
        assert_eq!(buf.next_expected(), 3);
        assert_eq!(buf.buffered_count(), 1); // 'e' still waiting
    }

    #[test]
    fn drain_ready_empty_buffer() {
        let mut buf: ReorderBuffer<i32> = ReorderBuffer::new(4);
        let drained: Vec<i32> = buf.drain_ready().collect();
        assert!(drained.is_empty());
    }

    #[test]
    fn drain_ready_no_contiguous() {
        let mut buf = ReorderBuffer::new(4);
        buf.insert(3, "far").unwrap();
        let drained: Vec<&str> = buf.drain_ready().collect();
        assert!(drained.is_empty());
        assert_eq!(buf.buffered_count(), 1);
    }

    #[test]
    fn large_sequence_numbers() {
        let mut buf = ReorderBuffer::new(4);
        let base = u64::MAX - 3;
        buf.insert(base, "a").unwrap();
        // This buffer expects sequence 0, so these won't be yielded until
        // next_expected reaches base - but we can verify insertion works.
        assert_eq!(buf.next_in_order(), None);
        assert_eq!(buf.buffered_count(), 1);
    }

    #[test]
    fn next_expected_advances_correctly() {
        let mut buf = ReorderBuffer::new(8);
        assert_eq!(buf.next_expected(), 0);

        buf.insert(0, "x").unwrap();
        buf.next_in_order();
        assert_eq!(buf.next_expected(), 1);

        buf.insert(1, "y").unwrap();
        buf.insert(2, "z").unwrap();
        let _: Vec<_> = buf.drain_ready().collect();
        assert_eq!(buf.next_expected(), 3);
    }

    #[test]
    fn capacity_exceeded_display() {
        assert_eq!(
            CapacityExceeded.to_string(),
            "reorder buffer capacity exceeded"
        );
    }

    #[test]
    fn interleaved_insert_and_drain() {
        let mut buf = ReorderBuffer::new(4);

        // Round 1: insert 0, 2 - drain yields 0
        buf.insert(0, 0).unwrap();
        buf.insert(2, 2).unwrap();
        assert_eq!(buf.next_in_order(), Some(0));
        assert_eq!(buf.next_in_order(), None);

        // Round 2: insert 1 - drain yields 1, 2
        buf.insert(1, 1).unwrap();
        let drained: Vec<i32> = buf.drain_ready().collect();
        assert_eq!(drained, vec![1, 2]);

        // Round 3: insert 3, 4 in order
        buf.insert(3, 3).unwrap();
        buf.insert(4, 4).unwrap();
        let drained: Vec<i32> = buf.drain_ready().collect();
        assert_eq!(drained, vec![3, 4]);
        assert!(buf.is_empty());
    }

    #[test]
    fn duplicate_sequence_overwrites_previous() {
        // BTreeMap::insert replaces the value for an existing key.
        // This is graceful - no panic, no error - but the original item is lost.
        let mut buf = ReorderBuffer::new(4);
        buf.insert(0, "first").unwrap();
        buf.insert(0, "replaced").unwrap();
        assert_eq!(buf.next_in_order(), Some("replaced"));
        assert_eq!(buf.next_in_order(), None);
        // Duplicate consumed one capacity slot (same key), count stays 0 after drain.
        assert!(buf.is_empty());
    }

    #[test]
    fn single_item() {
        let mut buf = ReorderBuffer::new(1);
        buf.insert(0, 42).unwrap();
        assert_eq!(buf.buffered_count(), 1);
        assert!(!buf.is_empty());
        assert_eq!(buf.next_in_order(), Some(42));
        assert!(buf.is_empty());
        assert_eq!(buf.next_expected(), 1);
    }

    #[test]
    fn large_gap_many_buffered() {
        let mut buf = ReorderBuffer::new(128);
        // Insert sequences 1..=100, leaving gap at 0.
        for i in 1..=100 {
            buf.insert(i, i).unwrap();
        }
        assert_eq!(buf.buffered_count(), 100);
        // Nothing drains - all waiting for seq 0.
        assert_eq!(buf.next_in_order(), None);

        // Fill the gap.
        buf.insert(0, 0).unwrap();
        let drained: Vec<u64> = buf.drain_ready().collect();
        assert_eq!(drained.len(), 101);
        assert_eq!(drained[0], 0);
        assert_eq!(drained[100], 100);
        assert!(buf.is_empty());
        assert_eq!(buf.next_expected(), 101);
    }

    #[test]
    fn reverse_order_insertion() {
        let mut buf = ReorderBuffer::new(8);
        // Insert in reverse: 4, 3, 2, 1, 0
        for i in (0..5).rev() {
            buf.insert(i, i).unwrap();
        }
        let drained: Vec<u64> = buf.drain_ready().collect();
        assert_eq!(drained, vec![0, 1, 2, 3, 4]);
    }

    /// Validates `ReorderBuffer` with the actual `DeltaResult` type to ensure
    /// the pipeline integration works end-to-end.
    ///
    /// Mirrors upstream `recv_files()` in `receiver.c` where results must be
    /// processed in file-list order regardless of worker completion order.
    #[test]
    fn delta_result_integration() {
        use crate::concurrent_delta::types::DeltaResult;

        let mut buf: ReorderBuffer<DeltaResult> = ReorderBuffer::new(16);

        // Simulate three workers completing out of order: seq 2, 0, 1
        let r2 = DeltaResult::success(20, 2000, 500, 1500).with_sequence(2);
        let r0 = DeltaResult::success(10, 1000, 300, 700).with_sequence(0);
        let r1 = DeltaResult::needs_redo(15, "checksum mismatch".to_string()).with_sequence(1);

        buf.insert(r2.sequence(), r2).unwrap();
        buf.insert(r0.sequence(), r0).unwrap();
        buf.insert(r1.sequence(), r1).unwrap();

        let drained: Vec<DeltaResult> = buf.drain_ready().collect();
        assert_eq!(drained.len(), 3);

        // Verify ordering by sequence.
        assert_eq!(drained[0].sequence(), 0);
        assert_eq!(drained[0].ndx(), 10);
        assert!(drained[0].is_success());

        assert_eq!(drained[1].sequence(), 1);
        assert_eq!(drained[1].ndx(), 15);
        assert!(drained[1].needs_retry());

        assert_eq!(drained[2].sequence(), 2);
        assert_eq!(drained[2].ndx(), 20);
        assert!(drained[2].is_success());
        assert_eq!(drained[2].bytes_written(), 2000);
    }
}
