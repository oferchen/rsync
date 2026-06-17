//! Public policy types that configure the reorder buffer spill layer.
//!
//! This submodule owns the user-facing knobs that select *whether* and *how*
//! the spill layer engages: [`SpillPolicy`] aggregates the byte threshold,
//! the on-disk scratch directory, the post-spill reclaim behaviour, the
//! spill granularity, the optional payload compression, and the post-read
//! reclaim choice into a single value. The parent [`super`] module owns the
//! runtime machinery ([`SpillableReorderBuffer`](super::SpillableReorderBuffer)
//! and its disk I/O paths); this submodule owns only the configuration
//! surface those paths consume.
//!
//! The default [`SpillPolicy`] disables spilling entirely, mirroring the
//! historical bare [`ReorderBuffer`](super::super::reorder::ReorderBuffer)
//! path so existing call sites keep their behaviour. The variants of
//! [`ReclaimMode`], [`SpillGranularity`], and [`SpillCompression`] are
//! deliberately additive: today only the defaults are wired through the
//! consumer pipeline, and the alternatives reserve syntactic room for
//! follow-up work without forcing another public API break. The
//! [`SpillReclaim`] knob is wired through
//! [`SpillableReorderBuffer`](super::SpillableReorderBuffer) and
//! controls whether the buffer re-spills in-memory residue after each
//! reload-from-disk delivery. Tests below pin the defaults so downstream
//! crates can rely on the contract.
//!
//! Extracted as the first split off the monolithic `spill.rs`; see the
//! `spill.rs` decomposition plan (`docs/audits/spill-rs-decomposition-plan.md`,
//! task SPL-1 #2323) for the broader submodule layout and the
//! `SpillPolicy` introduction tracked by #2336.

use std::path::{Path, PathBuf};

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

/// Post-read reclaim behaviour for the spill layer.
///
/// Controls what [`SpillableReorderBuffer`](super::SpillableReorderBuffer)
/// does immediately after reloading a spilled item from disk and delivering
/// it to the consumer.
///
/// The default, [`SpillReclaim::KeepInMemory`], matches the historical
/// behaviour: any items that were force-inserted alongside the reload stay
/// resident in memory. The alternative, [`SpillReclaim::RespillAfterRead`],
/// triggers an extra `spill_excess` pass after every successful reload so
/// the in-memory footprint stays bounded under sustained reload traffic.
/// Use it when many large batches stream through and RSS would otherwise
/// drift upward over the lifetime of a long-running transfer.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum SpillReclaim {
    /// Reloaded items and any co-resident in-memory items remain resident
    /// after delivery; the buffer never proactively re-spills.
    #[default]
    KeepInMemory,
    /// After each reload-from-disk delivery, the buffer re-spills excess
    /// in-memory items so RSS stays bounded by the configured threshold.
    RespillAfterRead,
}

