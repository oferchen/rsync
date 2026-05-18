//! Constructors, builder methods, and accessor methods for
//! [`SpillableReorderBuffer`].

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use super::super::super::reorder::ReorderBuffer;
use super::super::{
    DEFAULT_SPILL_THRESHOLD, SpillCodec, SpillCompression, SpillGranularity, SpillReclaim,
    SpillStats,
};
use super::SpillableReorderBuffer;

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
            compression: SpillCompression::None,
            reclaim: SpillReclaim::default(),
            memory_pressure_bytes: None,
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
            compression: SpillCompression::None,
            reclaim: SpillReclaim::default(),
            memory_pressure_bytes: None,
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

    /// Sets the per-record compression codec applied to spilled payloads.
    ///
    /// [`SpillCompression::None`] (the default) writes raw encoded bytes,
    /// matching the historical on-disk format. [`SpillCompression::Zstd`] is
    /// only constructable behind the `spill-compression` Cargo feature, so a
    /// default build cannot reach the Zstd branch at compile time - that
    /// `#[cfg]` gate is the "fail fast at construction" guarantee.
    #[must_use]
    pub fn with_compression(mut self, compression: SpillCompression) -> Self {
        self.compression = compression;
        self
    }

    /// Returns the codec currently applied to spilled payloads.
    #[must_use]
    pub fn compression(&self) -> SpillCompression {
        self.compression
    }

    /// Updates the post-read reclaim policy in place.
    pub fn set_reclaim(&mut self, reclaim: SpillReclaim) {
        self.reclaim = reclaim;
    }

    /// Returns the configured post-read reclaim policy.
    #[must_use]
    pub fn reclaim(&self) -> SpillReclaim {
        self.reclaim
    }

    /// Consuming builder that sets the post-read reclaim policy.
    #[must_use]
    pub fn with_reclaim(mut self, reclaim: SpillReclaim) -> Self {
        self.reclaim = reclaim;
        self
    }

    /// Sets the process-RSS threshold (in bytes) above which an insert
    /// forces a spill regardless of the byte budget.
    ///
    /// `None` (the default) preserves the historical behaviour where only
    /// `memory_used > threshold` triggers a spill. Pass `Some(bytes)` to
    /// engage the RSS-aware trigger; see
    /// [`SpillPolicy::memory_pressure_bytes`](super::super::policy::SpillPolicy::memory_pressure_bytes)
    /// for the platform support matrix.
    #[must_use]
    pub fn with_memory_pressure_bytes(mut self, bytes: Option<u64>) -> Self {
        self.memory_pressure_bytes = bytes;
        self
    }

    /// Returns the configured RSS-pressure threshold in bytes, if any.
    #[must_use]
    pub fn memory_pressure_bytes(&self) -> Option<u64> {
        self.memory_pressure_bytes
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
}
