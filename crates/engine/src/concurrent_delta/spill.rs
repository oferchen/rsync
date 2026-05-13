//! Bounded-memory spill-to-tempfile layer for the reorder buffer.
//!
//! When the in-memory reorder buffer accumulates more data than a configured
//! threshold (default 64 MB), excess items - those furthest from delivery -
//! are serialized to a temporary file on disk. On delivery the buffer
//! transparently reloads spilled items, maintaining the same in-order
//! guarantee as the underlying [`ReorderBuffer`].
//!
//! # Design
//!
//! Items must implement [`SpillCodec`] so they can be encoded to and decoded
//! from bytes. The codec uses a simple length-prefixed binary format -
//! each record is `[u32 len][payload bytes]` - which is compact, fast to
//! seek through, and platform-independent.
//!
//! Spilled items are indexed by `(sequence_number -> file_offset)` in a
//! `BTreeMap` so reload is O(log S) where S is the number of spilled items.
//! The temporary file is created via the `tempfile` crate and deleted
//! automatically when the buffer is dropped (RAII cleanup).
//!
//! # Spill strategy
//!
//! When `estimated_memory > threshold` after an insert, the buffer spills
//! the *highest-sequence* buffered items first - these are furthest from
//! the delivery cursor (`next_expected`) and thus least likely to be needed
//! soon. Items within a small "hot zone" around `next_expected` are kept
//! in memory to avoid thrashing.
//!
//! # Upstream Reference
//!
//! Upstream rsync processes files sequentially in `recv_files()` and never
//! buffers more than one file's data. This spill mechanism handles the
//! memory pressure that arises from parallel dispatch reordering, which
//! has no upstream equivalent.

use std::collections::BTreeMap;
use std::io::{self, Read, Seek, SeekFrom, Write};

use super::reorder::{CapacityExceeded, ReorderBuffer};

/// Default memory threshold (in bytes) before spilling begins.
///
/// Set to 64 MB, which accommodates roughly 64K items of 1 KB each.
/// Callers can tune this via [`SpillableReorderBuffer::new`].
pub const DEFAULT_SPILL_THRESHOLD: usize = 64 * 1024 * 1024;

/// Minimum number of items to keep in memory around `next_expected`.
///
/// Items within `[next_expected, next_expected + HOT_ZONE)` are never
/// spilled to avoid repeated disk round-trips for items about to be
/// delivered.
const HOT_ZONE: u64 = 16;

/// Codec for serializing and deserializing items to the spill file.
///
/// Implementations must produce a deterministic byte representation and
/// report an accurate `encoded_size` for memory accounting. The encoded
/// format is opaque to the spill layer - only `encode` and `decode` must
/// agree on the wire format.
pub trait SpillCodec: Sized {
    /// Writes the item to `writer` in a format that [`decode`](Self::decode) can read back.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if writing fails.
    fn encode(&self, writer: &mut dyn Write) -> io::Result<()>;

    /// Reads an item from `reader` that was previously written by [`encode`](Self::encode).
    ///
    /// # Errors
    ///
    /// Returns an I/O error if reading fails or the data is corrupt.
    fn decode(reader: &mut dyn Read) -> io::Result<Self>;

    /// Returns the approximate in-memory size of this item in bytes.
    ///
    /// Used for memory accounting to decide when to spill. Does not need
    /// to be exact - a conservative overestimate is fine.
    fn estimated_size(&self) -> usize;
}

