//! Runtime compression-level adaptation strategy.
//!
//! Defines the [`AdaptiveLevelStrategy`] trait for picking a per-block
//! compression level based on observed compressibility. The default
//! implementation, [`DefaultAdaptiveLevelStrategy`], smooths the observed
//! compression ratio with an EWMA and ratchets the encoder level up or down
//! when the smoothed ratio crosses configured thresholds.
//!
//! # Wire compatibility
//!
//! Compression level only affects sender-side CPU cost; the wire format
//! (token framing, message headers, codec advertisement) is unchanged. This
//! adaptation is therefore opt-in and is never advertised as a protocol
//! capability.
//!
//! Upstream rsync (`target/interop/upstream-src/rsync-3.4.1/token.c`) selects
//! a compression level once at session start via `do_compression_level` and
//! does not adjust it at runtime - this strategy is a Rust-side optimisation
//! layered on top of the upstream-compatible encoders.
//!
//! # Default behaviour
//!
//! [`DefaultAdaptiveLevelStrategy`] is disabled by default in the encoder
//! pipeline. The fixed-level path remains the default; this trait is wired in
//! only when an explicit configuration knob requests adaptive tuning.

use core::fmt::Debug;

/// Strategy for choosing a compression level at runtime.
///
/// Implementations observe the compression ratio achieved by the previous
/// block and recommend a level for the next block. The level is interpreted
/// in the codec's own scale (e.g. 1..=9 for zlib, 1..=19 for zstd); callers
/// are responsible for clamping the result to the codec's valid range.
pub trait AdaptiveLevelStrategy: Send + Sync + Debug {
    /// Returns the recommended compression level for the next block.
    ///
    /// `observed_ratio` is the compressed-to-input size ratio of the most
    /// recent block (0.0 < ratio <= 1.0+ - values above 1.0 indicate the
    /// payload expanded under compression). `current_level` is the level
    /// that produced the observation. Returned levels must be valid for the
    /// active codec; callers clamp before passing to the encoder.
    fn recommend_level(&mut self, observed_ratio: f64, current_level: i32) -> i32;

    /// Resets any internal smoothing state.
    ///
    /// Called when a new file or session begins so historical ratios from
    /// unrelated payloads do not bias future decisions.
    fn reset(&mut self);
}

/// Inclusive bounds clamping the level returned by an adaptive strategy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LevelBounds {
    /// Lowest level the strategy may return. Mapped to the codec's fastest
    /// configuration.
    pub min: i32,
    /// Highest level the strategy may return. Mapped to the codec's slowest
    /// (best-ratio) configuration.
    pub max: i32,
}

impl LevelBounds {
    /// Builds a bounds pair, ordering arguments so `min <= max`.
    #[must_use]
    pub const fn new(min: i32, max: i32) -> Self {
        if min <= max {
            Self { min, max }
        } else {
            Self { min: max, max: min }
        }
    }

    /// Clamps the supplied level into `[min, max]`.
    #[must_use]
    pub const fn clamp(&self, level: i32) -> i32 {
        if level < self.min {
            self.min
        } else if level > self.max {
            self.max
        } else {
            level
        }
    }
}

/// Tunable thresholds and smoothing factor for [`DefaultAdaptiveLevelStrategy`].
#[derive(Clone, Copy, Debug)]
pub struct AdaptiveLevelConfig {
    /// EWMA weight applied to the latest observation. Must lie in `(0.0, 1.0]`.
    /// Closer to `1.0` reacts faster; closer to `0.0` is more inertial.
    pub smoothing_alpha: f64,
    /// Smoothed ratio at or above which data is considered "incompressible";
    /// the strategy ratchets the level **down** to save CPU.
    pub poor_ratio_threshold: f64,
    /// Smoothed ratio at or below which data is considered "highly
    /// compressible"; the strategy ratchets the level **up** to capture more
    /// gains.
    pub good_ratio_threshold: f64,
    /// Inclusive bounds clamping every recommendation.
    pub bounds: LevelBounds,
}

