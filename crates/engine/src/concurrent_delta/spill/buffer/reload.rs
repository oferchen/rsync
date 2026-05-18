//! Reload-from-disk path for [`SpillableReorderBuffer`]: in-order delivery,
//! single-item and batch reload, and the on-disk tag dispatcher.

use std::io::{self, SeekFrom};

use super::super::{SpillCodec, SpillError, SpillReclaim};
use super::{SPILL_TAG_RAW, SPILL_TAG_ZSTD, SpillableReorderBuffer};

impl<T: SpillCodec> SpillableReorderBuffer<T> {
    /// Returns the next in-order item if available.
    ///
    /// First checks the in-memory buffer. If the next expected item was
    /// spilled to disk, it is reloaded transparently and the delivery
    /// cursor advances.
    ///
    /// # Errors
    ///
    /// Returns [`SpillError::Io`] if reloading a spilled item from disk
    /// fails (missing spill file, short read, decoder error). Returns
    /// [`SpillError::UnsupportedCompression`] when the on-disk record
    /// advertises a codec this build cannot decode. `Ok(None)` is returned
    /// when no item is ready for delivery.
    pub fn next_in_order(&mut self) -> Result<Option<T>, SpillError> {
        // Try in-memory first.
        if let Some(item) = self.inner.next_in_order() {
            self.memory_used = self.memory_used.saturating_sub(item.estimated_size());
            return Ok(Some(item));
        }

        // Check if the next expected sequence is spilled.
        let next = self.inner.next_expected();
        let Some(&offset) = self.spill_index.get(&next) else {
            return Ok(None);
        };

        // Batched records get reloaded as a unit: decode every item in the
        // record and re-insert the still-needed sequences into the ring so
        // the caller sees the historical in-order delivery semantics. The
        // siblings that come back in this batch live in memory again until
        // a later delivery drops them; account for their bytes so the
        // spill threshold tracker remains accurate. The matching debit
        // happens incrementally as each sibling reaches the in-memory
        // [`inner.next_in_order`](super::super::super::reorder::ReorderBuffer::next_in_order)
        // branch above.
        if let Some(members) = self.batch_members.remove(&offset) {
            let items = self.reload_batch(offset, members.len())?;
            debug_assert_eq!(items.len(), members.len());
            for (slot, item) in members.iter().zip(items) {
                if let Some(seq) = slot {
                    self.spill_index.remove(seq);
                    self.memory_used = self.memory_used.saturating_add(item.estimated_size());
                    self.inner.force_insert(*seq, item);
                }
            }
            self.reload_count += 1;

            // Take the immediately deliverable item out of the ring through
            // the same accounting path used by the in-memory fast path so
            // the credit issued above is matched by an equal debit.
            if let Some(item) = self.inner.next_in_order() {
                self.memory_used = self.memory_used.saturating_sub(item.estimated_size());
                // Honour the post-read reclaim policy: under
                // SpillReclaim::RespillAfterRead, push any in-memory residue
                // back to disk so RSS stays bounded by `threshold` even as
                // large reloaded batches stream through the delivery path.
                if matches!(self.reclaim, SpillReclaim::RespillAfterRead)
                    && self.memory_used > self.threshold
                {
                    self.spill_excess()?;
                }
                return Ok(Some(item));
            }
            debug_assert!(false, "force_insert at next_expected must succeed");
            return Ok(None);
        }

        let item = self.reload_item(offset)?;
        self.spill_index.remove(&next);
        self.reload_count += 1;
        // Re-insert into the inner ring at next_expected (offset 0, always
        // fits) so that next_in_order advances the delivery cursor. The
        // single-item path keeps memory_used unchanged because the item
        // was already debited at spill time and is delivered immediately
        // below without ever being credited back.
        self.inner.force_insert(next, item);
        let result = self.inner.next_in_order();
        debug_assert!(
            result.is_some(),
            "force_insert at next_expected must succeed"
        );

        // Honour the post-read reclaim policy: under
        // SpillReclaim::RespillAfterRead, push any in-memory residue back to
        // disk so RSS stays bounded by `threshold` even as large batches
        // stream through the reload path.
        if matches!(self.reclaim, SpillReclaim::RespillAfterRead)
            && self.memory_used > self.threshold
        {
            self.spill_excess()?;
        }

        Ok(result)
    }

