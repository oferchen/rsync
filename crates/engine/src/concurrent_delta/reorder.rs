//! Sequence-based reorder buffer for the concurrent delta pipeline.
//!
//! Workers in the concurrent delta pipeline complete in arbitrary order.
//! [`ReorderBuffer`] collects out-of-order results and yields them strictly
//! in sequence order, enabling the consumer to process results as if they
//! arrived sequentially.
//!
//! # Design
//!
//! Uses a pre-allocated ring buffer internally for O(1) insertion and O(1)
//! extraction of the next expected item. A configurable capacity bound
//! prevents unbounded memory growth when a slow item blocks delivery of
//! many subsequent items.
//!
//! Optionally, a caller can compose an
//! [`AdaptiveCapacityPolicy`](super::adaptive::AdaptiveCapacityPolicy) into
//! the buffer via [`ReorderBuffer::with_adaptive_policy`]. The buffer then
//! grows under sustained pressure and shrinks back toward the policy's
//! minimum once the gap closes, all while preserving the same public API.
//!
//! # Upstream Reference
//!
//! Upstream rsync processes files sequentially in `recv_files()`. This buffer
//! restores that sequential ordering after parallel dispatch, preserving the
//! invariant that post-processing (checksum verification, metadata commit)
//! sees files in file-list order.

use super::adaptive::{AdaptiveCapacityPolicy, AdaptiveState, ReorderStats};