impl AdaptiveLevelConfig {
    /// Default tuning suitable for zlib's 1..=9 scale.
    ///
    /// - `smoothing_alpha = 0.3` weights recent blocks without ignoring history.
    /// - `poor_ratio_threshold = 0.95` treats payloads compressing to 95 %+
    ///   of their original size as effectively incompressible.
    /// - `good_ratio_threshold = 0.5` rewards payloads that halve under
    ///   compression with a higher level.
    pub const ZLIB_DEFAULT: Self = Self {
        smoothing_alpha: 0.3,
        poor_ratio_threshold: 0.95,
        good_ratio_threshold: 0.5,
        bounds: LevelBounds::new(1, 9),
    };

    /// Default tuning for zstd's 1..=19 scale.
    pub const ZSTD_DEFAULT: Self = Self {
        smoothing_alpha: 0.3,
        poor_ratio_threshold: 0.95,
        good_ratio_threshold: 0.5,
        bounds: LevelBounds::new(1, 19),
    };
}

impl Default for AdaptiveLevelConfig {
    fn default() -> Self {
        Self::ZLIB_DEFAULT
    }
}

/// Default [`AdaptiveLevelStrategy`] implementation using EWMA smoothing.
///
/// Maintains an exponentially weighted moving average of the observed
/// compression ratio. When the EWMA exceeds [`AdaptiveLevelConfig::poor_ratio_threshold`]
/// the strategy decreases the level by one step (cheaper compression,
/// trading ratio for throughput). When the EWMA falls at or below
/// [`AdaptiveLevelConfig::good_ratio_threshold`] the strategy increases the
/// level by one step (more CPU, better ratio). Levels are always clamped to
/// the configured [`LevelBounds`].
#[derive(Clone, Copy, Debug)]
pub struct DefaultAdaptiveLevelStrategy {
    config: AdaptiveLevelConfig,
    smoothed_ratio: Option<f64>,
}

impl DefaultAdaptiveLevelStrategy {
    /// Creates a new strategy with the supplied tuning.
    #[must_use]
    pub const fn new(config: AdaptiveLevelConfig) -> Self {
        Self {
            config,
            smoothed_ratio: None,
        }
    }

    /// Creates a strategy preconfigured for zlib's 1..=9 level scale.
    #[must_use]
    pub const fn for_zlib() -> Self {
        Self::new(AdaptiveLevelConfig::ZLIB_DEFAULT)
    }

    /// Creates a strategy preconfigured for zstd's 1..=19 level scale.
    #[must_use]
    pub const fn for_zstd() -> Self {
        Self::new(AdaptiveLevelConfig::ZSTD_DEFAULT)
    }

    /// Returns the most recently smoothed ratio, if any blocks have been observed.
    #[must_use]
    pub const fn smoothed_ratio(&self) -> Option<f64> {
        self.smoothed_ratio
    }

    /// Returns the active configuration.
    #[must_use]
    pub const fn config(&self) -> AdaptiveLevelConfig {
        self.config
    }

    fn update_ewma(&mut self, observation: f64) -> f64 {
        // Reject NaN/negative samples to keep the EWMA well-defined; treat
        // them as a "no information" event and reuse the previous value.
        if !observation.is_finite() || observation < 0.0 {
            return self.smoothed_ratio.unwrap_or(observation);
        }
        let alpha = self.config.smoothing_alpha.clamp(f64::EPSILON, 1.0);
        let next = match self.smoothed_ratio {
            Some(prev) => alpha * observation + (1.0 - alpha) * prev,
            None => observation,
        };
        self.smoothed_ratio = Some(next);
        next
    }
}

impl Default for DefaultAdaptiveLevelStrategy {
    fn default() -> Self {
        Self::new(AdaptiveLevelConfig::default())
    }
}

impl AdaptiveLevelStrategy for DefaultAdaptiveLevelStrategy {
    fn recommend_level(&mut self, observed_ratio: f64, current_level: i32) -> i32 {
        let smoothed = self.update_ewma(observed_ratio);
        let next = if smoothed >= self.config.poor_ratio_threshold {
            current_level.saturating_sub(1)
        } else if smoothed <= self.config.good_ratio_threshold {
            current_level.saturating_add(1)
        } else {
            current_level
        };
        self.config.bounds.clamp(next)
    }

