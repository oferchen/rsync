//! Insert paths for [`SpillableReorderBuffer`] and the helpers that keep
//! the spill-state map consistent across insertions.

use super::super::{SpillCodec, SpillError};
use super::SpillableReorderBuffer;

impl<T: SpillCodec> SpillableReorderBuffer<T> {
    /// Inserts an item with the given sequence number.
    ///
    /// The item is first checked against the spill index - if this sequence
    /// was previously spilled (which should not happen with proper usage),
    /// the spilled entry is replaced. The item is inserted into the
    /// in-memory buffer, and if memory usage exceeds the threshold, excess
    /// items are spilled to disk.
    ///
    /// # Errors
    ///
    /// Returns [`SpillError::Capacity`] if the sequence offset from
    /// `next_expected` exceeds the ring buffer capacity. Returns
    /// [`SpillError::Io`] if a spill write fails (ENOSPC, missing temp
    /// directory, partial write, encoder failure). On I/O failure the
    /// affected item is preserved in memory; on capacity failure no
    /// insert occurs.
    pub fn insert(&mut self, sequence: u64, item: T) -> Result<(), SpillError> {
        let item_size = item.estimated_size();
        self.inner.insert(sequence, item)?;
        self.memory_used += item_size;

        // If this sequence was previously spilled, remove the stale entry.
        self.evict_from_spill_state(sequence);

        // RSS-aware trigger runs first so process-wide memory pressure can
        // force a spill before the byte budget is exhausted. The byte budget
        // is consulted independently afterwards.
        if self.should_force_spill_for_rss() || self.memory_used > self.threshold {
            self.spill_excess()?;
        }

        Ok(())
    }

    /// Inserts an item regardless of the capacity bound.
    ///
    /// Mirrors [`ReorderBuffer::force_insert`](super::super::super::reorder::ReorderBuffer::force_insert)
    /// but also tracks memory and triggers spill when needed.
    ///
    /// # Errors
    ///
    /// Returns [`SpillError::Io`] if a spill write fails after the insert.
    /// The newly inserted item is preserved in memory on failure.
    pub fn force_insert(&mut self, sequence: u64, item: T) -> Result<(), SpillError> {
        let item_size = item.estimated_size();
        self.inner.force_insert(sequence, item);
        self.memory_used += item_size;

        self.evict_from_spill_state(sequence);

        if self.should_force_spill_for_rss() || self.memory_used > self.threshold {
            self.spill_excess()?;
        }

        Ok(())
    }

    /// Removes any spill-state entry for `sequence`, including its membership
    /// in a whole-batch record so the disk copy is never resurrected over a
    /// newer in-memory version. The on-disk slot is replaced with `None` so
    /// the encode order survives partial evictions.
    pub(super) fn evict_from_spill_state(&mut self, sequence: u64) {
        if let Some(offset) = self.spill_index.remove(&sequence) {
            if let Some(members) = self.batch_members.get_mut(&offset) {
                for slot in members.iter_mut() {
                    if *slot == Some(sequence) {
                        *slot = None;
                        break;
                    }
                }
                if members.iter().all(Option::is_none) {
                    self.batch_members.remove(&offset);
                }
            }
        }
    }

    /// Restores items taken from the ring buffer back to memory after a
    /// failed batch spill so the caller can retry or shut down cleanly.
    pub(super) fn restore_taken(&mut self, taken: Vec<(u64, T, usize)>) {
        for (seq, item, _) in taken {
            self.inner.force_insert(seq, item);
        }
    }
}