/// Reorder buffer with transparent spill-to-tempfile for bounded memory.
///
/// Wraps a [`ReorderBuffer<T>`] and adds disk-backed overflow when the
/// estimated in-memory footprint exceeds a configurable threshold. The
/// public API mirrors `ReorderBuffer` so callers can use this as a
/// drop-in replacement.
///
/// # Type Parameter
///
/// `T` must implement [`SpillCodec`] for serialization. Items that are
/// never spilled (under-threshold operation) pay no serialization cost.
///
/// # Examples
///
/// ```rust,no_run
/// use engine::concurrent_delta::spill::SpillableReorderBuffer;
/// use engine::concurrent_delta::DeltaResult;
///
/// let mut buf: SpillableReorderBuffer<DeltaResult> =
///     SpillableReorderBuffer::new(64, 64 * 1024 * 1024);
///
/// buf.insert(1, DeltaResult::success(1u32, 100, 50, 50).with_sequence(1)).unwrap();
/// buf.insert(0, DeltaResult::success(0u32, 200, 100, 100).with_sequence(0)).unwrap();
/// assert_eq!(buf.next_in_order().unwrap().ndx().get(), 0);
/// assert_eq!(buf.next_in_order().unwrap().ndx().get(), 1);
/// ```
pub struct SpillableReorderBuffer<T: SpillCodec> {
    /// The underlying in-memory reorder buffer.
    inner: ReorderBuffer<T>,
    /// Approximate bytes of in-memory items.
    memory_used: usize,
    /// Maximum in-memory bytes before spilling.
    threshold: usize,
    /// Spilled items: sequence number -> byte offset in the spill file.
    spill_index: BTreeMap<u64, u64>,
    /// Temporary file for spilled items. Created lazily on first spill.
    spill_file: Option<tempfile::SpooledTempFile>,
    /// Current write position in the spill file.
    spill_write_pos: u64,
    /// Running count of spill-to-disk events (for diagnostics).
    spill_count: u64,
    /// Running count of reload-from-disk events (for diagnostics).
    reload_count: u64,
}

impl<T: SpillCodec> std::fmt::Debug for SpillableReorderBuffer<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SpillableReorderBuffer")
            .field("capacity", &self.inner.capacity())
            .field("memory_used", &self.memory_used)
            .field("threshold", &self.threshold)
            .field("buffered_count", &self.inner.buffered_count())
            .field("spilled_count", &self.spill_index.len())
            .field("spill_events", &self.spill_count)
            .field("reload_events", &self.reload_count)
            .finish()
    }
}

/// Diagnostic counters for spill activity.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SpillStats {
    /// Number of items currently spilled to disk.
    pub spilled_items: usize,
    /// Total spill-to-disk events since creation.
    pub spill_events: u64,
    /// Total reload-from-disk events since creation.
    pub reload_events: u64,
    /// Current estimated in-memory bytes.
    pub memory_used: usize,
    /// Configured spill threshold in bytes.
    pub threshold: usize,
}

impl<T: SpillCodec> SpillableReorderBuffer<T> {
    /// Creates a spillable reorder buffer with the given capacity and
    /// memory threshold.
    ///
    /// Items are kept in memory until `estimated_memory > threshold`, at
    /// which point excess items are serialized to a temporary file.
    ///
    /// # Panics
    ///
    /// Panics if `capacity` is zero.
    #[must_use]
    pub fn new(capacity: usize, threshold: usize) -> Self {
        Self {
            inner: ReorderBuffer::new(capacity),
            memory_used: 0,
            threshold,
            spill_index: BTreeMap::new(),
            spill_file: None,
            spill_write_pos: 0,
            spill_count: 0,
            reload_count: 0,
        }
    }

    /// Creates a spillable reorder buffer with the default 64 MB threshold.
    ///
    /// # Panics
    ///
    /// Panics if `capacity` is zero.
    #[must_use]
    pub fn with_default_threshold(capacity: usize) -> Self {
        Self::new(capacity, DEFAULT_SPILL_THRESHOLD)
    }

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
    /// Returns [`CapacityExceeded`] if the sequence offset from
    /// `next_expected` exceeds the ring buffer capacity (same as
    /// [`ReorderBuffer::insert`]).
    ///
    /// # Panics
    ///
    /// Panics if a spill I/O operation fails. In production the temp file
    /// resides on the same filesystem as the transfer destination, so I/O
    /// failures indicate a fatal disk condition.
    pub fn insert(&mut self, sequence: u64, item: T) -> Result<(), CapacityExceeded> {
        let item_size = item.estimated_size();
        self.inner.insert(sequence, item)?;
        self.memory_used += item_size;

        // If this sequence was previously spilled, remove the stale entry.
        self.spill_index.remove(&sequence);

        // Spill excess items if over threshold.
        if self.memory_used > self.threshold {
            self.spill_excess();
        }

        Ok(())
    }