/// Collects out-of-order items and yields them in sequence order.
///
/// Each item must carry a unique sequence number starting from 0. The buffer
/// holds items that arrived ahead of their turn and releases them as soon as
/// a contiguous run from `next_expected` becomes available.
///
/// Internally uses a pre-allocated ring buffer (`Box<[Option<T>]>`) indexed
/// by `(sequence - next_expected) + head`, wrapping at capacity. This gives
/// O(1) insert and O(1) drain - a significant improvement over the previous
/// `BTreeMap`-based O(log n) insert approach.
///
/// # Capacity Bound
///
/// When the distance between a new item's sequence number and `next_expected`
/// exceeds `capacity`, [`insert`](ReorderBuffer::insert) returns
/// `Err(CapacityExceeded)`. The caller can then apply backpressure or drain
/// pending items before retrying.
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
    /// Pre-allocated ring buffer slots.
    slots: Box<[Option<T>]>,
    /// Index into `slots` where `next_expected` sequence maps.
    head: usize,
    /// Next sequence number the consumer expects.
    next_expected: u64,
    /// Number of items currently stored in the ring buffer.
    count: usize,
    /// Maximum number of items allowed before rejecting inserts.
    capacity: usize,
    /// Highest occupied offset from `next_expected` plus one, used by the
    /// adaptive policy to size the live gap window. Reset to 0 whenever the
    /// buffer empties or the head advances past it.
    high_water_offset: usize,
    /// Optional adaptive capacity scaling state. `None` preserves the
    /// historical fixed-capacity behaviour.
    adaptive: Option<AdaptiveState>,
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
    /// Pre-allocates a ring buffer of `capacity` slots, trading memory for
    /// O(1) insert and drain operations.
    ///
    /// # Panics
    ///
    /// Panics if `capacity` is zero.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        assert!(capacity > 0, "reorder buffer capacity must be non-zero");
        let slots: Vec<Option<T>> = (0..capacity).map(|_| None).collect();
        Self {
            slots: slots.into_boxed_slice(),
            head: 0,
            next_expected: 0,
            count: 0,
            capacity,
            high_water_offset: 0,
            adaptive: None,
        }
    }

    /// Creates a reorder buffer governed by an
    /// [`AdaptiveCapacityPolicy`](super::adaptive::AdaptiveCapacityPolicy).
    ///
    /// The buffer starts at `policy.min` slots and resizes between `min` and
    /// `max` based on observed pressure. See [`AdaptiveCapacityPolicy`] for
    /// the grow / shrink rules.
    ///
    /// # Panics
    ///
    /// Panics if `policy.min` is zero (validated by the policy constructor).
    #[must_use]
    pub fn with_adaptive_policy(policy: AdaptiveCapacityPolicy) -> Self {
        let mut buf = Self::new(policy.min);
        buf.adaptive = Some(AdaptiveState::new(policy));
        buf
    }

    /// Computes the ring buffer index for a given sequence number.
    ///
    /// Returns `None` if the sequence is behind `next_expected` or the
    /// offset from `next_expected` exceeds capacity.
    fn slot_index(&self, sequence: u64) -> Option<usize> {
        if sequence < self.next_expected {
            return None;
        }
        let offset = (sequence - self.next_expected) as usize;
        if offset >= self.capacity {
            return None;
        }
        Some((self.head + offset) % self.capacity)
    }

    /// Inserts an item with the given sequence number.
    ///
    /// If the item's sequence equals `next_expected`, it can be retrieved
    /// immediately via [`next_in_order`](Self::next_in_order). Otherwise it
    /// is buffered until all preceding items have been yielded.
    ///
    /// Returns `Err(CapacityExceeded)` if the sequence offset from
    /// `next_expected` exceeds the ring buffer capacity. The item is not
    /// consumed on error.
    ///
    /// # Errors
    ///
    /// Returns [`CapacityExceeded`] when the buffer is full or the sequence
    /// offset exceeds capacity.
    pub fn insert(&mut self, sequence: u64, item: T) -> Result<(), CapacityExceeded> {
        // Adaptive policy may grow the ring before insert to avoid the error.
        if self.adaptive.is_some() && self.slot_index(sequence).is_none() {
            self.try_adaptive_preinsert_grow(sequence);
        }
        let idx = self.slot_index(sequence).ok_or(CapacityExceeded)?;
        if self.slots[idx].is_none() {
            self.count += 1;
        }
        self.slots[idx] = Some(item);
        if sequence >= self.next_expected {
            let offset_plus_one = (sequence - self.next_expected) as usize + 1;
            if offset_plus_one > self.high_water_offset {
                self.high_water_offset = offset_plus_one;
            }
        }
        self.maybe_adapt_capacity();
        Ok(())
    }

    /// Grows the ring (within policy bounds) when an incoming sequence would
    /// otherwise be rejected. Honours the policy's `max` cap; if the sequence
    /// still cannot fit, the caller's `insert` returns `CapacityExceeded`.
    fn try_adaptive_preinsert_grow(&mut self, sequence: u64) {
        let Some(state) = self.adaptive.as_ref() else {
            return;
        };
        if sequence < self.next_expected {
            return;
        }
        let needed = (sequence - self.next_expected) as usize + 1;
        let max = state.policy.max;
        if self.capacity >= max {
            return;
        }
        let target = needed.min(max);
        if target <= self.capacity {
            return;
        }
        self.grow(target);
        if let Some(state) = self.adaptive.as_mut() {
            state.grow_events += 1;
            state.reset_window();
        }
    }

    /// Records a utilization sample and applies grow / shrink decisions when
    /// an adaptive policy is configured. No-op for fixed-capacity buffers.
    fn maybe_adapt_capacity(&mut self) {
        if self.adaptive.is_none() {
            return;
        }
        let utilization = self.count as f32 / self.capacity as f32;
        let gap_window = self.high_water_offset;
        let count = self.count;
        let capacity = self.capacity;

        let (should_grow, should_shrink, target_grow, target_shrink) = {
            let state = self.adaptive.as_mut().expect("adaptive state present");
            state.record_sample(utilization);
            let grow = state.should_grow(count, capacity, gap_window);
            let shrink = !grow && state.should_shrink();
            let tg = if grow {
                state.policy.next_grow(capacity)
            } else {
                capacity
            };
            // Floor for shrink keeps every buffered item addressable.
            let floor = gap_window.max(state.policy.min);
            let ts = if shrink {
                state.policy.next_shrink(capacity, floor)
            } else {
                capacity
            };
            (grow, shrink, tg, ts)
        };

        if should_grow && target_grow > self.capacity {
            self.grow(target_grow);
            let state = self.adaptive.as_mut().expect("adaptive state present");
            state.grow_events += 1;
            state.reset_window();
        } else if should_shrink && target_shrink < self.capacity {
            self.resize_to(target_shrink);
            let state = self.adaptive.as_mut().expect("adaptive state present");
            state.shrink_events += 1;
            state.reset_window();
        }
    }

    /// Returns the next in-order item if available.
    ///
    /// Yields the item with sequence number equal to `next_expected` and
    /// advances the expected counter and head pointer. Returns `None` if
    /// that item has not yet been inserted.
    pub fn next_in_order(&mut self) -> Option<T> {
        let item = self.slots[self.head].take()?;
        self.head = (self.head + 1) % self.capacity;
        self.next_expected += 1;
        self.count -= 1;
        // The high-water mark is tracked relative to next_expected, so it
        // shifts down as we deliver. Saturate at zero when the buffer empties.
        self.high_water_offset = self.high_water_offset.saturating_sub(1);
        if self.count == 0 {
            self.high_water_offset = 0;
        }
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
    pub const fn buffered_count(&self) -> usize {
        self.count
    }

    /// Returns `true` if no items are buffered.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Returns the capacity bound.
    #[must_use]
    pub const fn capacity(&self) -> usize {
        self.capacity
    }

    /// Returns adaptive capacity counters (grow/shrink event totals plus the
    /// current capacity). Counters are zero for fixed-capacity buffers.
    #[must_use]
    pub fn stats(&self) -> ReorderStats {
        match self.adaptive.as_ref() {
            Some(s) => ReorderStats {
                grow_events: s.grow_events,
                shrink_events: s.shrink_events,
                capacity: self.capacity,
            },
            None => ReorderStats {
                grow_events: 0,
                shrink_events: 0,
                capacity: self.capacity,
            },
        }
    }

    /// Inserts an item regardless of the capacity bound.
    ///
    /// Used by the consumer loop to break deadlocks when the buffer is full
    /// and [`drain_ready`](Self::drain_ready) cannot make progress because
    /// `next_expected` is not yet buffered. For sequences within the existing
    /// ring capacity, this inserts directly. For sequences beyond capacity,
    /// the ring is grown to accommodate the item.
    pub fn force_insert(&mut self, sequence: u64, item: T) {
        if let Some(idx) = self.slot_index(sequence) {
            if self.slots[idx].is_none() {
                self.count += 1;
            }
            self.slots[idx] = Some(item);
        } else if sequence >= self.next_expected {
            // Sequence exceeds current capacity - grow the ring buffer.
            let offset = (sequence - self.next_expected) as usize;
            let new_capacity = offset + 1;
            self.grow(new_capacity);
            let idx = (self.head + offset) % self.capacity;
            if self.slots[idx].is_none() {
                self.count += 1;
            }
            self.slots[idx] = Some(item);
        } else {
            // Silently ignore sequences behind next_expected (already delivered).
            return;
        }
        if sequence >= self.next_expected {
            let offset_plus_one = (sequence - self.next_expected) as usize + 1;
            if offset_plus_one > self.high_water_offset {
                self.high_water_offset = offset_plus_one;
            }
        }
    }

    /// Grows the ring buffer to at least `min_capacity` slots.
    ///
    /// Linearizes the ring (head moves to index 0) during the resize. When
    /// invoked from the adaptive policy `min_capacity` already encodes the
    /// desired target; for legacy `force_insert` callers the doubling fallback
    /// preserves the original amortized growth contract.
    fn grow(&mut self, min_capacity: usize) {
        let new_cap = match self.adaptive.as_ref() {
            Some(_) => min_capacity.max(self.capacity + 1),
            None => min_capacity.max(self.capacity * 2),
        };
        self.resize_to(new_cap);
    }

    /// Reallocates the ring to exactly `new_capacity` slots, linearizing
    /// occupied entries to indices `0..count`. Caller must guarantee
    /// `new_capacity >= high_water_offset` so no buffered item is evicted.
    fn resize_to(&mut self, new_capacity: usize) {
        debug_assert!(
            new_capacity >= self.high_water_offset,
            "resize would evict buffered items: new_capacity={}, high_water={}",
            new_capacity,
            self.high_water_offset
        );
        let mut new_slots: Vec<Option<T>> = (0..new_capacity).map(|_| None).collect();
        let copy_len = self.capacity.min(new_capacity);
        for (i, slot) in new_slots.iter_mut().enumerate().take(copy_len) {
            let src = (self.head + i) % self.capacity;
            *slot = self.slots[src].take();
        }
        self.slots = new_slots.into_boxed_slice();
        self.head = 0;
        self.capacity = new_capacity;
    }

    /// Validates that all items have been delivered with no gaps in the sequence.
    ///
    /// Call this after all producers have finished and all results have been
    /// drained. Panics if any items remain in the buffer - their presence
    /// indicates a gap in the sequence (a work item was lost upstream).
    ///
    /// # Panics
    ///
    /// Panics if the buffer still contains pending items, meaning one or more
    /// sequence numbers between `next_expected` and the lowest buffered sequence
    /// were never inserted - a fatal correctness violation.
    pub fn finish(self) {
        if self.count > 0 {
            // Find the first occupied slot to report the gap.
            let mut first_seq = self.next_expected;
            for i in 0..self.capacity {
                let idx = (self.head + i) % self.capacity;
                if self.slots[idx].is_some() {
                    first_seq = self.next_expected + i as u64;
                    break;
                }
            }
            panic!(
                "ReorderBuffer: sequence gap detected - expected seq {} but next buffered is seq {} \
                 ({} items stranded)",
                self.next_expected, first_seq, self.count,
            );
        }
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
        // With capacity 2, valid offsets from next_expected (0) are 0 and 1
        buf.insert(0, "x").unwrap();
        buf.insert(1, "y").unwrap();
        // Seq 2 has offset 2 from next_expected=0, which equals capacity
        assert_eq!(buf.insert(2, "z"), Err(CapacityExceeded));
        assert_eq!(buf.buffered_count(), 2);
    }

    #[test]
    fn capacity_frees_after_drain() {
        let mut buf = ReorderBuffer::new(2);
        buf.insert(0, 10).unwrap();
        buf.insert(1, 20).unwrap();
        // Seq 2 has offset 2 from next_expected=0, which equals capacity
        assert_eq!(buf.insert(2, 30), Err(CapacityExceeded));

        assert_eq!(buf.next_in_order(), Some(10));
        // Now there is room (next_expected=1, seq 2 has offset 1)
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
        // Offset from next_expected (0) is enormous - must be rejected
        assert_eq!(buf.insert(base, "a"), Err(CapacityExceeded));
        assert_eq!(buf.buffered_count(), 0);
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
        // Ring buffer replaces the value for an existing slot.
        // This is graceful - no panic, no error - but the original item is lost.
        let mut buf = ReorderBuffer::new(4);
        buf.insert(0, "first").unwrap();
        buf.insert(0, "replaced").unwrap();
        assert_eq!(buf.next_in_order(), Some("replaced"));
        assert_eq!(buf.next_in_order(), None);
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
        for i in (0..5).rev() {
            buf.insert(i, i).unwrap();
        }
        let drained: Vec<u64> = buf.drain_ready().collect();
        assert_eq!(drained, vec![0, 1, 2, 3, 4]);
    }

    #[test]
    fn force_insert_bypasses_capacity() {
        let mut buf = ReorderBuffer::new(2);
        buf.insert(1, "a").unwrap();
        buf.insert(0, "b").unwrap();
        // Normal insert for seq 2 fails (offset 2 >= capacity 2).
        assert_eq!(buf.insert(2, "c"), Err(CapacityExceeded));
        // force_insert grows the ring to accommodate.
        buf.force_insert(2, "c");
        assert_eq!(buf.buffered_count(), 3);
        // drain_ready yields all three in order.
        let drained: Vec<&str> = buf.drain_ready().collect();
        assert_eq!(drained, vec!["b", "a", "c"]);
    }

    /// Verifies that concurrent producers submitting results out of order
    /// still yield strictly ascending sequence delivery to the consumer.
    ///
    /// Simulates the concurrent delta pipeline scenario: multiple worker
    /// threads produce results with known sequence numbers at variable rates.
    /// A single consumer thread owns the `ReorderBuffer` and receives items
    /// via a channel, inserting them and draining in-order results.
    ///
    /// The bounded capacity is exercised: when the buffer is full, the
    /// consumer must drain before accepting more items.
    #[test]
    fn concurrent_producers_in_order_delivery() {
        use std::sync::mpsc;
        use std::thread;

        const TOTAL_ITEMS: u64 = 200;
        const NUM_PRODUCERS: u64 = 4;
        const BUFFER_CAPACITY: usize = 32;

        let (tx, rx) = mpsc::channel::<(u64, u64)>();

        // Spawn producer threads - each owns a disjoint set of sequence numbers.
        let producers: Vec<_> = (0..NUM_PRODUCERS)
            .map(|producer_id| {
                let tx = tx.clone();
                thread::spawn(move || {
                    let mut seq = producer_id;
                    while seq < TOTAL_ITEMS {
                        // Simulate variable work duration via lightweight spin.
                        // Deterministic delay based on sequence to create reordering.
                        let spins = ((seq * 7 + producer_id * 13) % 100) as u32;
                        for _ in 0..spins {
                            std::hint::spin_loop();
                        }

                        tx.send((seq, seq)).unwrap();
                        seq += NUM_PRODUCERS;
                    }
                })
            })
            .collect();

        // Drop the original sender so rx terminates when producers finish.
        drop(tx);

        // Consumer owns the buffer - no shared-mutable-state deadlock.
        let mut buf: ReorderBuffer<u64> = ReorderBuffer::new(BUFFER_CAPACITY);
        let mut collected: Vec<u64> = Vec::with_capacity(TOTAL_ITEMS as usize);
        let mut capacity_pressure_observed = false;

        for (seq, val) in rx {
            // Try normal insert; on capacity exceeded, drain first then force.
            match buf.insert(seq, val) {
                Ok(()) => {}
                Err(CapacityExceeded) => {
                    capacity_pressure_observed = true;
                    // Drain what we can, then force-insert the item.
                    collected.extend(buf.drain_ready());
                    buf.force_insert(seq, val);
                }
            }
            // Opportunistically drain ready items.
            collected.extend(buf.drain_ready());
        }

        for p in producers {
            p.join().expect("producer panicked");
        }

        collected.extend(buf.drain_ready());

        assert_eq!(
            collected.len(),
            TOTAL_ITEMS as usize,
            "expected {TOTAL_ITEMS} items but got {}",
            collected.len()
        );

        // Verify strictly ascending sequence order.
        for (i, &val) in collected.iter().enumerate() {
            assert_eq!(
                val, i as u64,
                "expected sequence {i} but got {val} - ordering violated"
            );
        }

        // With 4 producers racing and capacity 32, we expect the buffer to
        // have been pressured at least once during the run.
        assert!(
            capacity_pressure_observed,
            "capacity backpressure was never triggered - increase TOTAL_ITEMS or decrease BUFFER_CAPACITY"
        );
    }

    #[test]
    fn finish_succeeds_when_fully_drained() {
        let mut buf = ReorderBuffer::new(8);
        buf.insert(0, "a").unwrap();
        buf.insert(1, "b").unwrap();
        buf.insert(2, "c").unwrap();
        let _: Vec<_> = buf.drain_ready().collect();
        buf.finish(); // no panic - all items delivered
    }

    #[test]
    fn finish_succeeds_on_empty_buffer() {
        let buf: ReorderBuffer<i32> = ReorderBuffer::new(4);
        buf.finish(); // no items were ever inserted, no gap
    }

    #[test]
    #[should_panic(expected = "sequence gap detected")]
    fn finish_panics_on_gap() {
        let mut buf = ReorderBuffer::new(8);
        // Insert seq 0 and seq 2, skip seq 1 entirely.
        buf.insert(0, "a").unwrap();
        buf.insert(2, "c").unwrap();
        let _: Vec<_> = buf.drain_ready().collect(); // delivers only seq 0
        // Finishing with seq 1 missing triggers the panic.
        buf.finish();
    }

    #[test]
    #[should_panic(expected = "sequence gap detected")]
    fn finish_panics_when_first_item_missing() {
        let mut buf = ReorderBuffer::new(8);
        // Only insert seq 1 and 2, never seq 0.
        buf.insert(1, "b").unwrap();
        buf.insert(2, "c").unwrap();
        let _: Vec<_> = buf.drain_ready().collect(); // delivers nothing
        buf.finish();
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
        assert_eq!(drained[0].ndx().get(), 10);
        assert!(drained[0].is_success());

        assert_eq!(drained[1].sequence(), 1);
        assert_eq!(drained[1].ndx().get(), 15);
        assert!(drained[1].needs_retry());

        assert_eq!(drained[2].sequence(), 2);
        assert_eq!(drained[2].ndx().get(), 20);
        assert!(drained[2].is_success());
        assert_eq!(drained[2].bytes_written(), 2000);
    }

    #[test]
    fn ring_buffer_wraps_correctly() {
        // Verify head pointer wraps around the ring buffer.
        let mut buf = ReorderBuffer::new(4);

        // Fill and drain twice to force head wrapping.
        for batch in 0..3u64 {
            let base = batch * 4;
            for i in 0..4 {
                buf.insert(base + i, base + i).unwrap();
            }
            let drained: Vec<u64> = buf.drain_ready().collect();
            assert_eq!(drained.len(), 4);
            for (j, &val) in drained.iter().enumerate() {
                assert_eq!(val, base + j as u64);
            }
            assert!(buf.is_empty());
        }
        assert_eq!(buf.next_expected(), 12);
    }

    #[test]
    fn ring_buffer_stress_sequential() {
        // Stress test: many sequential insert-drain cycles.
        let mut buf = ReorderBuffer::new(8);
        for i in 0..1000u64 {
            buf.insert(i, i).unwrap();
            assert_eq!(buf.next_in_order(), Some(i));
        }
        assert!(buf.is_empty());
        assert_eq!(buf.next_expected(), 1000);
    }

    #[test]
    fn interleaved_gaps_progressive_fill() {
        // Insert even-numbered items, then progressively fill odd gaps.
        // Each odd fill should cascade delivery through the next even.
        let mut buf = ReorderBuffer::new(16);

        // Insert 0, 2, 4, 6, 8 (gaps at 1, 3, 5, 7).
        for i in (0..10).step_by(2) {
            buf.insert(i, i).unwrap();
        }

        // Only seq 0 is deliverable.
        assert_eq!(buf.next_in_order(), Some(0));
        assert_eq!(buf.next_in_order(), None);
        assert_eq!(buf.buffered_count(), 4); // 2, 4, 6, 8

        // Fill gap at 1 - should cascade to deliver 1, 2.
        buf.insert(1, 1).unwrap();
        let drained: Vec<u64> = buf.drain_ready().collect();
        assert_eq!(drained, vec![1, 2]);
        assert_eq!(buf.next_expected(), 3);

        // Fill gap at 3 - cascades 3, 4.
        buf.insert(3, 3).unwrap();
        let drained: Vec<u64> = buf.drain_ready().collect();
        assert_eq!(drained, vec![3, 4]);

        // Fill gap at 5 - cascades 5, 6.
        buf.insert(5, 5).unwrap();
        let drained: Vec<u64> = buf.drain_ready().collect();
        assert_eq!(drained, vec![5, 6]);

        // Fill gap at 7 - cascades 7, 8.
        buf.insert(7, 7).unwrap();
        let drained: Vec<u64> = buf.drain_ready().collect();
        assert_eq!(drained, vec![7, 8]);

        assert!(buf.is_empty());
        assert_eq!(buf.next_expected(), 9);
    }

    #[test]
    fn burst_after_gap() {
        // Insert a contiguous burst 0-4, then a second burst 10-14 with a gap
        // at 5-9. Verify first burst drains, second is stuck until gap fills.
        let mut buf = ReorderBuffer::new(32);

        for i in 0..5 {
            buf.insert(i, i).unwrap();
        }
        for i in 10..15 {
            buf.insert(i, i).unwrap();
        }

        // Drain the first burst.
        let drained: Vec<u64> = buf.drain_ready().collect();
        assert_eq!(drained, vec![0, 1, 2, 3, 4]);
        assert_eq!(buf.next_expected(), 5);
        assert_eq!(buf.buffered_count(), 5); // 10-14 waiting

        // Nothing drains while 5-9 are missing.
        assert_eq!(buf.next_in_order(), None);

        // Fill the gap 5-9.
        for i in 5..10 {
            buf.insert(i, i).unwrap();
        }

        // Now 5-14 should all drain in order.
        let drained: Vec<u64> = buf.drain_ready().collect();
        assert_eq!(drained, vec![5, 6, 7, 8, 9, 10, 11, 12, 13, 14]);
        assert!(buf.is_empty());
        assert_eq!(buf.next_expected(), 15);
    }

    /// Deterministic pseudo-random permutation stress test.
    ///
    /// Inserts 1000 items in a deterministic shuffled order using a simple
    /// linear congruential generator. Verifies the output is perfectly
    /// ordered 0-999 regardless of insertion order.
    #[test]
    fn stress_deterministic_random_order() {
        const N: usize = 1000;
        let capacity = N;
        let mut buf = ReorderBuffer::new(capacity);

        // Generate a deterministic permutation of 0..N using Fisher-Yates
        // with a simple LCG for reproducibility.
        let mut perm: Vec<u64> = (0..N as u64).collect();
        let mut rng_state: u64 = 0xDEAD_BEEF_CAFE_1234; // fixed seed
        for i in (1..N).rev() {
            // LCG: state = state * 6364136223846793005 + 1442695040888963407
            rng_state = rng_state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            let j = (rng_state >> 33) as usize % (i + 1);
            perm.swap(i, j);
        }

        let mut collected: Vec<u64> = Vec::with_capacity(N);
        for &seq in &perm {
            buf.insert(seq, seq).unwrap();
            collected.extend(buf.drain_ready());
        }
        collected.extend(buf.drain_ready());

        assert_eq!(collected.len(), N);
        for (i, &val) in collected.iter().enumerate() {
            assert_eq!(
                val, i as u64,
                "expected sequence {i} but got {val} at output position {i}"
            );
        }
        assert!(buf.is_empty());
        assert_eq!(buf.next_expected(), N as u64);
    }

    #[test]
    fn large_gap_fill_one_at_a_time() {
        // Insert item 100 first, then fill 0-99 one at a time in forward order.
        // Nothing should be delivered until seq 0 arrives, then cascading delivery.
        let mut buf = ReorderBuffer::new(128);

        buf.insert(100, 100u64).unwrap();
        assert_eq!(buf.next_in_order(), None);
        assert_eq!(buf.buffered_count(), 1);

        // Fill 1-99 (still missing seq 0).
        for i in 1..100 {
            buf.insert(i, i).unwrap();
            assert_eq!(
                buf.next_in_order(),
                None,
                "should not deliver before seq 0 arrives (inserting {i})"
            );
        }
        assert_eq!(buf.buffered_count(), 100);

        // Insert seq 0 - triggers cascade of all 101 items.
        buf.insert(0, 0).unwrap();
        let drained: Vec<u64> = buf.drain_ready().collect();
        assert_eq!(drained.len(), 101);
        for (i, &val) in drained.iter().enumerate() {
            assert_eq!(val, i as u64);
        }
        assert!(buf.is_empty());
        assert_eq!(buf.next_expected(), 101);
    }

    #[test]
    fn force_insert_beyond_capacity_then_drain() {
        // Verify force_insert with a large gap grows the buffer and maintains
        // ordering after the gap is filled.
        let mut buf = ReorderBuffer::new(4);

        buf.insert(0, 0u64).unwrap();
        buf.insert(1, 1).unwrap();

        // Force-insert far beyond capacity.
        buf.force_insert(20, 20);
        assert!(buf.capacity() > 4); // ring was grown

        // Fill the gap 2-19.
        for i in 2..20 {
            buf.insert(i, i).unwrap();
        }

        let drained: Vec<u64> = buf.drain_ready().collect();
        assert_eq!(drained.len(), 21);
        for (i, &val) in drained.iter().enumerate() {
            assert_eq!(val, i as u64);
        }
        assert!(buf.is_empty());
    }

    #[test]
    fn multiple_drain_ready_calls_are_idempotent() {
        // After drain_ready exhausts contiguous items, subsequent calls
        // yield nothing until a new contiguous item is inserted.
        let mut buf = ReorderBuffer::new(8);
        buf.insert(0, 'a').unwrap();
        buf.insert(1, 'b').unwrap();
        buf.insert(3, 'd').unwrap(); // gap at 2

        let first: Vec<char> = buf.drain_ready().collect();
        assert_eq!(first, vec!['a', 'b']);

        let second: Vec<char> = buf.drain_ready().collect();
        assert!(second.is_empty());

        // Fill the gap.
        buf.insert(2, 'c').unwrap();
        let third: Vec<char> = buf.drain_ready().collect();
        assert_eq!(third, vec!['c', 'd']);
    }

    /// Verifies that `ReorderBuffer` produces strictly sequential output
    /// regardless of insertion order.
    ///
    /// This is the channel-agnostic ordering invariant: the reorder buffer
    /// restores submission order using sequence numbers, independent of the
    /// underlying channel implementation (std mpsc, crossbeam, etc.).
    ///
    /// Tests three scenarios:
    /// 1. A small batch inserted in a specific scrambled order.
    /// 2. A large batch (200 items) inserted in a deterministic pseudo-random
    ///    order derived from a fixed seed.
    /// 3. Burst insertion (groups of items arrive together) with interleaved drains.
    #[test]
    fn reorder_ordering_invariant() {
        // Scenario 1: Small batch, specific scrambled order.
        {
            let mut buf = ReorderBuffer::new(16);
            let insertion_order: Vec<u64> = vec![5, 2, 0, 3, 1, 4];
            for seq in &insertion_order {
                buf.insert(*seq, *seq).unwrap();
            }
            let drained: Vec<u64> = buf.drain_ready().collect();
            let expected: Vec<u64> = (0..6).collect();
            assert_eq!(
                drained, expected,
                "small batch: output must be sequential 0..6"
            );
            assert!(buf.is_empty());
        }

        // Scenario 2: Large batch with deterministic pseudo-random insertion order.
        // Uses a simple LCG (linear congruential generator) seeded at 42 to
        // produce a fixed permutation of 0..200, ensuring determinism across runs.
        {
            let n: u64 = 200;
            let mut indices: Vec<u64> = (0..n).collect();

            // Fisher-Yates shuffle with deterministic LCG.
            let mut rng_state: u64 = 42;
            for i in (1..indices.len()).rev() {
                // LCG: state = state * 6364136223846793005 + 1 (mod 2^64)
                rng_state = rng_state
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1);
                let j = (rng_state >> 33) as usize % (i + 1);
                indices.swap(i, j);
            }

            let mut buf = ReorderBuffer::new(n as usize);
            for &seq in &indices {
                buf.insert(seq, seq * 10).unwrap();
            }
            let drained: Vec<u64> = buf.drain_ready().collect();
            let expected: Vec<u64> = (0..n).map(|i| i * 10).collect();
            assert_eq!(
                drained, expected,
                "large batch: output must be sequential with correct values"
            );
            assert!(buf.is_empty());
        }

        // Scenario 3: Burst insertion with interleaved drains.
        // Items arrive in bursts (groups of 5), each burst scrambled,
        // with drains after each burst.
        {
            let total: u64 = 30;
            let burst_size: u64 = 5;
            let mut buf = ReorderBuffer::new(total as usize);
            let mut collected = Vec::new();

            for burst_start in (0..total).step_by(burst_size as usize) {
                // Each burst arrives in reverse order within the group.
                let burst_end = (burst_start + burst_size).min(total);
                for seq in (burst_start..burst_end).rev() {
                    buf.insert(seq, seq).unwrap();
                }
                // Drain whatever is ready after this burst.
                collected.extend(buf.drain_ready());
            }

            let expected: Vec<u64> = (0..total).collect();
            assert_eq!(
                collected, expected,
                "burst insertion: output must be sequential 0..{total}"
            );
            assert!(buf.is_empty());
        }
    }
}

