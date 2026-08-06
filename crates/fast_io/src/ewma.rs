//! Shared exponentially-weighted moving average (EWMA) primitive.
//!
//! A single, unit-tested EWMA type with two call sites: the adaptive
//! basis-read dispatcher ([`crate::adaptive_dispatch`]) folds per-backend
//! throughput samples through it today, and the drum-buffer-rope throughput
//! governor's per-stage telemetry will fold stage samples through the same
//! type. One implementation, one smoothing rule, no drift between consumers.
//!
//! # Smoothing rule
//!
//! Each [`Ewma::update`](crate::ewma::Ewma::update) blends the fresh `sample`
//! with the running estimate:
//!
//! ```text
//! value = alpha * sample + (1 - alpha) * value
//! ```
//!
//! The very first sample seeds `value` directly (no prior estimate exists to
//! blend against), so a fresh EWMA converges from the first observation rather
//! than from an arbitrary zero.

/// An exponentially-weighted moving average with a fixed smoothing factor.
///
/// The average is seeded lazily: until the first sample arrives [`Ewma::value`]
/// is `None`. The first sample becomes the value verbatim; every later sample
/// is blended per the module-level smoothing rule.
///
/// The type is deliberately storage-agnostic: it holds the smoothing factor and
/// the current value only. Callers that need lock-free or atomic persistence
/// (e.g. [`crate::adaptive_dispatch`], which stores the value in an `AtomicU64`)
/// reconstruct an `Ewma` from their own storage via [`Ewma::with_seed`], fold a
/// sample, and write the result back.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Ewma {
    /// Smoothing factor in `(0.0, 1.0]`. Larger values track recent samples
    /// more aggressively; smaller values smooth harder over history.
    alpha: f64,
    /// Current estimate, or `None` before the first sample seeds it.
    value: Option<f64>,
}

impl Ewma {
    /// Construct an unseeded EWMA with smoothing factor `alpha`.
    ///
    /// # Panics
    ///
    /// Panics if `alpha` is not a finite value in the half-open interval
    /// `(0.0, 1.0]`. An `alpha` of `0.0` would never incorporate new samples
    /// and an `alpha` above `1.0` would overshoot each sample; both are
    /// programming errors, not runtime conditions.
    #[must_use]
    pub fn new(alpha: f64) -> Self {
        Self::with_seed(alpha, None)
    }

    /// Construct an EWMA with smoothing factor `alpha` and an explicit initial
    /// `value`.
    ///
    /// `value` of `None` yields an unseeded average identical to [`Ewma::new`];
    /// `Some(v)` seeds the average so the next [`Ewma::update`] blends against
    /// `v`. This is the reconstruction entry point for callers that persist the
    /// running estimate outside the type (lock-free atomics, serialized state).
    ///
    /// # Panics
    ///
    /// Panics if `alpha` is not a finite value in `(0.0, 1.0]` (see
    /// [`Ewma::new`]).
    #[must_use]
    pub fn with_seed(alpha: f64, value: Option<f64>) -> Self {
        assert!(
            alpha.is_finite() && alpha > 0.0 && alpha <= 1.0,
            "EWMA alpha must be finite and in (0.0, 1.0], got {alpha}"
        );
        Self { alpha, value }
    }

    /// Fold `sample` into the average and return the updated estimate.
    ///
    /// The first sample seeds the average verbatim; subsequent samples blend
    /// per the module-level smoothing rule. The returned value equals the value
    /// [`Ewma::value`] would report afterwards, so callers persisting the result
    /// need not re-read.
    pub fn update(&mut self, sample: f64) -> f64 {
        let updated = match self.value {
            None => sample,
            Some(prev) => self.alpha * sample + (1.0 - self.alpha) * prev,
        };
        self.value = Some(updated);
        updated
    }

    /// Current estimate, or `None` before the first sample has been folded in.
    #[must_use]
    pub fn value(&self) -> Option<f64> {
        self.value
    }