    /// Inserts an item regardless of the capacity bound.
    ///
    /// Mirrors [`ReorderBuffer::force_insert`] but also tracks memory
    /// and triggers spill when needed.
    pub fn force_insert(&mut self, sequence: u64, item: T) {
        let item_size = item.estimated_size();
        self.inner.force_insert(sequence, item);
        self.memory_used += item_size;

        self.spill_index.remove(&sequence);

        if self.memory_used > self.threshold {
            self.spill_excess();
        }
    }

    /// Returns the next in-order item if available.
    ///
    /// First checks the in-memory buffer. If the next expected item was
    /// spilled to disk, it is reloaded transparently and the delivery
    /// cursor advances.
    #[must_use]
    pub fn next_in_order(&mut self) -> Option<T> {
        // Try in-memory first.
        if let Some(item) = self.inner.next_in_order() {
            self.memory_used = self.memory_used.saturating_sub(item.estimated_size());
            return Some(item);
        }

        // Check if the next expected sequence is spilled.
        let next = self.inner.next_expected();
        let &offset = self.spill_index.get(&next)?;

        let item = self
            .reload_item(offset)
            .unwrap_or_else(|e| panic!("failed to reload spilled item at offset {offset}: {e}"));
        self.spill_index.remove(&next);
        self.reload_count += 1;

        // Re-insert into the inner ring at next_expected (offset 0, always
        // fits) so that next_in_order advances the delivery cursor.
        self.inner.force_insert(next, item);
        let result = self.inner.next_in_order();
        debug_assert!(
            result.is_some(),
            "force_insert at next_expected must succeed"
        );
        result
    }

    /// Drains all contiguous in-order items starting from `next_expected`.
    ///
    /// Handles both in-memory and spilled items transparently. Items are
    /// yielded as long as the next expected sequence number is available
    /// either in memory or on disk.
    pub fn drain_ready(&mut self) -> Vec<T> {
        let mut items = Vec::new();
        while let Some(item) = self.next_in_order() {
            items.push(item);
        }
        items
    }

    /// Returns the next sequence number expected for in-order delivery.
    #[must_use]
    pub fn next_expected(&self) -> u64 {
        self.inner.next_expected()
    }

    /// Returns the total number of items buffered (in-memory + spilled).
    #[must_use]
    pub fn buffered_count(&self) -> usize {
        self.inner.buffered_count() + self.spill_index.len()
    }