/// Optional compression applied to spilled payloads.
///
/// [`SpillCompression::None`] (the default) writes raw encoded bytes, which
/// is what the existing on-disk format uses. `SpillCompression::Zstd` is
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
    /// `None` uses the default `SpooledTempFile` backend, which keeps spills
    /// in memory up to 1 MB before rolling over to a system tempfile.
    pub dir: Option<PathBuf>,
    /// Behaviour after a spill event when pressure subsides.
    pub reclaim_mode: ReclaimMode,
    /// Per-spill-event granularity.
    pub granularity: SpillGranularity,
    /// Optional payload compression for spilled bytes.
    pub compression: SpillCompression,
    /// Post-read reclaim policy. Default [`SpillReclaim::KeepInMemory`]
    /// preserves the historical behaviour.
    pub reclaim: SpillReclaim,
    /// Process RSS (in bytes) above which the spill layer engages
    /// regardless of [`threshold_bytes`](Self::threshold_bytes).
    ///
    /// `None` disables RSS-aware spilling and matches the historical
    /// byte-budget-only behaviour. When `Some`, the reorder buffer queries
    /// process RSS before consulting the byte budget and forces a spill
    /// when the threshold is crossed. RSS reads are cached for roughly
    /// 100 ms to keep the syscall overhead off the hot path.
    ///
    /// Platforms without a supported RSS source (currently Windows) treat
    /// this knob as a no-op: the byte budget retains full control.
    pub memory_pressure_bytes: Option<u64>,
    /// When `true`, the buffer never attempts disk I/O for spill operations.
    ///
    /// Instead of writing excess items to a temporary file, the buffer
    /// returns [`SpillError::SpillDisabled`](super::SpillError::SpillDisabled)
    /// when the memory threshold is exceeded. Use this on read-only
    /// filesystems, in containers without writable tmpfs, or when the
    /// caller prefers a clean error over silent disk I/O.
    ///
    /// Default is `false` (disk spill is permitted when a threshold is set).
    pub in_memory_only: bool,
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

    /// Overrides the post-read reclaim policy.
    #[must_use]
    pub fn with_reclaim(mut self, reclaim: SpillReclaim) -> Self {
        self.reclaim = reclaim;
        self
    }

    /// Sets the RSS threshold (in bytes) that forces a spill regardless of
    /// the byte budget. See
    /// [`memory_pressure_bytes`](Self::memory_pressure_bytes) for details.
    #[must_use]
    pub fn with_memory_pressure(mut self, bytes: u64) -> Self {
        self.memory_pressure_bytes = Some(bytes);
        self
    }

    /// Enables in-memory-only mode: the buffer returns
    /// [`SpillError::SpillDisabled`](super::SpillError::SpillDisabled) when
    /// the threshold is exceeded instead of writing to disk.
    #[must_use]
    pub fn with_in_memory_only(mut self) -> Self {
        self.in_memory_only = true;
        self
    }

    /// Returns `true` when the spill layer is configured to engage.
    #[must_use]
    pub const fn is_enabled(&self) -> bool {
        self.threshold_bytes.is_some()
    }

    /// Applies CLI-supplied overrides for the directory, threshold, and
    /// no-spill knobs.
    ///
    /// Each `Some` argument unconditionally replaces the corresponding
    /// field; each `None` leaves it untouched. `no_spill` is a plain
    /// `bool` because the CLI flag is a simple presence toggle. The
    /// intended call order is `defaults -> env-var loader ->
    /// apply_cli_overrides`, which cements the documented precedence rule
    /// **CLI > env > defaults**.
    pub fn apply_cli_overrides(
        &mut self,
        dir: Option<&Path>,
        threshold_bytes: Option<u64>,
        no_spill: bool,
    ) {
        if let Some(dir) = dir {
            self.dir = Some(dir.to_path_buf());
        }
        if let Some(bytes) = threshold_bytes {
            self.threshold_bytes = Some(bytes);
        }
        if no_spill {
            self.in_memory_only = true;
        }
    }

    /// Returns [`Self::default`] with any
    /// [`OC_RSYNC_SPILL_*`](super::env::ENV_SPILL_DIR) environment-variable
    /// overrides applied.
    ///
    /// Convenience wrapper around [`super::env::apply_env_overrides`] for
    /// call sites that want to materialise an env-driven policy without
    /// threading a mutable reference. Invalid env values are logged and
    /// left at the default; no variants of this constructor panic.
    #[must_use]
    pub fn from_env() -> Self {
        let mut policy = Self::default();
        super::env::apply_env_overrides(&mut policy);
        policy
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
        assert_eq!(policy.reclaim, SpillReclaim::KeepInMemory);
        assert!(policy.memory_pressure_bytes.is_none());
        assert!(!policy.in_memory_only);
    }

    #[test]
    fn with_memory_pressure_sets_rss_threshold() {
        let policy = SpillPolicy::default().with_memory_pressure(512 * 1024 * 1024);
        assert_eq!(policy.memory_pressure_bytes, Some(512 * 1024 * 1024));
        // The byte-budget knob remains independent.
        assert!(policy.threshold_bytes.is_none());
    }

    #[test]
    fn off_matches_default() {
        assert_eq!(SpillPolicy::off(), SpillPolicy::default());
    }

    #[test]
    fn with_in_memory_only_sets_flag() {
        let policy = SpillPolicy::with_threshold(4096).with_in_memory_only();
        assert!(policy.in_memory_only);
        assert!(policy.is_enabled());
    }

    #[test]
    fn in_memory_only_default_is_false() {
        assert!(!SpillPolicy::with_threshold(1024).in_memory_only);
    }

    #[test]
    fn enum_defaults_are_pinned() {
        assert_eq!(ReclaimMode::default(), ReclaimMode::KeepInMemory);
        assert_eq!(SpillGranularity::default(), SpillGranularity::WholeBatch);
        assert_eq!(SpillCompression::default(), SpillCompression::None);
        assert_eq!(SpillReclaim::default(), SpillReclaim::KeepInMemory);
    }

    #[test]
    fn with_reclaim_overrides_default() {
        let policy = SpillPolicy::with_threshold(2048).with_reclaim(SpillReclaim::RespillAfterRead);
        assert_eq!(policy.reclaim, SpillReclaim::RespillAfterRead);
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

    #[test]
    fn apply_cli_overrides_replaces_both_knobs() {
        let mut policy =
            SpillPolicy::with_threshold(1024).with_dir(PathBuf::from("/tmp/env-spill"));
        let cli_dir = PathBuf::from("/tmp/cli-spill");
        policy.apply_cli_overrides(Some(cli_dir.as_path()), Some(8 * 1024), false);
        assert_eq!(policy.dir.as_deref(), Some(cli_dir.as_path()));
        assert_eq!(policy.threshold_bytes, Some(8 * 1024));
    }

    #[test]
    fn apply_cli_overrides_none_preserves_existing_values() {
        let env_dir = PathBuf::from("/tmp/env-spill");
        let mut policy = SpillPolicy::with_threshold(2048).with_dir(env_dir.clone());
        policy.apply_cli_overrides(None, None, false);
        assert_eq!(policy.dir.as_deref(), Some(env_dir.as_path()));
        assert_eq!(policy.threshold_bytes, Some(2048));
    }

    #[test]
    fn apply_cli_overrides_can_set_fields_from_defaults() {
        let mut policy = SpillPolicy::default();
        let cli_dir = PathBuf::from("/tmp/cli-spill");
        policy.apply_cli_overrides(Some(cli_dir.as_path()), Some(64 * 1024 * 1024), false);
        assert_eq!(policy.dir.as_deref(), Some(cli_dir.as_path()));
        assert_eq!(policy.threshold_bytes, Some(64 * 1024 * 1024));
        assert!(policy.is_enabled());
    }

    #[test]
    fn apply_cli_overrides_threshold_only_keeps_dir() {
        let env_dir = PathBuf::from("/tmp/env-spill");
        let mut policy = SpillPolicy::with_threshold(1024).with_dir(env_dir.clone());
        policy.apply_cli_overrides(None, Some(4096), false);
        assert_eq!(policy.threshold_bytes, Some(4096));
        assert_eq!(policy.dir.as_deref(), Some(env_dir.as_path()));
    }

    #[test]
    fn spill_dir_flag_overrides_env() {
        // Simulate env-applied dir, then CLI override.
        let env_dir = PathBuf::from("/tmp/env-spill");
        let mut policy = SpillPolicy::with_threshold(1024).with_dir(env_dir);
        let cli_dir = PathBuf::from("/tmp/cli-spill");
        policy.apply_cli_overrides(Some(cli_dir.as_path()), None, false);
        // CLI wins.
        assert_eq!(policy.dir.as_deref(), Some(cli_dir.as_path()));
    }

    #[test]
    fn spill_threshold_bytes_flag_overrides_env() {
        // Simulate env-applied threshold, then CLI override.
        let mut policy = SpillPolicy::with_threshold(64 * 1024 * 1024);
        policy.apply_cli_overrides(None, Some(128 * 1024 * 1024), false);
        // CLI wins.
        assert_eq!(policy.threshold_bytes, Some(128 * 1024 * 1024));
    }

    #[test]
    fn flags_absent_use_env_or_default() {
        // Simulate env-applied values; CLI passes None for both knobs.
        let env_dir = PathBuf::from("/tmp/env-spill");
        let mut policy = SpillPolicy::with_threshold(32 * 1024 * 1024).with_dir(env_dir.clone());
        policy.apply_cli_overrides(None, None, false);
        // Env values preserved.
        assert_eq!(policy.dir.as_deref(), Some(env_dir.as_path()));
        assert_eq!(policy.threshold_bytes, Some(32 * 1024 * 1024));
    }

    #[test]
    fn apply_cli_overrides_no_spill_sets_in_memory_only() {
        let mut policy = SpillPolicy::with_threshold(4096);
        assert!(!policy.in_memory_only);
        policy.apply_cli_overrides(None, None, true);
        assert!(policy.in_memory_only);
    }

    #[test]
    fn apply_cli_overrides_no_spill_false_preserves_env() {
        let mut policy = SpillPolicy::with_threshold(4096).with_in_memory_only();
        assert!(policy.in_memory_only);
        policy.apply_cli_overrides(None, None, false);
        // CLI did not set --no-spill, so env-set value preserved.
        assert!(policy.in_memory_only);
    }

    #[test]
    fn no_spill_cli_overrides_env() {
        // Simulate: env did not set in_memory_only, CLI sets --no-spill.
        let mut policy = SpillPolicy::with_threshold(1024);
        assert!(!policy.in_memory_only);
        policy.apply_cli_overrides(None, None, true);
        assert!(policy.in_memory_only);
    }
}
