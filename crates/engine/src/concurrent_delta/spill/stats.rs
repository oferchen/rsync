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
    pub spill_events: u64,
    /// Total reload-from-disk events since creation.
    pub reload_events: u64,
    /// Current estimated in-memory bytes.
    pub memory_used: usize,
    /// Configured spill threshold in bytes.
    pub threshold: usize,
    /// Number of times the spill directory was re-created after vanishing.
    pub dir_recreate_events: u64,
}
