//! Public policy types that configure the reorder buffer spill layer.
//!
//! [`SpillPolicy`] groups every knob that tunes the
//! [`SpillableReorderBuffer`](super::SpillableReorderBuffer) into a single
//! value: the byte threshold, the on-disk scratch directory, the post-spill
//! reclaim behaviour, the spill granularity, and the optional payload
//! compression. The default value disables spilling entirely, mirroring the
//! historical bare [`ReorderBuffer`](super::super::reorder::ReorderBuffer)
//! path so existing call sites keep their behaviour.
//!
//! The variants of [`ReclaimMode`], [`SpillGranularity`], and
//! [`SpillCompression`] are deliberately additive: today only the defaults
//! are wired through the consumer pipeline, and the alternatives reserve
//! syntactic room for follow-up work without forcing another public API
//! break. Tests below pin the defaults so downstream crates can rely on the
//! contract.

use std::path::PathBuf;

/// What to do when memory pressure subsides after a spill event.
///
/// The default - [`ReclaimMode::KeepInMemory`] - matches the historical
/// "spill once, never re-spill" behaviour: once items are reloaded from
/// disk they remain in memory even if pressure returns. The alternative,
/// [`ReclaimMode::ReSpillIfPressureContinues`], allows the buffer to push
/// items back to disk on subsequent pressure events at the cost of extra
/// disk traffic during sustained bursts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum ReclaimMode {
    /// Reloaded items stay resident in memory for the rest of the transfer.
    #[default]
    KeepInMemory,
    /// Reloaded items may be spilled back to disk on subsequent pressure.
    ReSpillIfPressureContinues,
}

/// Granularity of a single spill event.
///
/// [`SpillGranularity::WholeBatch`] (the default) writes a contiguous run
/// of reorder slots in one disk operation, matching the current
/// [`SpillableReorderBuffer`](super::SpillableReorderBuffer) behaviour.
/// [`SpillGranularity::PerItem`] reserves room for a per-record spill
/// strategy that trades batch efficiency for finer eviction control.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum SpillGranularity {
    /// Spill a contiguous batch of items per disk operation.
    #[default]
    WholeBatch,
    /// Spill individual items as the budget is exceeded.
    PerItem,
}

/// Optional compression applied to spilled payloads.
///
/// [`SpillCompression::None`] (the default) writes raw encoded bytes, which
/// is what the existing on-disk format uses. [`SpillCompression::Zstd`] is
/// only constructable behind the `spill-compression` feature; this keeps
/// the default builds free of the codec dependency while leaving the enum
/// extensible.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum SpillCompression {
    /// Do not compress spilled payloads.
    #[default]
    None,
    /// Compress spilled payloads with zstd at the given level.
    ///
    /// Constructable only when the `spill-compression` feature is enabled.
    #[cfg(feature = "spill-compression")]
    Zstd {
        /// zstd compression level forwarded to the codec.
        level: i32,
    },
}

/// Aggregate of every public spill knob for [`ConcurrentDeltaConfig`].
///
/// [`SpillPolicy::default`] disables spilling (`threshold_bytes: None`) so
/// the consumer keeps its bare [`ReorderBuffer`](super::super::reorder::ReorderBuffer)
/// path. Set [`threshold_bytes`](Self::threshold_bytes) to opt in; the
/// other fields layer in policy adjustments on top.
///
/// [`ConcurrentDeltaConfig`]: super::super::config::ConcurrentDeltaConfig
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SpillPolicy {
    /// Memory budget (in bytes) before the spill layer engages.
    ///
    /// `None` disables spilling and the consumer uses the bare ring buffer.
    pub threshold_bytes: Option<u64>,
    /// Explicit on-disk scratch directory for spilled items.
    ///
    /// `None` defers to [`std::env::temp_dir`] at spill time. The chosen
    /// directory is honoured on the first spill of every batch so updates
    /// to this field take effect on the next consumer spawn without being
    /// cached at policy construction time.
    pub dir: Option<PathBuf>,
    /// Behaviour after a spill event when pressure subsides.
    pub reclaim_mode: ReclaimMode,
    /// Per-spill-event granularity.
    pub granularity: SpillGranularity,
    /// Optional payload compression for spilled bytes.
    pub compression: SpillCompression,
}

