//! Consecutive-drain helper that advances the sliding window.

use super::{BoundedReorderBuffer, DrainedItems};

impl<T> BoundedReorderBuffer<T> {
    /// Drains all consecutive items starting from `next_expected`.
    pub(super) fn drain_consecutive(&mut self) -> DrainedItems<T> {
        let mut items = Vec::new();
        while let Some(item) = self.pending.remove(&self.next_expected) {
            items.push(item);
            self.next_expected += 1;
            self.items_delivered += 1;
        }
        items
    }
}
