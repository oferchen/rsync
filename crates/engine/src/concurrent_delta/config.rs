//! Runtime configuration for the concurrent delta pipeline.
//!
//! [`ConcurrentDeltaConfig`] aggregates the knobs that tune the
//! [`DeltaConsumer`](super::consumer::DeltaConsumer) without changing its
//! public type signature. The only knob today is
//! [`spill_threshold_bytes`](ConcurrentDeltaConfig::spill_threshold_bytes),
//! which opts the consumer into bounded-memory spill-to-tempfile via
//! [`SpillableReorderBuffer`](super::spill::SpillableReorderBuffer).
//!
//! # Defaults
//!
//! [`ConcurrentDeltaConfig::default`] returns a configuration that mirrors
//! the historical behaviour: bare [`ReorderBuffer`](super::reorder::ReorderBuffer)
//! with no spill layer. Set `spill_threshold_bytes` to opt in.

use std::path::PathBuf;

/// Tunable runtime knobs for the concurrent delta pipeline.
///
/// Pass an instance to [`DeltaConsumer::spawn_with_config`](super::consumer::DeltaConsumer::spawn_with_config)
/// to override the historical fixed-capacity reorder behaviour. The default
/// value preserves backwards compatibility - no spill, no extra plumbing.
#[derive(Debug, Clone, Default)]
pub struct ConcurrentDeltaConfig {
    /// Optional memory budget for the reorder buffer in bytes.
    ///
    /// `None` (the default) keeps the consumer on the bare
    /// [`ReorderBuffer`](super::reorder::ReorderBuffer) path - inserts that
    /// overflow the ring fall back to capacity doubling. `Some(threshold)`
    /// switches the consumer to
    /// [`SpillableReorderBuffer`](super::spill::SpillableReorderBuffer): when
    /// the estimated in-memory footprint exceeds `threshold`, the oldest
    /// (highest-sequence) buffered items are written to a tempfile so the
    /// in-memory ring stays bounded.
    pub spill_threshold_bytes: Option<u64>,
    /// Optional explicit on-disk scratch directory for spilled items.
    ///
    /// Only consulted when `spill_threshold_bytes` is `Some`. `None` uses the
    /// default `SpooledTempFile` backend, which keeps spills in memory up to
    /// 1 MB before rolling over to a system tempfile.
    pub spill_dir: Option<PathBuf>,
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
            spill_threshold_bytes: Some(threshold_bytes),
            spill_dir: None,
        }
    }

    /// Sets an explicit on-disk scratch directory for spilled items.
    #[must_use]
    pub fn with_spill_dir(mut self, dir: PathBuf) -> Self {
        self.spill_dir = Some(dir);
        self
    }

    /// Returns `true` when the spill layer is configured to engage.
    #[must_use]
    pub const fn spill_enabled(&self) -> bool {
        self.spill_threshold_bytes.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_disables_spill() {
        let cfg = ConcurrentDeltaConfig::default();
        assert!(!cfg.spill_enabled());
        assert!(cfg.spill_threshold_bytes.is_none());
        assert!(cfg.spill_dir.is_none());
    }

    #[test]
    fn off_matches_default() {
        let a = ConcurrentDeltaConfig::off();
        let b = ConcurrentDeltaConfig::default();
        assert_eq!(a.spill_threshold_bytes, b.spill_threshold_bytes);
        assert_eq!(a.spill_dir, b.spill_dir);
    }

    #[test]
    fn with_spill_threshold_enables_spill() {
        let cfg = ConcurrentDeltaConfig::with_spill_threshold(16 * 1024);
        assert!(cfg.spill_enabled());
        assert_eq!(cfg.spill_threshold_bytes, Some(16 * 1024));
        assert!(cfg.spill_dir.is_none());
    }

    #[test]
    fn with_spill_dir_sets_directory() {
        let dir = PathBuf::from("/tmp/oc-rsync-spill");
        let cfg = ConcurrentDeltaConfig::with_spill_threshold(1024).with_spill_dir(dir.clone());
        assert!(cfg.spill_enabled());
        assert_eq!(cfg.spill_dir.as_deref(), Some(dir.as_path()));
    }
}
