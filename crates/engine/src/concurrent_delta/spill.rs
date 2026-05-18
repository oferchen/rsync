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
//! from bytes. The on-disk format is `[u32 len][payload bytes]` per record;
//! payload contents and per-record fan-out are controlled by
//! [`SpillGranularity`]. The default ([`SpillGranularity::WholeBatch`])
//! packs every candidate selected by a single spill event into one record
//! so the 4-byte length header is amortised across many items.
//! [`SpillGranularity::PerItem`] writes one record per item, matching the
//! historical layout and keeping a single reload's decode cost bounded to
//! a single payload. Both formats are compact, fast to seek through, and
//! platform-independent.
//!
//! Spilled items are indexed by `(sequence_number -> file_offset)` in a
//! `BTreeMap` so reload is O(log S) where S is the number of spilled items.
//! By default the temporary file is created via the `tempfile` crate
//! (`SpooledTempFile`) and deleted automatically when the buffer is dropped
//! (RAII cleanup). Callers may supply an explicit spill directory via
//! [`SpillableReorderBuffer::with_spill_dir`], which is more resilient when
//! the directory is shared across long-running transfers.
//!
//! # Spill strategy
//!
//! When `estimated_memory > threshold` after an insert, the buffer spills
//! the *highest-sequence* buffered items first - these are furthest from
//! the delivery cursor (`next_expected`) and thus least likely to be needed
//! soon. Items within a small "hot zone" around `next_expected` are kept
//! in memory to avoid thrashing. Under
//! [`SpillGranularity::WholeBatch`] every non-hot-zone candidate is
//! evicted in one batched write; under [`SpillGranularity::PerItem`] the
//! eviction stops as soon as the in-memory budget drops back below the
//! threshold.
//!
//! # Error handling
//!
//! Every disk operation surfaces its error to the caller via [`SpillError`].
//! Earlier revisions panicked on I/O failure, which translated heavy-transfer
//! ENOSPC and temp-directory-vanish events into process crashes. The current
//! API returns errors so the receiver can map them to rsync exit code 11
//! ([`FileIo`](https://github.com/RsyncProject/rsync/blob/master/errcode.h))
//! and abort cleanly. When an explicit spill directory disappears mid-transfer
//! the buffer attempts a single `create_dir_all` recovery before propagating
//! the failure.
//!
//! # Upstream Reference
//!
//! Upstream rsync processes files sequentially in `recv_files()` and never
//! buffers more than one file's data. This spill mechanism handles the
//! memory pressure that arises from parallel dispatch reordering, which
//! has no upstream equivalent.

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use super::reorder::{CapacityExceeded, ReorderBuffer};

pub mod policy;
pub mod stats;
pub use policy::{ReclaimMode, SpillCompression, SpillGranularity, SpillPolicy};
pub use stats::SpillStats;

mod tempfile;
use tempfile::{SpillBackend, open_backend};

#[cfg(test)]
mod tests_per_knob;

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

/// Errors surfaced by the spill layer.
///
/// Producers should treat any [`SpillError::Io`] as fatal for the affected
/// transfer: ENOSPC, missing spill directories, and partial writes all
/// indicate that the disk backing the reorder buffer can no longer be
/// trusted. The receiver maps these to exit code 11 ([`FileIo`]) so the
/// transfer aborts with the same semantics as upstream rsync's I/O failures.
///
/// [`FileIo`]: https://github.com/RsyncProject/rsync/blob/master/errcode.h
#[derive(Debug)]
pub enum SpillError {
    /// Capacity bound from the underlying ring buffer was exceeded.
    Capacity(CapacityExceeded),
    /// Disk I/O failed while reading or writing spilled items.
    Io(io::Error),
}

impl SpillError {
    /// Returns the underlying I/O error if this is an I/O failure.
    #[must_use]
    pub fn io_error(&self) -> Option<&io::Error> {
        match self {
            SpillError::Io(e) => Some(e),
            SpillError::Capacity(_) => None,
        }
    }

    /// Returns `true` if this error indicates the disk is out of space.
    #[must_use]
    pub fn is_out_of_space(&self) -> bool {
        self.io_error()
            .is_some_and(|e| e.kind() == io::ErrorKind::StorageFull)
    }
}

impl std::fmt::Display for SpillError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SpillError::Capacity(_) => write!(f, "reorder buffer capacity exceeded"),
            SpillError::Io(e) => write!(f, "reorder spill I/O failed: {e}"),
        }
    }
}

impl std::error::Error for SpillError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            SpillError::Capacity(_) => None,
            SpillError::Io(e) => Some(e),
        }
    }
}

