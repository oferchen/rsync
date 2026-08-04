//! Per-stage throughput aggregation for the governor's Sense phase.
//!
//! [`StageAggregator`] folds the stream of [`StageSample`]s drained from the
//! telemetry bus into one exponentially-weighted moving average per
//! [`Constraint`] stage, using the shared [`Ewma`] primitive so the governor
//! and the adaptive basis dispatcher smooth identically. In this inert
//! (G2) step the aggregator is the entire "control loop": it computes a
//! bytes/second estimate per stage and takes no further action.

use fast_io::ewma::Ewma;

use super::sample::{Constraint, StageSample};

/// Smoothing factor for per-stage throughput averages.
///
/// Reuses the α the adaptive basis dispatcher was tuned with
/// (`fast_io::adaptive_dispatch`, α = 0.2): a fresh sample contributes 20% and
/// history 80%, which rejects per-file jitter while still tracking a genuine
/// stage slowdown within a handful of windows. Sharing the constant keeps the
/// two EWMA consumers from drifting apart.
pub const STAGE_ALPHA: f64 = 0.2;

/// Folds telemetry samples into a smoothed bytes/second estimate per stage.
///
/// One [`Ewma`] per [`Constraint`], indexed by [`Constraint::index`]. A sample
/// whose rate is undefined (zero duration) is ignored rather than poisoning the
/// average. The aggregator is single-owner - the governor thread holds it - so
/// it needs no interior synchronization.
#[derive(Debug, Clone)]
pub struct StageAggregator {
    stages: [Ewma; Constraint::COUNT],
}

impl StageAggregator {
    /// Creates an aggregator with every stage unseeded (no samples yet).
    #[must_use]
    pub fn new() -> Self {
        Self {
            stages: [Ewma::new(STAGE_ALPHA); Constraint::COUNT],
        }
    }

    /// Folds one sample into its stage's average and returns the updated
    /// bytes/second estimate, or `None` when the sample carried no usable rate
    /// (zero duration) and was skipped.
    pub fn fold(&mut self, sample: &StageSample) -> Option<f64> {
        let rate = sample.bytes_per_sec()?;
        Some(self.stages[sample.stage.index()].update(rate))
    }

    /// Current smoothed throughput for `stage`, or `None` before any sample
    /// with a usable rate has been folded in for it.
    #[must_use]
    pub fn throughput(&self, stage: Constraint) -> Option<f64> {
        self.stages[stage.index()].value()
    }
}

impl Default for StageAggregator {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn sample(stage: Constraint, bytes: u64, millis: u64) -> StageSample {
        StageSample::new(stage, bytes, Duration::from_millis(millis), 0)
    }

    #[test]
    fn first_sample_seeds_stage_rate_exactly() {
        let mut agg = StageAggregator::new();
        // 1000 bytes in 1 ms = 1_000_000 bytes/s.
        let out = agg.fold(&sample(Constraint::Read, 1_000, 1)).expect("rate");
        assert!((out - 1_000_000.0).abs() < 1e-3, "seed rate {out}");
        assert_eq!(agg.throughput(Constraint::Read), Some(out));
    }

    #[test]
    fn second_sample_blends_per_alpha() {
        let mut agg = StageAggregator::new();
        agg.fold(&sample(Constraint::Compute, 1_000, 1)); // 1_000_000 B/s
        let out = agg
            .fold(&sample(Constraint::Compute, 2_000, 1)) // 2_000_000 B/s
            .expect("rate");
        let expected = STAGE_ALPHA * 2_000_000.0 + (1.0 - STAGE_ALPHA) * 1_000_000.0;
        assert!(
            (out - expected).abs() < 1e-3,
            "blended {out} want {expected}"
        );
    }

    #[test]
    fn stages_are_independent() {
        let mut agg = StageAggregator::new();
        agg.fold(&sample(Constraint::Read, 1_000, 1));
        agg.fold(&sample(Constraint::WireWrite, 500, 1));
        assert_eq!(agg.throughput(Constraint::Read), Some(1_000_000.0));
        assert_eq!(agg.throughput(Constraint::WireWrite), Some(500_000.0));
        assert_eq!(agg.throughput(Constraint::Compute), None);
    }

    #[test]
    fn zero_duration_sample_is_skipped() {
        let mut agg = StageAggregator::new();
        assert_eq!(agg.fold(&sample(Constraint::Read, 1_000, 0)), None);
        assert_eq!(
            agg.throughput(Constraint::Read),
            None,
            "skipped sample must not seed the average"
        );
    }

    #[test]
    fn converges_toward_sustained_rate() {
        let mut agg = StageAggregator::new();
        // Seed high, then feed a sustained lower rate; the average must track down.
        agg.fold(&sample(Constraint::Compute, 10_000, 1)); // 10 MB/s
        for _ in 0..100 {
            agg.fold(&sample(Constraint::Compute, 1_000, 1)); // 1 MB/s
        }
        let v = agg.throughput(Constraint::Compute).expect("seeded");
        assert!((v - 1_000_000.0).abs() < 1.0, "converged to {v}");
    }
}
