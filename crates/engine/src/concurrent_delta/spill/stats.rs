//! Diagnostic counters surfaced by the spill layer.
//!
//! [`SpillStats`] is a plain-data snapshot of the spill activity tracked by
//! [`SpillableReorderBuffer`](super::SpillableReorderBuffer). It is intended
//! for logging, metrics, and tests; the buffer itself owns the live counters
//! and exposes a copy via
//! [`spill_stats`](super::SpillableReorderBuffer::spill_stats).

/// Diagnostic counters for spill activity.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SpillStats {
    /// Number of items currently spilled to disk.
    pub spilled_items: usize,
    /// Total spill-to-disk events since creation.
    ///
    /// One event is one on-disk record written. Under
    /// [`SpillGranularity::PerItem`](super::SpillGranularity::PerItem) this
    /// rises once per spilled item; under
    /// [`SpillGranularity::WholeBatch`](super::SpillGranularity::WholeBatch)
    /// it rises once per batch. See [`spill_activations`](Self::spill_activations)
    /// for a per-call counter that is invariant to granularity.
    pub spill_events: u64,
    /// Total spill-activation events since creation.
    ///
    /// Counts each `spill_excess` call that successfully wrote at least one
    /// record, regardless of how many records that call produced. Adaptive
    /// ring sizing (ROB-7) and bench normal-operation spill-rate audits
    /// (ROB-6) use this counter because it is invariant to
    /// [`SpillGranularity`](super::SpillGranularity).
    pub spill_activations: u64,
    /// Total reload-from-disk events since creation.
    pub reload_events: u64,
    /// Current estimated in-memory bytes.
    pub memory_used: usize,
    /// Configured spill threshold in bytes.
    pub threshold: usize,
    /// Number of times the spill directory was re-created after vanishing.
    pub dir_recreate_events: u64,
}
