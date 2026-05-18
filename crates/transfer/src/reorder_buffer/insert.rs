//! Insertion path: window admission, bypass shortcut, and stall accounting.

use super::{BackpressureError, BoundedReorderBuffer, DrainedItems};

impl<T> BoundedReorderBuffer<T> {
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
    /// Stall metrics are updated on each call: a stall episode begins when
    /// a non-head item is inserted into an empty buffer, and ends when the
    /// head item arrives and triggers a drain.
    ///
    /// # Errors
    ///
    /// Returns [`BackpressureError`] when `seq >= next_expected + window_size`.
    #[must_use = "drained items must be processed to maintain ordering"]
    pub fn insert(&mut self, seq: u64, item: T) -> Result<DrainedItems<T>, BackpressureError> {
        if self.bypass {
            self.items_delivered += 1;
            return Ok(vec![item]);
        }
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

        // Detect stall start: inserting a non-head item into an empty buffer
        // means the head is missing and subsequent items will block.
        let was_empty = self.pending.is_empty();
        if was_empty && seq != self.next_expected && self.stall_start.is_none() {
            self.stall_start = Some((self.clock)());
            self.stall_count += 1;
        }

        self.pending.insert(seq, item);

        // Update peak depth after insertion.
        let depth = self.pending.len() as u64;
        if depth > self.peak_depth {
            self.peak_depth = depth;
        }

        let prev_expected = self.next_expected;
        let drained = self.drain_consecutive();

        // If the drain advanced next_expected, the stall (if any) has ended.
        if self.next_expected > prev_expected {
            if let Some(start) = self.stall_start.take() {
                let elapsed = (self.clock)().duration_since(start);
                self.total_stall_nanos += elapsed.as_nanos() as u64;
            }
        }

        Ok(drained)
    }
}