#[cfg(test)]
mod adaptive_tests {
    use super::*;

    /// Default-constructed buffers must remain unaffected by adaptive logic.
    #[test]
    fn fixed_capacity_default_unchanged() {
        let mut buf: ReorderBuffer<u64> = ReorderBuffer::new(4);
        for i in 0..4 {
            buf.insert(i, i).unwrap();
        }
        assert_eq!(buf.capacity(), 4);
        let stats = buf.stats();
        assert_eq!(stats.grow_events, 0);
        assert_eq!(stats.shrink_events, 0);
        assert_eq!(stats.capacity, 4);
        // Capacity-exceeded behaviour preserved.
        assert_eq!(buf.insert(4, 4), Err(CapacityExceeded));
    }

    #[test]
    fn adaptive_buffer_starts_at_min_capacity() {
        let policy = AdaptiveCapacityPolicy::new(4, 32, 2.0);
        let buf: ReorderBuffer<u64> = ReorderBuffer::with_adaptive_policy(policy);
        assert_eq!(buf.capacity(), 4);
        assert_eq!(buf.stats().grow_events, 0);
    }

    /// Inserting beyond `min` capacity grows the ring (without losing items)
    /// up to but never beyond `max`.
    #[test]
    fn grows_under_load() {
        let policy = AdaptiveCapacityPolicy::with_window(2, 16, 2.0, 64);
        let mut buf: ReorderBuffer<u64> = ReorderBuffer::with_adaptive_policy(policy);

        // Build a wide gap (insert seq 0..7 with a hole at 0) to force growth.
        for seq in 1..8 {
            // seq 1, 2 fit (capacity 2 -> grows). Subsequent inserts continue
            // to grow up to max as the gap window widens.
            buf.insert(seq, seq).unwrap();
        }
        let stats = buf.stats();
        assert!(stats.grow_events > 0, "buffer never grew under load");
        assert!(
            buf.capacity() >= 8,
            "capacity {} < required gap window 8",
            buf.capacity()
        );
        assert!(
            buf.capacity() <= 16,
            "capacity {} exceeded max",
            buf.capacity()
        );

        // Fill the head and confirm ordered drain still works after growth.
        buf.insert(0, 0).unwrap();
        let drained: Vec<u64> = buf.drain_ready().collect();
        assert_eq!(drained, (0..8).collect::<Vec<_>>());
    }