    /// Returns `true` if no items are buffered anywhere.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty() && self.spill_index.is_empty()
    }

    /// Returns the ring buffer capacity.
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.inner.capacity()
    }

    /// Returns diagnostic counters for spill activity.
    #[must_use]
    pub fn spill_stats(&self) -> SpillStats {
        SpillStats {
            spilled_items: self.spill_index.len(),
            spill_events: self.spill_count,
            reload_events: self.reload_count,
            memory_used: self.memory_used,
            threshold: self.threshold,
        }
    }

    /// Returns the configured memory threshold in bytes.
    #[must_use]
    pub fn threshold(&self) -> usize {
        self.threshold
    }

    /// Spills the highest-sequence items to disk until memory usage drops
    /// below the threshold.
    ///
    /// Items close to `next_expected` are preserved in memory when possible
    /// (the "hot zone"). If the hot zone alone exceeds the threshold, the
    /// hot zone shrinks to ensure at least one item can be spilled.
    fn spill_excess(&mut self) {
        let next = self.inner.next_expected();
        let count = self.inner.buffered_count();
        if count == 0 {
            return;
        }

        // The hot zone protects items near next_expected from thrashing.
        // Scale it down when the threshold is very tight.
        let hot_zone = HOT_ZONE.min(count as u64 / 2).max(1);
        let hot_limit = next.saturating_add(hot_zone);

        // Collect sequences eligible for spilling: those above the hot zone,
        // ordered from highest to lowest so we spill the furthest-from-delivery
        // items first.
        let capacity = self.inner.capacity();
        let mut candidates: Vec<u64> = Vec::new();
        for offset in (0..capacity).rev() {
            let seq = next + offset as u64;
            if seq < hot_limit {
                break;
            }
            candidates.push(seq);
        }

        // Extract and spill candidates until under threshold.
        for seq in candidates {
            if self.memory_used <= self.threshold {
                break;
            }
            if let Some(item) = self.inner.take(seq) {
                let item_size = item.estimated_size();
                match self.spill_item(seq, &item) {
                    Ok(()) => {
                        self.memory_used = self.memory_used.saturating_sub(item_size);
                        self.spill_count += 1;
                    }
                    Err(e) => {
                        // Re-insert the item on spill failure.
                        self.inner.force_insert(seq, item);
                        panic!("spill I/O failed: {e}");
                    }
                }
            }
        }
    }

    /// Serializes a single item to the spill file.
    fn spill_item(&mut self, sequence: u64, item: &T) -> io::Result<()> {
        let file = self.spill_file.get_or_insert_with(|| {
            // Use SpooledTempFile: keeps small spills in memory (up to 1 MB),
            // rolls over to disk for larger volumes. This avoids disk I/O
            // for transient pressure spikes.
            tempfile::SpooledTempFile::new(1024 * 1024)
        });

        file.seek(SeekFrom::Start(self.spill_write_pos))?;

        // Write the payload to a temporary buffer first to get the length.
        let mut payload = Vec::new();
        item.encode(&mut payload)?;
        let len = payload.len() as u32;

        // Write length-prefixed record: [u32 len][payload].
        file.write_all(&len.to_le_bytes())?;
        file.write_all(&payload)?;

        // Record the offset for this sequence.
        self.spill_index.insert(sequence, self.spill_write_pos);
        self.spill_write_pos += 4 + payload.len() as u64;

        Ok(())
    }

    /// Reloads a single item from the spill file at the given offset.
    fn reload_item(&mut self, offset: u64) -> io::Result<T> {
        let file = self
            .spill_file
            .as_mut()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "spill file not initialized"))?;

        file.seek(SeekFrom::Start(offset))?;

        // Read length prefix.
        let mut len_buf = [0u8; 4];
        file.read_exact(&mut len_buf)?;
        let len = u32::from_le_bytes(len_buf) as usize;

        // Read payload.
        let mut payload = vec![0u8; len];
        file.read_exact(&mut payload)?;

        T::decode(&mut payload.as_slice())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Simple SpillCodec for u64 used in tests.
    impl SpillCodec for u64 {
        fn encode(&self, w: &mut dyn Write) -> io::Result<()> {
            w.write_all(&self.to_le_bytes())
        }

        fn decode(r: &mut dyn Read) -> io::Result<Self> {
            let mut buf = [0u8; 8];
            r.read_exact(&mut buf)?;
            Ok(u64::from_le_bytes(buf))
        }

        fn estimated_size(&self) -> usize {
            8
        }
    }

    #[test]
    fn no_spill_under_threshold() {
        let mut buf: SpillableReorderBuffer<u64> = SpillableReorderBuffer::new(64, 1024); // 1 KB threshold

        // Insert a few items - well under threshold.
        for i in 0..10 {
            buf.insert(i, i * 10).unwrap();
        }

        let stats = buf.spill_stats();
        assert_eq!(stats.spilled_items, 0);
        assert_eq!(stats.spill_events, 0);
        assert_eq!(stats.memory_used, 80); // 10 * 8 bytes

        let items = buf.drain_ready();
        assert_eq!(items.len(), 10);
        for (i, &val) in items.iter().enumerate() {
            assert_eq!(val, i as u64 * 10);
        }
    }

    #[test]
    fn spill_triggers_when_threshold_exceeded() {
        // Threshold of 40 bytes = 5 items of 8 bytes each.
        let mut buf: SpillableReorderBuffer<u64> = SpillableReorderBuffer::new(64, 40);

        // Insert items 5..=15 first (gap at 0..5).
        // After 6 items, memory > 40, should trigger spill.
        for i in (0..16).rev() {
            buf.insert(i, i * 100).unwrap();
        }

        let stats = buf.spill_stats();
        assert!(stats.spill_events > 0, "expected spill events, got 0");

        // Despite spilling, items should drain correctly in order.
        let items = buf.drain_ready();
        assert_eq!(items.len(), 16);
        for (i, &val) in items.iter().enumerate() {
            assert_eq!(val, i as u64 * 100, "wrong value at index {i}");
        }
    }

    #[test]
    fn correct_delivery_order_after_spill_and_reload() {
        // Very tight threshold: 16 bytes = 2 items.
        let mut buf: SpillableReorderBuffer<u64> = SpillableReorderBuffer::new(32, 16);

        // Insert out of order.
        buf.insert(5, 50).unwrap();
        buf.insert(3, 30).unwrap();
        buf.insert(7, 70).unwrap();
        buf.insert(1, 10).unwrap();
        buf.insert(6, 60).unwrap();
        buf.insert(4, 40).unwrap();
        buf.insert(2, 20).unwrap();
        buf.insert(0, 0).unwrap();

        let items = buf.drain_ready();
        assert_eq!(items.len(), 8);
        let expected: Vec<u64> = (0..8).map(|i| i * 10).collect();
        assert_eq!(items, expected);
    }

    #[test]
    fn cleanup_on_drop() {
        // The SpooledTempFile is cleaned up when the buffer is dropped.
        // We verify no panic occurs on drop.
        let mut buf: SpillableReorderBuffer<u64> = SpillableReorderBuffer::new(32, 16);

        for i in (0..20).rev() {
            buf.insert(i, i).unwrap();
        }

        let stats = buf.spill_stats();
        assert!(stats.spill_events > 0);

        drop(buf); // Should clean up temp file without panic.
    }

    #[test]
    fn interleaved_spill_and_deliver() {
        // Threshold allows 3 items in memory (24 bytes for u64).
        let mut buf: SpillableReorderBuffer<u64> = SpillableReorderBuffer::new(32, 24);

        // Phase 1: Insert 0..4 in reverse, draining as we go.
        buf.insert(3, 30).unwrap();
        buf.insert(2, 20).unwrap();
        buf.insert(1, 10).unwrap();
        buf.insert(0, 0).unwrap();

        let items = buf.drain_ready();
        assert_eq!(items, vec![0, 10, 20, 30]);

        // Phase 2: Insert 4..8.
        buf.insert(7, 70).unwrap();
        buf.insert(6, 60).unwrap();
        buf.insert(5, 50).unwrap();
        buf.insert(4, 40).unwrap();

        let items = buf.drain_ready();
        assert_eq!(items, vec![40, 50, 60, 70]);

        assert!(buf.is_empty());
    }

    #[test]
    fn exact_threshold_boundary() {
        // Threshold of exactly 40 bytes = 5 items.
        let mut buf: SpillableReorderBuffer<u64> = SpillableReorderBuffer::new(32, 40);

        // Insert exactly 5 items - should NOT spill (40 <= 40 is not > 40).
        for i in 0..5 {
            buf.insert(i, i).unwrap();
        }

        let stats = buf.spill_stats();
        assert_eq!(stats.spill_events, 0, "should not spill at exact threshold");
        assert_eq!(stats.memory_used, 40);

        // 6th item pushes over threshold - should trigger spill.
        buf.insert(5, 5).unwrap();
        let stats = buf.spill_stats();
        assert!(stats.spill_events > 0, "should spill above threshold");

        // All items still deliver correctly.
        let items = buf.drain_ready();
        assert_eq!(items, vec![0, 1, 2, 3, 4, 5]);
    }

    #[test]
    fn empty_buffer_operations() {
        let mut buf: SpillableReorderBuffer<u64> = SpillableReorderBuffer::new(8, 1024);

        assert!(buf.is_empty());
        assert_eq!(buf.buffered_count(), 0);
        assert_eq!(buf.next_expected(), 0);
        assert!(buf.next_in_order().is_none());
        assert!(buf.drain_ready().is_empty());
    }

    #[test]
    fn force_insert_with_spill() {
        let mut buf: SpillableReorderBuffer<u64> = SpillableReorderBuffer::new(4, 24); // 3 items before spill

        buf.force_insert(0, 0);
        buf.force_insert(1, 10);
        buf.force_insert(2, 20);
        buf.force_insert(3, 30);
        buf.force_insert(10, 100); // beyond capacity, triggers grow + possibly spill

        // Drain what's available.
        let items = buf.drain_ready();
        assert_eq!(items, vec![0, 10, 20, 30]);

        // Items 4-9 are missing, so 10 is not yet deliverable.
        assert!(buf.next_in_order().is_none());
    }

    #[test]
    fn spill_stats_tracking() {
        let mut buf: SpillableReorderBuffer<u64> = SpillableReorderBuffer::new(32, 32); // 4 items before spill

        for i in (0..10).rev() {
            buf.insert(i, i).unwrap();
        }

        let stats = buf.spill_stats();
        assert!(stats.spill_events > 0);
        assert_eq!(stats.threshold, 32);

        // Drain all - should trigger reloads.
        let items = buf.drain_ready();
        assert_eq!(items.len(), 10);

        let stats = buf.spill_stats();
        assert!(
            stats.reload_events > 0,
            "expected reload events after drain"
        );
        assert_eq!(stats.spilled_items, 0, "no items should remain spilled");
    }

    #[test]
    fn large_scale_spill_and_drain() {
        // 100 items, threshold allows ~10 in memory.
        let mut buf: SpillableReorderBuffer<u64> = SpillableReorderBuffer::new(128, 80);

        // Insert all 100 items in reverse order.
        for i in (0..100).rev() {
            buf.insert(i, i * 7).unwrap();
        }

        let items = buf.drain_ready();
        assert_eq!(items.len(), 100);
        for (i, &val) in items.iter().enumerate() {
            assert_eq!(val, i as u64 * 7, "wrong value at position {i}");
        }

        let stats = buf.spill_stats();
        assert!(stats.spill_events > 0);
        assert!(stats.reload_events > 0);
        assert!(buf.is_empty());
    }

    #[test]
    fn delta_result_spill_codec_roundtrip() {
        use crate::concurrent_delta::types::DeltaResult;

        let original = DeltaResult::success(42u32, 1000, 300, 700).with_sequence(5);
        let mut encoded = Vec::new();
        original.encode(&mut encoded).unwrap();

        let decoded = DeltaResult::decode(&mut encoded.as_slice()).unwrap();
        assert_eq!(decoded.ndx().get(), 42);
        assert_eq!(decoded.sequence(), 5);
        assert_eq!(decoded.bytes_written(), 1000);
        assert_eq!(decoded.literal_bytes(), 300);
        assert_eq!(decoded.matched_bytes(), 700);
        assert!(decoded.is_success());
    }

    #[test]
    fn delta_result_needs_redo_codec_roundtrip() {
        use crate::concurrent_delta::types::DeltaResult;

        let original =
            DeltaResult::needs_redo(10u32, "checksum mismatch".to_string()).with_sequence(3);
        let mut encoded = Vec::new();
        original.encode(&mut encoded).unwrap();

        let decoded = DeltaResult::decode(&mut encoded.as_slice()).unwrap();
        assert_eq!(decoded.ndx().get(), 10);
        assert_eq!(decoded.sequence(), 3);
        assert!(decoded.needs_retry());
    }

    #[test]
    fn delta_result_failed_codec_roundtrip() {
        use crate::concurrent_delta::types::DeltaResult;

        let original = DeltaResult::failed(99u32, "I/O error on disk".to_string()).with_sequence(7);
        let mut encoded = Vec::new();
        original.encode(&mut encoded).unwrap();

        let decoded = DeltaResult::decode(&mut encoded.as_slice()).unwrap();
        assert_eq!(decoded.ndx().get(), 99);
        assert_eq!(decoded.sequence(), 7);
        assert!(!decoded.is_success());
        assert!(!decoded.needs_retry());
    }

    #[test]
    fn spillable_buffer_with_delta_results() {
        use crate::concurrent_delta::types::DeltaResult;

        let mut buf: SpillableReorderBuffer<DeltaResult> = SpillableReorderBuffer::new(32, 200); // ~2 items before spill

        // Insert several results out of order.
        buf.insert(
            2,
            DeltaResult::success(20u32, 2000, 500, 1500).with_sequence(2),
        )
        .unwrap();
        buf.insert(
            0,
            DeltaResult::success(10u32, 1000, 300, 700).with_sequence(0),
        )
        .unwrap();
        buf.insert(
            1,
            DeltaResult::needs_redo(15u32, "mismatch".to_string()).with_sequence(1),
        )
        .unwrap();

        let items = buf.drain_ready();
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].ndx().get(), 10);
        assert!(items[0].is_success());
        assert_eq!(items[1].ndx().get(), 15);
        assert!(items[1].needs_retry());
        assert_eq!(items[2].ndx().get(), 20);
        assert!(items[2].is_success());
    }
}
