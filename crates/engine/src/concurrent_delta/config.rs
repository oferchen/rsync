//! Runtime configuration for the concurrent delta pipeline.
//!
//! [`ConcurrentDeltaConfig`] aggregates the knobs that tune the
//! [`DeltaConsumer`](super::consumer::DeltaConsumer) without changing its
//! public type signature. The primary knob is
//! [`spill_policy`](ConcurrentDeltaConfig::spill_policy), which opts the
//! consumer into bounded-memory spill-to-tempfile via
//! [`SpillableReorderBuffer`](super::spill::SpillableReorderBuffer) and
//! exposes the full [`SpillPolicy`] surface.
//!
//! # Defaults
//!
//! [`ConcurrentDeltaConfig::default`] returns a configuration that mirrors
//! the historical behaviour: bare [`ReorderBuffer`](super::reorder::ReorderBuffer)
//! with no spill layer. Set `spill_policy.threshold_bytes` to opt in.
//!
//! # Backwards compatibility
//!
//! The legacy fields `spill_threshold_bytes` and `spill_dir` were collapsed
//! into [`SpillPolicy`]. The [`spill_threshold_bytes`](ConcurrentDeltaConfig::spill_threshold_bytes)
//! and [`spill_dir`](ConcurrentDeltaConfig::spill_dir) accessors are kept as
//! deprecated shims that forward to [`spill_policy`](ConcurrentDeltaConfig::spill_policy).
//! The `with_spill_threshold` / `with_spill_dir` constructors are not
//! deprecated; they remain the supported way to populate the policy.

use std::path::{Path, PathBuf};

use super::spill::policy::SpillPolicy;

/// Tunable runtime knobs for the concurrent delta pipeline.
///
/// Pass an instance to [`DeltaConsumer::spawn_with_config`](super::consumer::DeltaConsumer::spawn_with_config)
/// to override the historical fixed-capacity reorder behaviour. The default
/// value preserves backwards compatibility - no spill, no extra plumbing.
#[derive(Debug, Clone, Default)]
pub struct ConcurrentDeltaConfig {
    /// Spill policy that controls bounded-memory reordering.
    ///
    /// The default value disables spilling. Populate
    /// [`SpillPolicy::threshold_bytes`] to engage the
    /// [`SpillableReorderBuffer`](super::spill::SpillableReorderBuffer)
    /// path; the other [`SpillPolicy`] knobs layer additional policy on top.
    pub spill_policy: SpillPolicy,
}

impl ConcurrentDeltaConfig {
    /// Returns a configuration that disables the spill layer.
    ///
    /// Equivalent to [`ConcurrentDeltaConfig::default`].
    #[must_use]
    pub fn off() -> Self {
        Self::default()
    }

    /// Returns a configuration that enables the spill layer with the given
    /// byte threshold.
    #[must_use]
    pub fn with_spill_threshold(threshold_bytes: u64) -> Self {
        Self {
            spill_policy: SpillPolicy::with_threshold(threshold_bytes),
        }
    }

    /// Returns a configuration that wires through an explicit [`SpillPolicy`].
    #[must_use]
    pub fn with_spill_policy(spill_policy: SpillPolicy) -> Self {
        Self { spill_policy }
    }

    /// Sets an explicit on-disk scratch directory for spilled items.
    ///
    /// Forwards to [`SpillPolicy::with_dir`]. Has no effect unless the spill
    /// layer is also enabled via
    /// [`with_spill_threshold`](Self::with_spill_threshold) or by populating
    /// [`spill_policy`](Self::spill_policy) directly.
    #[must_use]
    pub fn with_spill_dir(mut self, dir: PathBuf) -> Self {
        self.spill_policy.dir = Some(dir);
        self
    }

    /// Returns `true` when the spill layer is configured to engage.
    #[must_use]
    pub const fn spill_enabled(&self) -> bool {
        self.spill_policy.threshold_bytes.is_some()
    }

    /// Deprecated forwarding shim for the former `spill_threshold_bytes`
    /// field. New code should read [`spill_policy`](Self::spill_policy)
    /// directly.
    #[deprecated(note = "use spill_policy.threshold_bytes")]
    #[must_use]
    pub const fn spill_threshold_bytes(&self) -> Option<u64> {
        self.spill_policy.threshold_bytes
    }

    /// Deprecated forwarding shim for the former `spill_dir` field. New code
    /// should read [`spill_policy`](Self::spill_policy) directly.
    #[deprecated(note = "use spill_policy.dir")]
    #[must_use]
    pub fn spill_dir(&self) -> Option<&Path> {
        self.spill_policy.dir.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::concurrent_delta::spill::policy::{ReclaimMode, SpillCompression, SpillGranularity};

    #[test]
    fn default_disables_spill() {
        let cfg = ConcurrentDeltaConfig::default();
        assert!(!cfg.spill_enabled());
        assert!(cfg.spill_policy.threshold_bytes.is_none());
        assert!(cfg.spill_policy.dir.is_none());
        assert_eq!(cfg.spill_policy.reclaim_mode, ReclaimMode::KeepInMemory);
        assert_eq!(cfg.spill_policy.granularity, SpillGranularity::WholeBatch);
        assert_eq!(cfg.spill_policy.compression, SpillCompression::None);
    }

    #[test]
    fn off_matches_default() {
        let a = ConcurrentDeltaConfig::off();
        let b = ConcurrentDeltaConfig::default();
        assert_eq!(a.spill_policy, b.spill_policy);
    }

    #[test]
    fn with_spill_threshold_enables_spill() {
        let cfg = ConcurrentDeltaConfig::with_spill_threshold(16 * 1024);
        assert!(cfg.spill_enabled());
        assert_eq!(cfg.spill_policy.threshold_bytes, Some(16 * 1024));
        assert!(cfg.spill_policy.dir.is_none());
    }

    #[test]
    fn with_spill_dir_sets_directory() {
        let dir = PathBuf::from("/tmp/oc-rsync-spill");
        let cfg = ConcurrentDeltaConfig::with_spill_threshold(1024).with_spill_dir(dir.clone());
        assert!(cfg.spill_enabled());
        assert_eq!(cfg.spill_policy.dir.as_deref(), Some(dir.as_path()));
    }

    #[test]
    fn with_spill_policy_wires_full_policy() {
        let policy = SpillPolicy::with_threshold(8 * 1024)
            .with_granularity(SpillGranularity::PerItem)
            .with_reclaim_mode(ReclaimMode::ReSpillIfPressureContinues);
        let cfg = ConcurrentDeltaConfig::with_spill_policy(policy.clone());
        assert_eq!(cfg.spill_policy, policy);
        assert!(cfg.spill_enabled());
    }

    #[test]
    #[allow(deprecated)]
    fn deprecated_accessors_forward_to_policy() {
        let dir = PathBuf::from("/tmp/oc-rsync-spill-legacy");
        let cfg = ConcurrentDeltaConfig::with_spill_threshold(2048).with_spill_dir(dir.clone());
        assert_eq!(cfg.spill_threshold_bytes(), Some(2048));
        assert_eq!(cfg.spill_dir(), Some(dir.as_path()));
    }
}
