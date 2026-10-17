//! Insertion path: window admission, bypass shortcut, and stall accounting.

use super::{BackpressureError, BoundedReorderBuffer, DrainedItems, MAX_RING_CAPACITY};

impl<T> BoundedReorderBuffer<T> {
    /// Inserts an item and returns any consecutive items ready for delivery.
    ///
    /// If `seq` is within the acceptance window `[next_expected, next_expected + window_size)`,
    /// the item is buffered. If this insertion completes a contiguous run starting
    /// at `next_expected`, all consecutive items are drained and returned.
    ///
    /// In-order arrivals (`seq == next_expected` with no buffered trailing items)
    /// and passthrough insertions return [`DrainedItems::Single`] directly on the
    /// stack without touching the ring buffer or allocating on the heap.
    ///
    /// Returns `Err(BackpressureError)` if `seq` is outside the window. Items
    /// with `seq < next_expected` (already delivered) are silently ignored and
    /// return an empty drain.
    ///
    /// Stall metrics are updated on each call: a stall episode begins when
    /// a non-head item is inserted into an empty buffer, and ends when the
    /// head item arrives and triggers a drain.
    ///
    /// # Errors
    ///
    /// Returns [`BackpressureError`] when `seq >= next_expected + window_size`.
    #[inline]
    #[must_use = "drained items must be processed to maintain ordering"]
    pub fn insert(&mut self, seq: u64, item: T) -> Result<DrainedItems<T>, BackpressureError> {
        if self.bypass {
            self.items_delivered += 1;
            return Ok(DrainedItems::Single(item));
        }
        // Already delivered - ignore duplicate/stale submissions.
        if seq < self.next_expected {
            return Ok(DrainedItems::Empty);
        }

        let window_end = self.next_expected.saturating_add(self.window_size);
        if seq >= window_end {
            return Err(BackpressureError {
                sequence: seq,
                window_start: self.next_expected,
                window_end,
            });
        }

        // Fast path: in-order arrival.
        if seq == self.next_expected {
            let depth = (self.buffered + 1) as u64;
            if depth > self.peak_depth {
                self.peak_depth = depth;
            }
            if self.buffered == 0 {
                self.next_expected += 1;
                self.items_delivered += 1;
                return Ok(DrainedItems::Single(item));
            }
            let drained = self.drain_from_head(item);
            if let Some(start) = self.stall_start.take() {
                let elapsed = (self.clock)().duration_since(start);
                self.total_stall_nanos += elapsed.as_nanos() as u64;
            }
            return Ok(drained);
        }

        // Out-of-order arrival (`seq > self.next_expected`).
        // Detect stall start: inserting a non-head item into an empty buffer
        // means the head is missing and subsequent items will block.
        if self.buffered == 0 && self.stall_start.is_none() {
            self.stall_start = Some((self.clock)());
            self.stall_count += 1;
        }

        if self.slots.is_empty() {
            let cap = (self.window_size as usize).clamp(1, MAX_RING_CAPACITY);
            self.slots.resize_with(cap, || None);
        }

        let cap = self.slots.len();
        let offset = (seq - self.next_expected) as usize;
        debug_assert!(
            offset < cap,
            "offset {offset} exceeds ring capacity {cap} (window_size={})",
            self.window_size
        );
        let idx = (self.head + offset) % cap;
        if self.slots[idx].is_none() {
            self.buffered += 1;
        }
        self.slots[idx] = Some(item);

        let depth = self.buffered as u64;
        if depth > self.peak_depth {
            self.peak_depth = depth;
        }

        Ok(DrainedItems::Empty)
    }
}