impl SpillPolicy {
    /// Returns a policy with spilling disabled. Equivalent to [`Self::default`].
    #[must_use]
    pub fn off() -> Self {
        Self::default()
    }

    /// Returns a policy that engages the spill layer at the given byte budget.
    ///
    /// All other knobs use their defaults; chain the `with_*` builders to
    /// override individual fields.
    #[must_use]
    pub fn with_threshold(threshold_bytes: u64) -> Self {
        Self {
            threshold_bytes: Some(threshold_bytes),
            ..Self::default()
        }
    }

    /// Sets the explicit on-disk scratch directory.
    #[must_use]
    pub fn with_dir(mut self, dir: PathBuf) -> Self {
        self.dir = Some(dir);
        self
    }

    /// Overrides the post-spill reclaim behaviour.
    #[must_use]
    pub fn with_reclaim_mode(mut self, mode: ReclaimMode) -> Self {
        self.reclaim_mode = mode;
        self
    }

    /// Overrides the per-spill-event granularity.
    #[must_use]
    pub fn with_granularity(mut self, granularity: SpillGranularity) -> Self {
        self.granularity = granularity;
        self
    }

    /// Overrides the spill-payload compression codec.
    #[must_use]
    pub fn with_compression(mut self, compression: SpillCompression) -> Self {
        self.compression = compression;
        self
    }

    /// Returns `true` when the spill layer is configured to engage.
    #[must_use]
    pub const fn is_enabled(&self) -> bool {
        self.threshold_bytes.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_disables_spill() {
        let policy = SpillPolicy::default();
        assert!(!policy.is_enabled());
        assert!(policy.threshold_bytes.is_none());
        assert!(policy.dir.is_none());
        assert_eq!(policy.reclaim_mode, ReclaimMode::KeepInMemory);
        assert_eq!(policy.granularity, SpillGranularity::WholeBatch);
        assert_eq!(policy.compression, SpillCompression::None);
    }

    #[test]
    fn off_matches_default() {
        assert_eq!(SpillPolicy::off(), SpillPolicy::default());
    }

    #[test]
    fn enum_defaults_are_pinned() {
        assert_eq!(ReclaimMode::default(), ReclaimMode::KeepInMemory);
        assert_eq!(SpillGranularity::default(), SpillGranularity::WholeBatch);
        assert_eq!(SpillCompression::default(), SpillCompression::None);
    }

    #[test]
    fn with_threshold_enables_spill() {
        let policy = SpillPolicy::with_threshold(64 * 1024);
        assert!(policy.is_enabled());
        assert_eq!(policy.threshold_bytes, Some(64 * 1024));
        assert!(policy.dir.is_none());
    }

    #[test]
    fn builder_chain_sets_every_field() {
        let dir = PathBuf::from("/tmp/oc-rsync-spill");
        let policy = SpillPolicy::with_threshold(2048)
            .with_dir(dir.clone())
            .with_reclaim_mode(ReclaimMode::ReSpillIfPressureContinues)
            .with_granularity(SpillGranularity::PerItem);
        assert_eq!(policy.threshold_bytes, Some(2048));
        assert_eq!(policy.dir.as_deref(), Some(dir.as_path()));
        assert_eq!(policy.reclaim_mode, ReclaimMode::ReSpillIfPressureContinues);
        assert_eq!(policy.granularity, SpillGranularity::PerItem);
        assert_eq!(policy.compression, SpillCompression::None);
    }

    #[test]
    fn with_compression_overrides_default() {
        let policy = SpillPolicy::with_threshold(1024).with_compression(SpillCompression::None);
        assert_eq!(policy.compression, SpillCompression::None);
    }

    #[test]
    fn policies_are_value_equal() {
        let a = SpillPolicy::with_threshold(4096).with_granularity(SpillGranularity::PerItem);
        let b = SpillPolicy::with_threshold(4096).with_granularity(SpillGranularity::PerItem);
        assert_eq!(a, b);
    }
}
