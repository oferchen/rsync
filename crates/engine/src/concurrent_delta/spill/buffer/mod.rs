//! [`SpillableReorderBuffer`] - reorder buffer with disk-backed overflow.
//!
//! The struct, its [`Debug`] impl, and the shared on-disk format constants
//! live here. Method `impl` blocks are split across focused submodules so each
//! file stays under the LoC cap:
//!
//! - `lifecycle` - constructors, builders, accessors.
//! - `insert` - inbound insert paths and the spill-state eviction helper.
//! - `spill` - spill-to-disk path: candidate selection, per-item and
//!   whole-batch writers, RSS pressure trigger, compression wrapper, raw
//!   record writer, and directory recreation.
//! - `reload` - reload-from-disk path: in-order delivery, single-item and
//!   batch reload, on-disk tag dispatch.
//!
//! The parent [`spill`](super) module re-exports [`SpillableReorderBuffer`]
//! so the canonical public path
//! (`engine::concurrent_delta::spill::SpillableReorderBuffer`) stays stable.

use std::collections::BTreeMap;
use std::path::PathBuf;

use super::super::reorder::ReorderBuffer;
use super::tempfile::SpillBackend;
use super::{SpillCodec, SpillCompression, SpillGranularity, SpillReclaim};

mod insert;
mod lifecycle;
mod reload;
mod spill;

#[cfg(test)]
mod tests;

/// Minimum number of items to keep in memory around `next_expected`.
///
/// Items within `[next_expected, next_expected + HOT_ZONE)` are never
/// spilled to avoid repeated disk round-trips for items about to be
/// delivered.
const HOT_ZONE: u64 = 16;

/// On-disk tag byte marking a raw (uncompressed) payload record.
///
/// Every spill record on disk is prefixed by a single tag byte before the
/// length and payload so the reader can dispatch without ambiguity. `0x00`
/// means "decode the following payload as-is".
const SPILL_TAG_RAW: u8 = 0x00;

/// On-disk tag byte marking a zstd-compressed payload record.
///
/// `0x01` means "decompress the following payload with zstd, then decode the
/// inflated bytes". The reader returns [`SpillError`](super::SpillError)`::UnsupportedCompression`
/// when this tag is observed in a build without the `spill-compression`
/// feature, instead of attempting to decode garbage.
const SPILL_TAG_ZSTD: u8 = 0x01;

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
    /// chosen by a single `spill_excess` call into one length-prefixed
    /// record. [`SpillGranularity::PerItem`] keeps the historical
    /// one-record-per-item layout.
    granularity: SpillGranularity,
    /// Running count of spill-to-disk events (for diagnostics).
    spill_count: u64,
    /// Running count of reload-from-disk events (for diagnostics).
    reload_count: u64,
    /// Running count of `create_dir_all` retries after the spill directory
    /// disappeared mid-transfer.
    dir_recreate_count: u64,
    /// Codec applied to each spill record's payload. `SpillCompression::None`
    /// (the default) keeps the historical raw-byte format. `Zstd` is only
    /// constructable behind the `spill-compression` Cargo feature.
    compression: SpillCompression,
    /// Post-read reclaim behaviour. Default keeps the historical "spill once,
    /// drain freely" path; [`SpillReclaim::RespillAfterRead`] enables the
    /// post-reload `spill_excess` pass.
    reclaim: SpillReclaim,
    /// Optional process-RSS threshold (in bytes) that forces a spill when
    /// crossed. `None` preserves the historical byte-budget-only behaviour.
    /// Wired from [`SpillPolicy::memory_pressure_bytes`](super::policy::SpillPolicy::memory_pressure_bytes).
    memory_pressure_bytes: Option<u64>,
    /// When `true`, the buffer never attempts disk I/O for spill operations
    /// and instead returns [`SpillError::SpillDisabled`](super::SpillError::SpillDisabled).
    /// Wired from [`SpillPolicy::in_memory_only`](super::policy::SpillPolicy::in_memory_only).
    in_memory_only: bool,
    /// One-shot flag that ensures the spill-activation warning fires at most
    /// once per transfer. Set to `true` after the first successful spill
    /// event emits a `tracing::warn!`.
    spill_warned: bool,
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
            .field("compression", &self.compression)
            .field("reclaim", &self.reclaim)
            .field("in_memory_only", &self.in_memory_only)
            .field("spill_warned", &self.spill_warned)
            .finish()
    }
}