    /// Smoothing factor this average was constructed with.
    #[must_use]
    pub fn alpha(&self) -> f64 {
        self.alpha
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// A newly constructed EWMA reports no value until seeded.
    #[test]
    fn unseeded_has_no_value() {
        let ewma = Ewma::new(0.2);
        assert_eq!(ewma.value(), None);
        assert!((ewma.alpha() - 0.2).abs() < f64::EPSILON);
    }

    /// The first sample seeds the average verbatim; no blending occurs because
    /// there is no prior estimate.
    #[test]
    fn first_sample_seeds_directly() {
        let mut ewma = Ewma::new(0.2);
        let out = ewma.update(1_000_000.0);
        assert!((out - 1_000_000.0).abs() < 1e-6);
        assert_eq!(ewma.value(), Some(out));
    }

    /// The second sample blends with the seed using exactly the documented
    /// smoothing rule. This pins the arithmetic that adaptive_dispatch relied
    /// on before the extraction so decisions cannot shift.
    #[test]
    fn second_sample_blends_per_alpha() {
        let alpha = 0.2;
        let mut ewma = Ewma::new(alpha);
        ewma.update(1_000_000.0);
        let out = ewma.update(2_000_000.0);
        let expected = alpha * 2_000_000.0 + (1.0 - alpha) * 1_000_000.0;
        assert!((out - expected).abs() < 1e-6);
        assert_eq!(ewma.value(), Some(out));
    }

    /// Repeatedly feeding a constant drives the average to that constant.
    #[test]
    fn converges_to_constant_input() {
        let mut ewma = Ewma::new(0.2);
        for _ in 0..1_000 {
            ewma.update(500.0);
        }
        let v = ewma.value().expect("seeded");
        assert!((v - 500.0).abs() < 1e-6, "converged to {v}, want 500");
    }

    /// A larger alpha tracks a step change faster than a smaller alpha after an
    /// identical number of samples.
    #[test]
    fn larger_alpha_tracks_faster() {
        let mut fast = Ewma::new(0.8);
        let mut slow = Ewma::new(0.1);
        fast.update(0.0);
        slow.update(0.0);
        for _ in 0..5 {
            fast.update(100.0);
            slow.update(100.0);
        }
        assert!(
            fast.value().unwrap() > slow.value().unwrap(),
            "fast={:?} slow={:?}",
            fast.value(),
            slow.value()
        );
    }

    /// `with_seed(alpha, Some(v))` behaves as if `v` had already been folded in:
    /// the next update blends against `v`. This is the reconstruction contract
    /// adaptive_dispatch depends on.
    #[test]
    fn with_seed_reconstructs_running_estimate() {
        let alpha = 0.2;
        let mut ewma = Ewma::with_seed(alpha, Some(1_000_000.0));
        let out = ewma.update(2_000_000.0);
        let expected = alpha * 2_000_000.0 + (1.0 - alpha) * 1_000_000.0;
        assert!((out - expected).abs() < 1e-6);
    }

    /// Alpha of exactly 1.0 is the boundary of the valid interval: every sample
    /// replaces the estimate outright.
    #[test]
    fn alpha_one_tracks_latest_sample() {
        let mut ewma = Ewma::new(1.0);
        ewma.update(10.0);
        assert_eq!(ewma.update(42.0), 42.0);
    }

    #[test]
    #[should_panic(expected = "EWMA alpha")]
    fn alpha_zero_panics() {
        let _ = Ewma::new(0.0);
    }

    #[test]
    #[should_panic(expected = "EWMA alpha")]
    fn alpha_above_one_panics() {
        let _ = Ewma::new(1.000_001);
    }

    #[test]
    #[should_panic(expected = "EWMA alpha")]
    fn alpha_nan_panics() {
        let _ = Ewma::new(f64::NAN);
    }

    proptest! {
        /// For any smoothing factor and any stream of finite samples, the EWMA
        /// output stays within `[min, max]` of the samples seen so far. A
        /// convex blend of values inside an interval cannot leave that interval.
        #[test]
        fn output_within_sample_window(
            alpha in 0.001_f64..=1.0,
            samples in prop::collection::vec(-1.0e9_f64..1.0e9, 1..64),
        ) {
            let mut ewma = Ewma::new(alpha);
            let mut lo = f64::INFINITY;
            let mut hi = f64::NEG_INFINITY;
            for &s in &samples {
                lo = lo.min(s);
                hi = hi.max(s);
                let out = ewma.update(s);
                prop_assert!(out >= lo - 1e-6, "out {out} below min {lo}");
                prop_assert!(out <= hi + 1e-6, "out {out} above max {hi}");
            }
        }
    }
}
