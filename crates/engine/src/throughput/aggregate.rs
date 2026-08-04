//! Per-stage throughput aggregation for the governor's Sense phase.
//!
//! [`StageAggregator`] folds the stream of [`StageSample`]s drained from the
//! telemetry bus into one exponentially-weighted moving average per
//! [`Constraint`] stage, using the shared [`Ewma`] primitive so the governor
//! and the adaptive basis dispatcher smooth identically. In this inert
//! (G2) step the aggregator is the entire "control loop": it computes a
//! bytes/second estimate per stage and takes no further action.

use fast_io::ewma::Ewma;

use super::drum::{StageSignal, StageSignals};
use super::sample::{Constraint, StageSample};

/// Smoothing factor for per-stage throughput averages.
///
/// Reuses the α the adaptive basis dispatcher was tuned with
/// (`fast_io::adaptive_dispatch`, α = 0.2): a fresh sample contributes 20% and
/// history 80%, which rejects per-file jitter while still tracking a genuine
/// stage slowdown within a handful of windows. Sharing the constant keeps the
/// two EWMA consumers from drifting apart.
pub const STAGE_ALPHA: f64 = 0.2;

/// Folds telemetry samples into a smoothed bytes/second estimate per stage and
/// tracks each stage's input-queue occupancy for drum identification.
///
/// One [`Ewma`] per [`Constraint`], indexed by [`Constraint::index`]. A sample
/// whose rate is undefined (zero duration) is ignored rather than poisoning the
/// average, but its occupancy is still recorded. The aggregator is single-owner
/// - the governor thread holds it - so it needs no interior synchronization.
///
/// Occupancy arrives as an absolute queue depth, but stages have different
/// capacities, so the aggregator normalizes each stage against its own observed
/// high-water mark. This self-calibrates without any external capacity hint: a
/// stage whose input sits at its highest-ever depth reads `1.0`; an empty one
/// reads `0.0`. Drum identification only compares a stage's normalized input
/// against its normalized output, so the shared, self-relative scale is exactly
/// what the constraint signature needs.
#[derive(Debug, Clone)]
pub struct StageAggregator {
    stages: [Ewma; Constraint::COUNT],
    /// Most recent input-queue occupancy observed for each stage.
    occupancy: [usize; Constraint::COUNT],
    /// Highest input-queue occupancy ever observed for each stage, the
    /// normalization denominator.
    occupancy_high_water: [usize; Constraint::COUNT],
}

impl StageAggregator {
    /// Creates an aggregator with every stage unseeded (no samples yet).
    #[must_use]
    pub fn new() -> Self {
        Self {
            stages: [Ewma::new(STAGE_ALPHA); Constraint::COUNT],
            occupancy: [0; Constraint::COUNT],
            occupancy_high_water: [0; Constraint::COUNT],
        }
    }

    /// Folds one sample into its stage's average and returns the updated
    /// bytes/second estimate, or `None` when the sample carried no usable rate
    /// (zero duration) and was skipped.
    ///
    /// The sample's input-queue occupancy is recorded regardless of whether the
    /// rate was usable, so a zero-duration sample still updates the drum signal.
    pub fn fold(&mut self, sample: &StageSample) -> Option<f64> {
        let idx = sample.stage.index();
        self.occupancy[idx] = sample.queue_occupancy;
        if sample.queue_occupancy > self.occupancy_high_water[idx] {
            self.occupancy_high_water[idx] = sample.queue_occupancy;
        }
        let rate = sample.bytes_per_sec()?;
        Some(self.stages[idx].update(rate))
    }

    /// Current smoothed throughput for `stage`, or `None` before any sample
    /// with a usable rate has been folded in for it.
    #[must_use]
    pub fn throughput(&self, stage: Constraint) -> Option<f64> {
        self.stages[stage.index()].value()
    }

    /// Normalized input-queue occupancy for `stage` in `[0.0, 1.0]`.
    ///
    /// The latest observed depth divided by the stage's high-water mark; `0.0`
    /// before any occupancy has been seen (high-water still zero).
    #[must_use]
    pub fn input_occupancy(&self, stage: Constraint) -> f64 {
        let idx = stage.index();
        let high = self.occupancy_high_water[idx];
        if high == 0 {
            0.0
        } else {
            self.occupancy[idx] as f64 / high as f64
        }
    }

    /// Builds the per-stage [`StageSignals`] snapshot the drum identifier reads.
    ///
    /// Each real stage's signal carries its smoothed rate and normalized input
    /// occupancy; a second pass fills each stage's output occupancy from its
    /// [`successor`](Constraint::successor)'s input occupancy, so a stage backed
    /// up at its own input while its downstream neighbour is empty reads as the
    /// classic constraint.
    #[must_use]
    pub fn stage_signals(&self) -> StageSignals {
        let mut signals = StageSignals::new();
        for stage in Constraint::REAL {
            signals.set(
                stage,
                StageSignal {
                    rate: self.throughput(stage),
                    input_occupancy: self.input_occupancy(stage),
                    output_occupancy: 0.0,
                },
            );
        }
        for stage in Constraint::REAL {
            if let Some(successor) = stage.successor() {
                let downstream_input = signals.get(successor).input_occupancy;
                signals.set_output_occupancy(stage, downstream_input);
            }
        }
        signals
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

    fn sample_occ(stage: Constraint, bytes: u64, millis: u64, occ: usize) -> StageSample {
        StageSample::new(stage, bytes, Duration::from_millis(millis), occ)
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
    fn occupancy_normalizes_against_stage_high_water() {
        let mut agg = StageAggregator::new();
        assert_eq!(agg.input_occupancy(Constraint::WireWrite), 0.0);
        // First occupancy seeds the high-water mark: reads full.
        agg.fold(&sample_occ(Constraint::WireWrite, 1_000, 1, 8));
        assert!((agg.input_occupancy(Constraint::WireWrite) - 1.0).abs() < 1e-12);
        // A lower depth reads as a fraction of the high-water mark.
        agg.fold(&sample_occ(Constraint::WireWrite, 1_000, 1, 2));
        assert!((agg.input_occupancy(Constraint::WireWrite) - 0.25).abs() < 1e-12);
    }

    #[test]
    fn zero_duration_sample_still_records_occupancy() {
        let mut agg = StageAggregator::new();
        // Rate is skipped (zero duration) but occupancy must still register.
        assert_eq!(
            agg.fold(&sample_occ(Constraint::WireWrite, 1_000, 0, 4)),
            None
        );
        assert!((agg.input_occupancy(Constraint::WireWrite) - 1.0).abs() < 1e-12);
    }

    #[test]
    fn stage_signals_fills_output_from_successor_input() {
        let mut agg = StageAggregator::new();
        // Compute busy at input, WireWrite (its successor) idle at input.
        agg.fold(&sample_occ(Constraint::Compute, 2_000, 1, 10));
        agg.fold(&sample_occ(Constraint::WireWrite, 2_000, 1, 10)); // seed WW high-water
        agg.fold(&sample_occ(Constraint::WireWrite, 2_000, 1, 0)); // now empty
        let signals = agg.stage_signals();
        let compute = signals.get(Constraint::Compute);
        assert!(compute.rate.is_some());
        assert!((compute.input_occupancy - 1.0).abs() < 1e-12);
        // Compute's output occupancy mirrors WireWrite's (empty) input.
        assert!((compute.output_occupancy - 0.0).abs() < 1e-12);
        // The terminal Consumer stage has no successor: output stays 0.
        assert_eq!(signals.get(Constraint::Consumer).output_occupancy, 0.0);
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