    /// The grow path must never breach `policy.max`.
    #[test]
    fn never_exceeds_max() {
        let policy = AdaptiveCapacityPolicy::with_window(2, 8, 2.0, 64);
        let mut buf: ReorderBuffer<u64> = ReorderBuffer::with_adaptive_policy(policy);

        // Try to drive a gap of 12 - capacity should cap at 8.
        for seq in 1..8 {
            buf.insert(seq, seq).unwrap();
        }
        // Once at max, inserts beyond capacity must error.
        let err = buf.insert(8, 8);
        assert_eq!(err, Err(CapacityExceeded));
        assert!(buf.capacity() <= 8);
    }

    /// After a sustained idle window, capacity should shrink back toward `min`.
    #[test]
    fn shrinks_when_idle() {
        let policy = AdaptiveCapacityPolicy::with_window(2, 32, 2.0, 4);
        let mut buf: ReorderBuffer<u64> = ReorderBuffer::with_adaptive_policy(policy);

        // Force a grow: build a gap so capacity expands.
        for seq in 1..6 {
            buf.insert(seq, seq).unwrap();
        }
        let grown = buf.capacity();
        assert!(grown >= 6, "expected grown capacity, got {grown}");
        let grow_events = buf.stats().grow_events;
        assert!(grow_events >= 1);

        // Drain everything to clear the gap.
        buf.insert(0, 0).unwrap();
        let _: Vec<_> = buf.drain_ready().collect();
        assert!(buf.is_empty());

        // Submit single-item inserts that immediately drain - low utilization.
        for seq in 6..30 {
            buf.insert(seq, seq).unwrap();
            let _ = buf.next_in_order();
        }
        let stats = buf.stats();
        assert!(stats.shrink_events >= 1, "buffer never shrank when idle");
        assert!(buf.capacity() < grown, "capacity did not decrease");
    }