    fn reset(&mut self) {
        self.smoothed_ratio = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx_eq(a: f64, b: f64, eps: f64) -> bool {
        (a - b).abs() <= eps
    }

    #[test]
    fn level_bounds_orders_arguments() {
        let b = LevelBounds::new(9, 1);
        assert_eq!(b.min, 1);
        assert_eq!(b.max, 9);
    }

    #[test]
    fn level_bounds_clamps_within_range() {
        let b = LevelBounds::new(1, 9);
        assert_eq!(b.clamp(0), 1);
        assert_eq!(b.clamp(5), 5);
        assert_eq!(b.clamp(99), 9);
    }

    #[test]
    fn first_observation_seeds_ewma() {
        let mut s = DefaultAdaptiveLevelStrategy::for_zlib();
        let _ = s.recommend_level(0.7, 6);
        assert!(approx_eq(s.smoothed_ratio().unwrap(), 0.7, 1e-12));
    }

    #[test]
    fn monotonic_bad_ratio_decreases_level() {
        // Persistently incompressible (~0.99) data should drive the level
        // toward the lower bound.
        let mut s = DefaultAdaptiveLevelStrategy::for_zlib();
        let mut level = 6;
        for _ in 0..50 {
            level = s.recommend_level(0.99, level);
        }
        assert_eq!(level, s.config().bounds.min);
    }

    #[test]
    fn monotonic_good_ratio_increases_level() {
        // Persistently highly compressible (~0.2) data should drive the
        // level toward the upper bound.
        let mut s = DefaultAdaptiveLevelStrategy::for_zlib();
        let mut level = 1;
        for _ in 0..50 {
            level = s.recommend_level(0.2, level);
        }
        assert_eq!(level, s.config().bounds.max);
    }

    #[test]
    fn neutral_ratio_holds_level() {
        let mut s = DefaultAdaptiveLevelStrategy::for_zlib();
        // 0.7 sits between good (0.5) and poor (0.95) thresholds.
        let level = s.recommend_level(0.7, 5);
        assert_eq!(level, 5);
    }

    #[test]
    fn ewma_smooths_outlier() {
        // A single bad block after many good blocks should not yet flip
        // the smoothed ratio above the poor threshold, so the level stays
        // unchanged or keeps moving up.
        let mut s = DefaultAdaptiveLevelStrategy::for_zlib();
        let mut level = 5;
        for _ in 0..20 {
            level = s.recommend_level(0.2, level);
        }
        assert_eq!(level, s.config().bounds.max);
        let pre_outlier = s.smoothed_ratio().unwrap();
        let next_level = s.recommend_level(0.99, level);
        let post_outlier = s.smoothed_ratio().unwrap();
        // EWMA must move toward the outlier but not jump straight to it.
        assert!(post_outlier > pre_outlier);
        assert!(post_outlier < 0.99);
        // Level should not crash through the bounds from one outlier.
        assert!(next_level >= s.config().bounds.min);
        assert!(next_level <= s.config().bounds.max);
    }

    #[test]
    fn reset_clears_history() {
        let mut s = DefaultAdaptiveLevelStrategy::for_zlib();
        let _ = s.recommend_level(0.99, 6);
        assert!(s.smoothed_ratio().is_some());
        s.reset();
        assert!(s.smoothed_ratio().is_none());
    }

    #[test]
    fn rejects_non_finite_observation() {
        let mut s = DefaultAdaptiveLevelStrategy::for_zlib();
        let _ = s.recommend_level(0.5, 6);
        let prior = s.smoothed_ratio();
        let _ = s.recommend_level(f64::NAN, 6);
        assert_eq!(s.smoothed_ratio(), prior);
        let _ = s.recommend_level(-1.0, 6);
        assert_eq!(s.smoothed_ratio(), prior);
    }

    #[test]
    fn for_zstd_uses_wider_bounds() {
        let s = DefaultAdaptiveLevelStrategy::for_zstd();
        assert_eq!(s.config().bounds, LevelBounds::new(1, 19));
    }

    #[test]
    fn alpha_is_clamped_to_unit_interval() {
        // An out-of-range alpha must not panic or produce NaN.
        let mut s = DefaultAdaptiveLevelStrategy::new(AdaptiveLevelConfig {
            smoothing_alpha: 5.0,
            ..AdaptiveLevelConfig::ZLIB_DEFAULT
        });
        let _ = s.recommend_level(0.3, 4);
        let _ = s.recommend_level(0.4, 4);
        assert!(s.smoothed_ratio().unwrap().is_finite());
    }
}