impl From<CapacityExceeded> for SpillError {
    fn from(e: CapacityExceeded) -> Self {
        SpillError::Capacity(e)
    }
}

impl From<io::Error> for SpillError {
    fn from(e: io::Error) -> Self {
        SpillError::Io(e)
    }
}

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
/// assert_eq!(buf.next_in_order().unwrap().unwrap().ndx().get(), 0);
/// assert_eq!(buf.next_in_order().unwrap().unwrap().ndx().get(), 1);
/// ```
pub struct SpillableReorderBuffer<T: SpillCodec> {
    /// The underlying in-memory reorder buffer.
    inner: ReorderBuffer<T>,
    /// Approximate bytes of in-memory items.
    memory_used: usize,
    /// Maximum in-memory bytes before spilling.
    threshold: usize,
    /// Spilled items: sequence number -> byte offset of the owning record.
    ///
    /// For [`SpillGranularity::PerItem`] every sequence has its own record
    /// and offset. For [`SpillGranularity::WholeBatch`] multiple sequences
    /// can map to the same record offset; the sibling list lives in
    /// [`batch_members`](Self::batch_members).
    spill_index: BTreeMap<u64, u64>,
    /// Reverse lookup for whole-batch records: record offset -> the
    /// sequences originally packed into that record, in encode order.
    ///
    /// Only populated when the spill is written under
    /// [`SpillGranularity::WholeBatch`]. Per-item records skip this map.
    /// When an entry is removed from [`spill_index`](Self::spill_index) the
    /// matching slot here is replaced with `None` so the on-disk decode
    /// order survives partial evictions.
    batch_members: BTreeMap<u64, Vec<Option<u64>>>,
    /// Temporary storage for spilled items. Created lazily on first spill.
    spill_file: Option<SpillBackend>,
    /// Caller-supplied spill directory for the directory-backed flavour.
    /// `None` means use a spooled tempfile.
    spill_dir: Option<PathBuf>,
    /// Current write position in the spill file.
    spill_write_pos: u64,
    /// Per-spill-event record format.
    ///
    /// [`SpillGranularity::WholeBatch`] (default) packs every candidate
    /// chosen by a single [`spill_excess`](Self::spill_excess) call into one
    /// length-prefixed record. [`SpillGranularity::PerItem`] keeps the
    /// historical one-record-per-item layout.
    granularity: SpillGranularity,
    /// Running count of spill-to-disk events (for diagnostics).
    spill_count: u64,
    /// Running count of reload-from-disk events (for diagnostics).
    reload_count: u64,
    /// Running count of `create_dir_all` retries after the spill directory
    /// disappeared mid-transfer.
    dir_recreate_count: u64,
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
            .field("dir_recreate_count", &self.dir_recreate_count)
            .field("granularity", &self.granularity)
            .finish()
    }
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
            batch_members: BTreeMap::new(),
            spill_file: None,
            spill_dir: None,
            spill_write_pos: 0,
            granularity: SpillGranularity::default(),
            spill_count: 0,
            reload_count: 0,
            dir_recreate_count: 0,
        }
    }

    /// Creates a spillable reorder buffer that backs its spill file with an
    /// explicit on-disk directory.
    ///
    /// The directory is created if it does not exist. If it later disappears
    /// during a transfer (operator cleanup, tmpfs eviction, container restart)
    /// the buffer recreates it once before propagating the underlying error.
    ///
    /// # Errors
    ///
    /// Returns an [`io::Error`] if the directory cannot be created.
    ///
    /// # Panics
    ///
    /// Panics if `capacity` is zero.
    pub fn with_spill_dir(
        capacity: usize,
        threshold: usize,
        dir: impl Into<PathBuf>,
    ) -> io::Result<Self> {
        let dir = dir.into();
        fs::create_dir_all(&dir)?;
        Ok(Self {
            inner: ReorderBuffer::new(capacity),
            memory_used: 0,
            threshold,
            spill_index: BTreeMap::new(),
            batch_members: BTreeMap::new(),
            spill_file: None,
            spill_dir: Some(dir),
            spill_write_pos: 0,
            granularity: SpillGranularity::default(),
            spill_count: 0,
            reload_count: 0,
            dir_recreate_count: 0,
        })
    }

    /// Overrides the per-spill-event record granularity.
    ///
    /// [`SpillGranularity::WholeBatch`] (the default) packs every candidate
    /// chosen by a single spill event into one length-prefixed record, which
    /// amortises the 4-byte header across many items.
    /// [`SpillGranularity::PerItem`] writes one record per item so a single
    /// reload only has to decode one item's payload.
    #[must_use]
    pub fn with_granularity(mut self, granularity: SpillGranularity) -> Self {
        self.granularity = granularity;
        self
    }

    /// Returns the configured per-spill-event record granularity.
    #[must_use]
    pub fn granularity(&self) -> SpillGranularity {
        self.granularity
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

        // Spill excess items if over threshold.
        if self.memory_used > self.threshold {
            self.spill_excess()?;
        }

        Ok(())
    }

    /// Inserts an item regardless of the capacity bound.
    ///
    /// Mirrors [`ReorderBuffer::force_insert`] but also tracks memory
    /// and triggers spill when needed.
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

        if self.memory_used > self.threshold {
            self.spill_excess()?;
        }

        Ok(())
    }

    /// Removes any spill-state entry for `sequence`, including its membership
    /// in a whole-batch record so the disk copy is never resurrected over a
    /// newer in-memory version. The on-disk slot is replaced with `None` so
    /// the encode order survives partial evictions.
    fn evict_from_spill_state(&mut self, sequence: u64) {
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

    /// Returns the next in-order item if available.
    ///
    /// First checks the in-memory buffer. If the next expected item was
    /// spilled to disk, it is reloaded transparently and the delivery
    /// cursor advances.
    ///
    /// # Errors
    ///
    /// Returns [`SpillError::Io`] if reloading a spilled item from disk
    /// fails (missing spill file, short read, decoder error). `Ok(None)`
    /// is returned when no item is ready for delivery.
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
        // [`inner.next_in_order`](ReorderBuffer::next_in_order) branch above.
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
    /// Returns [`SpillError::Io`] if reloading a spilled item fails. Any
    /// items already drained before the failure are discarded along with
    /// the error; callers that need them should drain incrementally via
    /// [`next_in_order`](Self::next_in_order).
    pub fn drain_ready(&mut self) -> Result<Vec<T>, SpillError> {
        let mut items = Vec::new();
        while let Some(item) = self.next_in_order()? {
            items.push(item);
        }
        Ok(items)
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
            dir_recreate_events: self.dir_recreate_count,
        }
    }

    /// Returns the configured memory threshold in bytes.
    #[must_use]
    pub fn threshold(&self) -> usize {
        self.threshold
    }

    /// Returns the configured spill directory, if any.
    #[must_use]
    pub fn spill_dir(&self) -> Option<&Path> {
        self.spill_dir.as_deref()
    }

    /// Spills the highest-sequence items to disk until memory usage drops
    /// below the threshold.
    ///
    /// Items close to `next_expected` are preserved in memory when possible
    /// (the "hot zone"). If the hot zone alone exceeds the threshold, the
    /// hot zone shrinks to ensure at least one item can be spilled.
    fn spill_excess(&mut self) -> Result<(), SpillError> {
        let next = self.inner.next_expected();
        let count = self.inner.buffered_count();
        if count == 0 {
            return Ok(());
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

        match self.granularity {
            SpillGranularity::PerItem => self.spill_candidates_per_item(&candidates),
            SpillGranularity::WholeBatch => self.spill_candidates_whole_batch(&candidates),
        }
    }

    /// Per-item spill: each candidate becomes its own length-prefixed record.
    ///
    /// Matches the historical on-disk layout: `[u32 len][payload]` per item.
    fn spill_candidates_per_item(&mut self, candidates: &[u64]) -> Result<(), SpillError> {
        for &seq in candidates {
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
                        // Re-insert the item on spill failure so the caller
                        // can retry or shut down without losing the result.
                        self.inner.force_insert(seq, item);
                        return Err(SpillError::Io(e));
                    }
                }
            }
        }
        Ok(())
    }

    /// Whole-batch spill: combine every candidate selected for this spill
    /// event into a single length-prefixed record so the per-item header
    /// overhead is paid once.
    ///
    /// The disk layout is `[u32 total_payload_len][payload1][payload2]...`.
    /// Every non-hot-zone candidate is evicted in one event so the next
    /// write amortises the 4-byte header across many items - the spill
    /// event leaves the hot zone in memory and nothing else, instead of
    /// repeatedly re-entering [`spill_excess`](Self::spill_excess) one
    /// item at a time.
    fn spill_candidates_whole_batch(&mut self, candidates: &[u64]) -> Result<(), SpillError> {
        // Collect every candidate eligible for eviction. Walk the selection
        // in the same highest-first order as the per-item path so the
        // closest-to-delivery items stay in memory.
        let mut taken: Vec<(u64, T, usize)> = Vec::new();
        for &seq in candidates {
            if let Some(item) = self.inner.take(seq) {
                let item_size = item.estimated_size();
                taken.push((seq, item, item_size));
            }
        }

        if taken.is_empty() {
            return Ok(());
        }

        // Encode all payloads up front. A codec failure must not leave a
        // partial record on disk, and re-insertion is straightforward while
        // the items are still owned here.
        let mut payload = Vec::new();
        for (_, item, _) in &taken {
            if let Err(e) = item.encode(&mut payload) {
                self.restore_taken(taken);
                return Err(SpillError::Io(e));
            }
        }
        if payload.len() > u32::MAX as usize {
            self.restore_taken(taken);
            return Err(SpillError::Io(io::Error::new(
                io::ErrorKind::InvalidData,
                "spill record exceeds u32::MAX bytes",
            )));
        }
        let len = payload.len() as u32;
        let mut record_offset = self.spill_write_pos;

        let written = match self.write_record(&len.to_le_bytes(), &payload) {
            Ok(w) => w,
            Err(e) if e.kind() == io::ErrorKind::NotFound && self.spill_dir.is_some() => {
                if !self.spill_index.is_empty() {
                    self.restore_taken(taken);
                    return Err(SpillError::Io(e));
                }
                if let Err(retry_err) = self.recreate_spill_dir() {
                    self.restore_taken(taken);
                    return Err(SpillError::Io(retry_err));
                }
                // recreate_spill_dir resets write_pos and clears the index,
                // so re-anchor the record offset before the retry write.
                record_offset = self.spill_write_pos;
                match self.write_record(&len.to_le_bytes(), &payload) {
                    Ok(w) => w,
                    Err(retry_err) => {
                        self.restore_taken(taken);
                        return Err(SpillError::Io(retry_err));
                    }
                }
            }
            Err(e) => {
                self.restore_taken(taken);
                return Err(SpillError::Io(e));
            }
        };

        // Record the placement of every item now that the write committed.
        let slots: Vec<Option<u64>> = taken.iter().map(|(seq, _, _)| Some(*seq)).collect();
        for (seq, _, item_size) in &taken {
            self.spill_index.insert(*seq, record_offset);
            self.memory_used = self.memory_used.saturating_sub(*item_size);
        }
        if slots.len() > 1 {
            self.batch_members.insert(record_offset, slots);
        }
        self.spill_write_pos = record_offset.saturating_add(written);
        self.spill_count += 1;
        Ok(())
    }

    /// Restores items taken from the ring buffer back to memory after a
    /// failed batch spill so the caller can retry or shut down cleanly.
    fn restore_taken(&mut self, taken: Vec<(u64, T, usize)>) {
        for (seq, item, _) in taken {
            self.inner.force_insert(seq, item);
        }
    }

    /// Serializes a single item to the spill file.
    ///
    /// On [`io::ErrorKind::NotFound`] for a directory-backed buffer this
    /// invokes [`recreate_spill_dir`](Self::recreate_spill_dir) and retries
    /// once. All other errors (ENOSPC, partial writes via the
    /// [`Write::write_all`] contract, encoder failures) bubble up unchanged.
    fn spill_item(&mut self, sequence: u64, item: &T) -> io::Result<()> {
        // Encode payload up front so a codec error never leaves a partial
        // record in the spill file.
        let mut payload = Vec::new();
        item.encode(&mut payload)?;
        if payload.len() > u32::MAX as usize {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "spill record exceeds u32::MAX bytes",
            ));
        }
        let len = payload.len() as u32;

        match self.write_record(&len.to_le_bytes(), &payload) {
            Ok(written) => {
                self.spill_index.insert(sequence, self.spill_write_pos);
                self.spill_write_pos += written;
                Ok(())
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound && self.spill_dir.is_some() => {
                // Temp directory vanished mid-transfer. Recovery is only
                // safe when no prior items had been spilled - otherwise
                // those items are lost on disk and silently continuing
                // would corrupt the transfer. With prior items present
                // we surface NotFound; the caller treats it as a fatal
                // I/O error and the transfer aborts with exit 11.
                if !self.spill_index.is_empty() {
                    return Err(e);
                }
                self.recreate_spill_dir()?;
                let written = self.write_record(&len.to_le_bytes(), &payload)?;
                self.spill_index.insert(sequence, self.spill_write_pos);
                self.spill_write_pos += written;
                Ok(())
            }
            Err(e) => Err(e),
        }
    }

    /// Writes a length-prefixed record to the spill file, opening it lazily.
    ///
    /// Returns the number of bytes written (always `header.len() + payload.len()`
    /// on success). All `write_all` calls obey the standard library contract
    /// of returning [`io::ErrorKind::WriteZero`] on partial writes.
    fn write_record(&mut self, header: &[u8], payload: &[u8]) -> io::Result<u64> {
        let dir = self.spill_dir.clone();
        let backend = match self.spill_file.as_mut() {
            Some(b) => b,
            None => self.spill_file.insert(open_backend(dir.as_deref())?),
        };
        let file = backend.file();
        file.seek(SeekFrom::Start(self.spill_write_pos))?;
        file.write_all(header)?;
        file.write_all(payload)?;
        Ok(header.len() as u64 + payload.len() as u64)
    }

    /// Reloads a single item from the spill file at the given offset.
    fn reload_item(&mut self, offset: u64) -> io::Result<T> {
        let backend = self
            .spill_file
            .as_mut()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "spill file not initialized"))?;
        let file = backend.file();

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

    /// Re-creates the spill directory after a [`io::ErrorKind::NotFound`].
    ///
    /// Drops any stale file handle, attempts `create_dir_all` once, and
    /// resets the in-flight write cursor and spill index. On retry success
    /// the next write opens a fresh tempfile. Any items previously spilled
    /// to the vanished file are now unrecoverable; the caller's transfer
    /// must treat the surrounding error as fatal if it needed those items.
    fn recreate_spill_dir(&mut self) -> io::Result<()> {
        let Some(dir) = self.spill_dir.clone() else {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "spill backend has no directory to re-create",
            ));
        };
        // Drop the stale file handle before recreating the parent so the
        // OS does not keep a deleted inode pinned in our process.
        self.spill_file = None;
        fs::create_dir_all(&dir)?;
        self.spill_write_pos = 0;
        self.spill_index.clear();
        self.batch_members.clear();
        self.dir_recreate_count += 1;
        Ok(())
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

    /// Codec wrapper whose `encode` fails on demand. Used to inject ENOSPC
    /// and partial-write scenarios without touching the real filesystem.
    #[derive(Clone, Copy)]
    struct FailingCodec {
        value: u64,
        size: usize,
        fail_kind: Option<io::ErrorKind>,
    }

    impl SpillCodec for FailingCodec {
        fn encode(&self, w: &mut dyn Write) -> io::Result<()> {
            if let Some(kind) = self.fail_kind {
                return Err(io::Error::new(kind, "injected encode failure"));
            }
            w.write_all(&self.value.to_le_bytes())?;
            // Pad to claimed size so memory accounting matches.
            if self.size > 8 {
                w.write_all(&vec![0u8; self.size - 8])?;
            }
            Ok(())
        }

        fn decode(r: &mut dyn Read) -> io::Result<Self> {
            let mut buf = [0u8; 8];
            r.read_exact(&mut buf)?;
            Ok(Self {
                value: u64::from_le_bytes(buf),
                size: 8,
                fail_kind: None,
            })
        }

        fn estimated_size(&self) -> usize {
            self.size
        }
    }

    fn drain_all<T: SpillCodec>(buf: &mut SpillableReorderBuffer<T>) -> Vec<T> {
        buf.drain_ready().expect("drain should succeed")
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

        let items = drain_all(&mut buf);
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
        let items = drain_all(&mut buf);
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

        let items = drain_all(&mut buf);
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

        let items = drain_all(&mut buf);
        assert_eq!(items, vec![0, 10, 20, 30]);

        // Phase 2: Insert 4..8.
        buf.insert(7, 70).unwrap();
        buf.insert(6, 60).unwrap();
        buf.insert(5, 50).unwrap();
        buf.insert(4, 40).unwrap();

        let items = drain_all(&mut buf);
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
        let items = drain_all(&mut buf);
        assert_eq!(items, vec![0, 1, 2, 3, 4, 5]);
    }

    #[test]
    fn empty_buffer_operations() {
        let mut buf: SpillableReorderBuffer<u64> = SpillableReorderBuffer::new(8, 1024);

        assert!(buf.is_empty());
        assert_eq!(buf.buffered_count(), 0);
        assert_eq!(buf.next_expected(), 0);
        assert!(buf.next_in_order().unwrap().is_none());
        assert!(drain_all(&mut buf).is_empty());
    }

    #[test]
    fn force_insert_with_spill() {
        let mut buf: SpillableReorderBuffer<u64> = SpillableReorderBuffer::new(4, 24); // 3 items before spill

        buf.force_insert(0, 0).unwrap();
        buf.force_insert(1, 10).unwrap();
        buf.force_insert(2, 20).unwrap();
        buf.force_insert(3, 30).unwrap();
        buf.force_insert(10, 100).unwrap(); // beyond capacity, triggers grow + possibly spill

        // Drain what's available.
        let items = drain_all(&mut buf);
        assert_eq!(items, vec![0, 10, 20, 30]);

        // Items 4-9 are missing, so 10 is not yet deliverable.
        assert!(buf.next_in_order().unwrap().is_none());
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
        let items = drain_all(&mut buf);
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

        let items = drain_all(&mut buf);
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

        let items = drain_all(&mut buf);
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].ndx().get(), 10);
        assert!(items[0].is_success());
        assert_eq!(items[1].ndx().get(), 15);
        assert!(items[1].needs_retry());
        assert_eq!(items[2].ndx().get(), 20);
        assert!(items[2].is_success());
    }

    // ---- Hardening tests for ENOSPC / temp-dir vanish / partial writes ----

    #[test]
    fn enospc_during_spill_propagates_as_io_error() {
        // Threshold is tiny so the very next insert must spill. The codec
        // returns ENOSPC, simulating the kernel rejecting the spill write.
        let mut buf: SpillableReorderBuffer<FailingCodec> = SpillableReorderBuffer::new(8, 16);
        let healthy = FailingCodec {
            value: 0,
            size: 8,
            fail_kind: None,
        };
        let healthy2 = FailingCodec {
            value: 1,
            size: 16,
            fail_kind: None,
        };
        let poison = FailingCodec {
            value: 99,
            size: 64,
            fail_kind: Some(io::ErrorKind::StorageFull),
        };

        // Seed two healthy items so the spill candidate set is non-empty.
        buf.insert(0, healthy).unwrap();
        buf.insert(1, healthy2).unwrap();

        // Inserting the poisoned item pushes us over the threshold and the
        // codec rejects with ENOSPC during the spill write.
        let err = buf
            .insert(2, poison)
            .expect_err("ENOSPC must surface as an error");

        match err {
            SpillError::Io(ref e) => assert_eq!(e.kind(), io::ErrorKind::StorageFull),
            SpillError::Capacity(_) => panic!("expected I/O error, got capacity"),
        }
        assert!(err.is_out_of_space(), "is_out_of_space should be true");
    }

    #[test]
    fn partial_write_surfaces_as_write_zero() {
        // A writer that accepts one byte and then returns zero models the
        // ENOSPC-mid-record case the std library surfaces as `WriteZero`
        // through the `Write::write_all` contract.
        struct OneByteWriter {
            wrote: bool,
        }
        impl Write for OneByteWriter {
            fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
                if self.wrote {
                    Ok(0)
                } else {
                    self.wrote = true;
                    Ok(1)
                }
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        let mut writer = OneByteWriter { wrote: false };
        let codec = FailingCodec {
            value: 7,
            size: 64,
            fail_kind: None,
        };
        let err = codec
            .encode(&mut writer)
            .expect_err("partial write must surface");
        assert_eq!(err.kind(), io::ErrorKind::WriteZero);
    }

    #[test]
    fn temp_dir_vanish_recreates_when_no_prior_spills() {
        // Vanish-before-first-spill is the recoverable case: no data has
        // been written yet, so re-creating the directory and retrying
        // is safe.
        let scratch = ::tempfile::tempdir().expect("create scratch root");
        let spill_dir = scratch.path().join("spill");
        let mut buf: SpillableReorderBuffer<u64> =
            SpillableReorderBuffer::with_spill_dir(16, 8, &spill_dir)
                .expect("setup spill directory");

        // Operator wipes the spill directory before any spill happens.
        fs::remove_dir_all(&spill_dir).expect("remove spill dir");
        assert!(!spill_dir.exists());

        // These inserts trigger spills. The first spill finds the dir
        // missing, recreates it once, and retries successfully.
        buf.insert(0, 100).unwrap();
        buf.insert(1, 200).unwrap();
        buf.insert(2, 300).unwrap();

        let stats = buf.spill_stats();
        assert_eq!(
            stats.dir_recreate_events, 1,
            "expected exactly one dir recreate, got {}",
            stats.dir_recreate_events
        );
        assert!(spill_dir.exists(), "spill dir should be back");
        assert!(stats.spill_events > 0, "spill must have occurred");
    }

    #[test]
    fn temp_dir_vanish_after_prior_spills_returns_error() {
        // Vanish after prior spills is unrecoverable: those items live
        // only on the now-missing disk. We surface the I/O error rather
        // than silently lose them.
        let scratch = ::tempfile::tempdir().expect("create scratch root");
        let spill_dir = scratch.path().join("spill");
        let mut buf: SpillableReorderBuffer<u64> =
            SpillableReorderBuffer::with_spill_dir(16, 8, &spill_dir)
                .expect("setup spill directory");

        // Prime the buffer with at least one successful spill.
        buf.insert(0, 100).unwrap();
        buf.insert(1, 200).unwrap();
        assert!(buf.spill_stats().spilled_items > 0);

        // Operator wipes the spill directory mid-transfer. Drop the stale
        // file handle so the next write opens a fresh tempfile and observes
        // the missing parent.
        buf.spill_file = None;
        fs::remove_dir_all(&spill_dir).expect("remove spill dir");

        // The next insert that triggers a spill should surface NotFound
        // (or another io::Error) without panicking and without recreating
        // the directory: prior items are unrecoverable.
        let mut saw_error = false;
        for i in 2u64..6 {
            if let Err(e) = buf.insert(i, i * 100) {
                assert!(matches!(e, SpillError::Io(_)), "expected I/O error");
                saw_error = true;
                break;
            }
        }
        assert!(saw_error, "expected spill failure after dir vanish");
        assert_eq!(
            buf.spill_stats().dir_recreate_events,
            0,
            "must not silently recreate when prior items exist"
        );
    }

    #[test]
    fn dir_recreate_failure_surfaces_io_error() {
        // Point the spill dir at a path whose parent is a regular file:
        // create_dir_all is guaranteed to fail with NotADirectory or similar.
        let scratch = ::tempfile::tempdir().expect("create scratch root");
        let blocker = scratch.path().join("blocker");
        fs::write(&blocker, b"not a directory").expect("write blocker file");
        let invalid_dir = blocker.join("spill");

        // with_spill_dir performs the first create_dir_all eagerly. The
        // failure must surface cleanly rather than panicking.
        let err = SpillableReorderBuffer::<u64>::with_spill_dir(8, 8, &invalid_dir)
            .expect_err("expected create_dir_all to fail");
        // Different platforms map "parent is a file" to different ErrorKinds
        // (NotADirectory on modern Linux, Other on older toolchains, sometimes
        // AlreadyExists on macOS); any io::Error meets the contract.
        let _ = err.kind();
    }

    #[test]
    fn directory_backed_spill_round_trip() {
        // Sanity: the directory backend yields the same byte-for-byte
        // results as the default spooled backend.
        let scratch = ::tempfile::tempdir().expect("create scratch root");
        let mut buf: SpillableReorderBuffer<u64> =
            SpillableReorderBuffer::with_spill_dir(64, 24, scratch.path().join("spill"))
                .expect("setup spill directory");

        for i in (0..16).rev() {
            buf.insert(i, i * 11).unwrap();
        }
        let items = drain_all(&mut buf);
        let expected: Vec<u64> = (0..16).map(|i| i * 11).collect();
        assert_eq!(items, expected);
        assert!(buf.spill_stats().spill_events > 0);
    }

    #[test]
    fn spill_error_display_and_source() {
        let cap_err = SpillError::from(CapacityExceeded);
        assert_eq!(format!("{cap_err}"), "reorder buffer capacity exceeded");
        assert!(std::error::Error::source(&cap_err).is_none());

        let io_err = SpillError::from(io::Error::new(io::ErrorKind::StorageFull, "disk full"));
        assert!(format!("{io_err}").contains("disk full"));
        assert!(std::error::Error::source(&io_err).is_some());
    }

    // ---- SpillGranularity wiring tests (STN-5 #2339) ----

    /// Total bytes that ended up on the spill backend, regardless of which
    /// flavour (`SpooledTempFile` or `Directory`) was selected.
    fn spill_file_size<T: SpillCodec>(buf: &mut SpillableReorderBuffer<T>) -> u64 {
        let backend = buf
            .spill_file
            .as_mut()
            .expect("spill backend must exist for size probe");
        backend.file().seek(SeekFrom::End(0)).expect("seek end")
    }

    /// Populates `buf` with enough out-of-order items to force the
    /// configured spill path to run several spill events. Items are
    /// inserted in descending sequence order so the hot-zone filter does
    /// not protect them. Each item is 8 bytes apiece (`u64`).
    fn force_batch_spill(buf: &mut SpillableReorderBuffer<u64>, min_items: usize) {
        let n = (min_items + HOT_ZONE as usize + 4) as u64;
        for i in (0..n).rev() {
            buf.insert(i, i).expect("insert under capacity");
        }
    }

    #[test]
    fn granularity_whole_batch_writes_single_chunk() {
        // Default granularity packs every candidate selected by a single
        // `spill_excess` call into one length-prefixed record. The on-disk
        // size for that record is therefore `4 + sum(payloads)` with the
        // 4-byte header paid exactly once.
        let mut buf: SpillableReorderBuffer<u64> = SpillableReorderBuffer::new(128, 16);
        assert_eq!(buf.granularity(), SpillGranularity::WholeBatch);

        force_batch_spill(&mut buf, 8);

        // Walk the disk: each whole-batch record is `[u32 len][payload]`.
        // The total file size therefore equals the per-record overhead
        // (4 bytes) times the number of spill events plus the sum of the
        // encoded payloads.
        let stats = buf.spill_stats();
        let spilled = stats.spilled_items as u64;
        let on_disk = spill_file_size(&mut buf);
        let payload_bytes = spilled * 8; // u64 SpillCodec writes 8 bytes per item
        let header_bytes = stats.spill_events * 4;
        assert!(spilled > 0, "test setup must trigger at least one spill");
        assert_eq!(
            on_disk,
            payload_bytes + header_bytes,
            "WholeBatch records must amortise the 4-byte header per spill event \
             (spilled={spilled}, events={}, payload_bytes={payload_bytes}, header_bytes={header_bytes})",
            stats.spill_events
        );
        // At least one event must actually be a multi-item batch, otherwise
        // the optimisation is indistinguishable from per-item.
        assert!(
            spilled > stats.spill_events,
            "expected at least one multi-item batch (spilled={spilled}, events={})",
            stats.spill_events
        );

        // Sanity: items must still drain in order.
        let items = drain_all(&mut buf);
        assert!(!items.is_empty());
        for (i, v) in items.iter().enumerate() {
            assert_eq!(*v, i as u64, "WholeBatch reload must preserve order");
        }
    }

    #[test]
    fn granularity_per_item_writes_n_chunks() {
        // Per-item granularity writes one `[u32 len][payload]` record per
        // spilled item, so the disk footprint includes one 4-byte length
        // prefix per item.
        let mut buf: SpillableReorderBuffer<u64> = SpillableReorderBuffer::new(128, 16)
            .with_granularity(SpillGranularity::PerItem);
        assert_eq!(buf.granularity(), SpillGranularity::PerItem);

        force_batch_spill(&mut buf, 8);

        let stats = buf.spill_stats();
        let spilled = stats.spilled_items as u64;
        let on_disk = spill_file_size(&mut buf);
        let payload_bytes = spilled * 8;
        let header_bytes = spilled * 4; // one length prefix per item
        assert!(spilled > 0, "test setup must trigger at least one spill");
        assert_eq!(
            on_disk,
            payload_bytes + header_bytes,
            "PerItem records carry one 4-byte length prefix per item \
             (spilled={spilled}, payload_bytes={payload_bytes}, header_bytes={header_bytes})"
        );

        // Drain order is the same contract as the WholeBatch path.
        let items = drain_all(&mut buf);
        assert!(!items.is_empty());
        for (i, v) in items.iter().enumerate() {
            assert_eq!(*v, i as u64, "PerItem reload must preserve order");
        }
    }

    #[test]
    fn granularity_per_item_round_trip_byte_identical() {
        // Encoding and decoding under PerItem granularity round-trips every
        // item back to its original value. This pins the contract that the
        // chosen layout never corrupts payload bytes.
        let mut buf: SpillableReorderBuffer<u64> = SpillableReorderBuffer::new(64, 16)
            .with_granularity(SpillGranularity::PerItem);

        let inputs: Vec<u64> = (0..24).map(|i| (i as u64) * 7919).collect();
        for (seq, value) in inputs.iter().enumerate().rev() {
            buf.insert(seq as u64, *value).expect("insert");
        }
        assert!(buf.spill_stats().spill_events > 0);

        let drained = drain_all(&mut buf);
        assert_eq!(drained, inputs, "PerItem round-trip must be byte-identical");
    }

    #[test]
    fn granularity_whole_batch_round_trip_byte_identical() {
        // The default WholeBatch path must also round-trip every payload
        // exactly, even when several items share one packed record.
        let mut buf: SpillableReorderBuffer<u64> = SpillableReorderBuffer::new(64, 16);
        assert_eq!(buf.granularity(), SpillGranularity::WholeBatch);

        let inputs: Vec<u64> = (0..24).map(|i| (i as u64).wrapping_mul(2654435761)).collect();
        for (seq, value) in inputs.iter().enumerate().rev() {
            buf.insert(seq as u64, *value).expect("insert");
        }
        assert!(buf.spill_stats().spill_events > 0);

        let drained = drain_all(&mut buf);
        assert_eq!(
            drained, inputs,
            "WholeBatch round-trip must be byte-identical"
        );
    }
}