    /// Shrinks must clamp at `policy.min` no matter how long the idle window.
    #[test]
    fn never_drops_below_min() {
        let min = 4usize;
        let policy = AdaptiveCapacityPolicy::with_window(min, 32, 2.0, 4);
        let mut buf: ReorderBuffer<u64> = ReorderBuffer::with_adaptive_policy(policy);

        // Drive growth then quiesce.
        for seq in 1..10 {
            buf.insert(seq, seq).unwrap();
        }
        buf.insert(0, 0).unwrap();
        let _: Vec<_> = buf.drain_ready().collect();

        for seq in 10..200 {
            buf.insert(seq, seq).unwrap();
            let _ = buf.next_in_order();
        }
        assert!(
            buf.capacity() >= min,
            "capacity {} dropped below min {min}",
            buf.capacity()
        );
        // Min is the hard floor.
        assert_eq!(buf.capacity(), min);
    }

    /// Stats reflect both grow and shrink events accurately.
    #[test]
    fn stats_track_both_events() {
        let policy = AdaptiveCapacityPolicy::with_window(2, 16, 2.0, 4);
        let mut buf: ReorderBuffer<u64> = ReorderBuffer::with_adaptive_policy(policy);

        // Trigger at least one grow.
        for seq in 1..6 {
            buf.insert(seq, seq).unwrap();
        }
        assert!(buf.stats().grow_events >= 1);

        // Drain and idle to trigger at least one shrink.
        buf.insert(0, 0).unwrap();
        let _: Vec<_> = buf.drain_ready().collect();
        for seq in 6..40 {
            buf.insert(seq, seq).unwrap();
            let _ = buf.next_in_order();
        }
        let stats = buf.stats();
        assert!(stats.grow_events >= 1);
        assert!(stats.shrink_events >= 1);
        assert_eq!(stats.capacity, buf.capacity());
    }

    /// Ordering is preserved across grow / shrink transitions.
    #[test]
    fn ordering_preserved_through_resize() {
        let policy = AdaptiveCapacityPolicy::with_window(2, 64, 2.0, 8);
        let mut buf: ReorderBuffer<u64> = ReorderBuffer::with_adaptive_policy(policy);

        let n: u64 = 200;
        // Insert in reversed bursts of 8 to force out-of-order delivery and
        // exercise both grow and shrink paths.
        let mut collected: Vec<u64> = Vec::with_capacity(n as usize);
        for burst_start in (0..n).step_by(8) {
            let end = (burst_start + 8).min(n);
            for seq in (burst_start..end).rev() {
                buf.insert(seq, seq).unwrap();
            }
            collected.extend(buf.drain_ready());
        }
        let expected: Vec<u64> = (0..n).collect();
        assert_eq!(collected, expected);
    }
}