    /// Drains all contiguous in-order items starting from `next_expected`.
    ///
    /// Handles both in-memory and spilled items transparently. Items are
    /// yielded as long as the next expected sequence number is available
    /// either in memory or on disk.
    ///
    /// # Errors
    ///
    /// Returns [`SpillError::Io`] if reloading a spilled item fails. Returns
    /// [`SpillError::UnsupportedCompression`] when the on-disk record
    /// advertises a codec this build cannot decode. Any items already
    /// drained before the failure are discarded along with the error;
    /// callers that need them should drain incrementally via
    /// [`next_in_order`](Self::next_in_order).
    pub fn drain_ready(&mut self) -> Result<Vec<T>, SpillError> {
        let mut items = Vec::new();
        while let Some(item) = self.next_in_order()? {
            items.push(item);
        }
        Ok(items)
    }

    /// Reloads a single item from the spill file at the given offset.
    ///
    /// Reads the leading tag byte and dispatches: [`SPILL_TAG_RAW`] decodes
    /// the payload as-is; [`SPILL_TAG_ZSTD`] decompresses with zstd before
    /// decoding (only when the `spill-compression` feature is enabled).
    /// Any other tag surfaces [`SpillError::UnsupportedCompression`].
    fn reload_item(&mut self, offset: u64) -> Result<T, SpillError> {
        let backend = self
            .spill_file
            .as_mut()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "spill file not initialized"))?;
        let file = backend.file();

        file.seek(SeekFrom::Start(offset))?;

        // Read compression tag.
        let mut tag_buf = [0u8; 1];
        file.read_exact(&mut tag_buf)?;
        let tag = tag_buf[0];

        // Read length prefix.
        let mut len_buf = [0u8; 4];
        file.read_exact(&mut len_buf)?;
        let len = u32::from_le_bytes(len_buf) as usize;

        // Read payload.
        let mut payload = vec![0u8; len];
        file.read_exact(&mut payload)?;

        decode_payload::<T>(tag, payload)
    }

    /// Reloads a whole-batch record holding `count` packed items.
    ///
    /// The record header carries the total payload length; items are
    /// self-delimiting via [`SpillCodec::decode`] and are returned in the
    /// order they were encoded.
    fn reload_batch(&mut self, offset: u64, count: usize) -> io::Result<Vec<T>> {
        let backend = self
            .spill_file
            .as_mut()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "spill file not initialized"))?;
        let file = backend.file();

        file.seek(SeekFrom::Start(offset))?;

        let mut len_buf = [0u8; 4];
        file.read_exact(&mut len_buf)?;
        let total_len = u32::from_le_bytes(len_buf) as usize;

        let mut payload = vec![0u8; total_len];
        file.read_exact(&mut payload)?;

        let mut cursor = payload.as_slice();
        let mut items = Vec::with_capacity(count);
        for _ in 0..count {
            items.push(T::decode(&mut cursor)?);
        }
        Ok(items)
    }
}

/// Decodes a payload according to the leading tag byte.
///
/// `SPILL_TAG_RAW` decodes the bytes as-is. `SPILL_TAG_ZSTD` first decompresses
/// with zstd, but only when the `spill-compression` feature is enabled - a
/// default build hits the catch-all arm and surfaces
/// [`SpillError::UnsupportedCompression`] so the caller fails loudly rather
/// than feeding garbage to the codec.
fn decode_payload<T: SpillCodec>(tag: u8, payload: Vec<u8>) -> Result<T, SpillError> {
    match tag {
        SPILL_TAG_RAW => Ok(T::decode(&mut payload.as_slice())?),
        #[cfg(feature = "spill-compression")]
        SPILL_TAG_ZSTD => {
            let inflated = zstd::stream::decode_all(payload.as_slice())?;
            Ok(T::decode(&mut inflated.as_slice())?)
        }
        #[cfg(not(feature = "spill-compression"))]
        SPILL_TAG_ZSTD => Err(SpillError::UnsupportedCompression(SPILL_TAG_ZSTD)),
        unknown => Err(SpillError::UnsupportedCompression(unknown)),
    }
}
