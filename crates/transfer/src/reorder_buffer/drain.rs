//! Consecutive-drain helper that advances the sliding window.

use super::{BoundedReorderBuffer, DrainedItems};

impl<T> BoundedReorderBuffer<T> {
    /// Drains `head_item` (which arrived at `next_expected`) followed by all
    /// consecutive occupied slots in the ring buffer.
    #[inline]
    pub(super) fn drain_from_head(&mut self, head_item: T) -> DrainedItems<T> {
        let cap = self.slots.len();
        debug_assert!(cap > 0, "buffered > 0 implies ring is allocated");
        debug_assert!(
            self.slots[self.head].is_none(),
            "head slot must be vacant before head_item arrives"
        );
        self.head = (self.head + 1) % cap;
        self.next_expected += 1;
        self.items_delivered += 1;

        // If the very next slot is still empty, only `head_item` is ready;
        // return it on the stack without allocating a Vec.
        let Some(second_item) = self.slots[self.head].take() else {
            return DrainedItems::Single(head_item);
        };
        self.buffered -= 1;
        self.head = (self.head + 1) % cap;
        self.next_expected += 1;
        self.items_delivered += 1;

        let mut items = Vec::with_capacity(self.buffered + 2);
        items.push(head_item);
        items.push(second_item);

        while let Some(next_item) = self.slots[self.head].take() {
            self.buffered -= 1;
            self.head = (self.head + 1) % cap;
            self.next_expected += 1;
            self.items_delivered += 1;
            items.push(next_item);
        }
        if self.buffered == 0 {
            self.head = 0;
        }
        DrainedItems::Batch(items)
    }
}
