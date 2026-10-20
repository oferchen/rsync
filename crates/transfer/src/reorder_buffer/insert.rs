//! Insertion path: window admission, bypass shortcut, and stall accounting.

use super::{BackpressureError, BoundedReorderBuffer, DrainedItems, MIN_RING_CAPACITY};

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

        // The window check above bounds the offset by `window_size`, so it fits
        // whenever the ring could ever hold it.
        let offset = usize::try_from(seq - self.next_expected).map_err(|_| BackpressureError {
            sequence: seq,
            window_start: self.next_expected,
            window_end,
        })?;
        self.reserve_offset(offset);
        let cap = self.slots.len();
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

    /// Grows the ring so `offset` slots past `head` are addressable.
    ///
    /// Capacity doubles from [`MIN_RING_CAPACITY`], so memory follows the
    /// deepest reorder gap actually seen rather than the configured window.
    /// Occupied slots are re-laid out in sequence order starting at index 0.
    fn reserve_offset(&mut self, offset: usize) {
        let cap = self.slots.len();
        if offset < cap {
            return;
        }
        let new_cap = offset
            .checked_add(1)
            .and_then(usize::checked_next_power_of_two)
            .unwrap_or(offset + 1)
            .max(MIN_RING_CAPACITY);
        let mut slots = Vec::with_capacity(new_cap);
        slots.extend((0..cap).map(|i| self.slots[(self.head + i) % cap].take()));
        slots.resize_with(new_cap, || None);
        self.slots = slots;
        self.head = 0;
    }
}
