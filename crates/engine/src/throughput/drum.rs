//! Drum identification with hysteresis for the throughput governor.
//!
//! The "drum" in a Theory-of-Constraints drum-buffer-rope pipeline is the
//! single stage that paces the whole system - the constraint. This module turns
//! a snapshot of per-stage telemetry into a drum designation and debounces it so
//! transient noise cannot make the designation flap.
//!
//! # Constraint signature
//!
//! A stage is the constraint when work piles up at *its* input while its
//! downstream neighbour stays starved: the stage cannot consume its backlog fast
//! enough, yet nothing downstream is holding it back. Formally, [`classify`]
//! nominates the stage that
//!
//! - has a throughput estimate (an unsampled stage cannot be the drum), and
//! - shows **high input** occupancy ([`HIGH_OCCUPANCY`] or more), and
//! - shows **low output** occupancy ([`LOW_OCCUPANCY`] or less),
//!
//! breaking ties by the **lowest sustained bytes/second** - the slowest such
//! stage is the true pace-setter. When no stage matches the signature the
//! candidate is [`Constraint::Unknown`]: the pipeline is balanced or
//! under-observed and there is no drum to protect.
//!
//! # Hysteresis
//!
//! Occupancy and rate estimates jitter window to window. If the governor re-sized
//! buffers every time the instantaneous candidate changed it would chase noise
//! and oscillate. [`DrumIdentifier`] therefore commits a new drum only after
//! [`HYSTERESIS_WINDOWS`] consecutive evaluation windows agree on the same
//! challenger; a single window that re-affirms the incumbent clears any pending
//! challenge. This is the State pattern element of the governor: an explicit,
//! debounced transition between drum designations.

use super::sample::Constraint;

/// Input-queue occupancy fraction at or above which a stage's input is "full".
///
/// Occupancy is normalized to `[0.0, 1.0]` (see
/// [`StageAggregator::stage_signals`](super::aggregate::StageAggregator::stage_signals)),
/// so this is three-quarters full. Chosen well above the midpoint so a stage is
/// only flagged as backlogged when its input is genuinely saturated, not merely
/// busy; paired with [`LOW_OCCUPANCY`] it leaves a wide neutral band that keeps
/// balanced pipelines out of any drum designation.
pub const HIGH_OCCUPANCY: f64 = 0.75;

/// Output-queue occupancy fraction at or below which a stage's output is "empty".
///
/// One-quarter full: the downstream neighbour is drinking from a nearly dry
/// queue, so this stage is not being throttled from below. The gap to
/// [`HIGH_OCCUPANCY`] is the neutral band that suppresses drum flapping when
/// every stage sits near the middle.
pub const LOW_OCCUPANCY: f64 = 0.25;

/// Consecutive agreeing evaluation windows required to commit a drum change.
///
/// At the governor's 250 ms poll window (see
/// [`POLL_INTERVAL`](super::governor::POLL_INTERVAL)) three windows is ~750 ms
/// of sustained agreement - long enough to reject a one- or two-window blip from
/// scheduling jitter or a single slow file, short enough to react to a genuine
/// constraint shift inside a second. It mirrors the seq-matches debounce used
/// elsewhere in the pipeline: require a run, not a single sample, before acting.
pub const HYSTERESIS_WINDOWS: u8 = 3;

/// One stage's telemetry for a single evaluation window.
///
/// All occupancies are normalized to `[0.0, 1.0]`. `rate` is the smoothed
/// bytes/second estimate, or `None` when the stage has not been sampled yet.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StageSignal {
    /// Smoothed throughput in bytes/second, or `None` if unsampled.
    pub rate: Option<f64>,
    /// Normalized input-queue occupancy in `[0.0, 1.0]`.
    pub input_occupancy: f64,
    /// Normalized output-queue occupancy in `[0.0, 1.0]` (the successor stage's
    /// input occupancy).
    pub output_occupancy: f64,
}

impl StageSignal {
    /// An unsampled, idle stage: no rate, empty queues.
    #[must_use]
    pub const fn unsampled() -> Self {
        Self {
            rate: None,
            input_occupancy: 0.0,
            output_occupancy: 0.0,
        }
    }

    /// Whether this stage matches the constraint signature: sampled, input at or
    /// above [`HIGH_OCCUPANCY`], output at or below [`LOW_OCCUPANCY`].
    #[must_use]
    fn is_constraint_candidate(&self) -> bool {
        self.rate.is_some()
            && self.input_occupancy >= HIGH_OCCUPANCY
            && self.output_occupancy <= LOW_OCCUPANCY
    }
}

/// Per-stage telemetry snapshot for one evaluation window.
///
/// Indexed by [`Constraint::index`]; the [`Constraint::Unknown`] slot is present
/// for a dense array but never classified.
#[derive(Debug, Clone, Copy)]
pub struct StageSignals {
    signals: [StageSignal; Constraint::COUNT],
}

impl Default for StageSignals {
    fn default() -> Self {
        Self::new()
    }
}

impl StageSignals {
    /// Builds a snapshot with every stage unsampled and idle.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            signals: [StageSignal::unsampled(); Constraint::COUNT],
        }
    }

    /// Returns the signal recorded for `stage`.
    #[must_use]
    pub fn get(&self, stage: Constraint) -> StageSignal {
        self.signals[stage.index()]
    }

    /// Overwrites the signal for `stage`.
    pub fn set(&mut self, stage: Constraint, signal: StageSignal) {
        self.signals[stage.index()] = signal;
    }

    /// Sets `stage`'s output occupancy without disturbing its other fields.
    pub fn set_output_occupancy(&mut self, stage: Constraint, output_occupancy: f64) {
        self.signals[stage.index()].output_occupancy = output_occupancy;
    }
}

/// Classifies the instantaneous constraint stage from one window's signals.
///
/// Returns the slowest stage matching the constraint signature (see the
/// `module docs`), or [`Constraint::Unknown`] when none match. Pure and
/// side-effect free so it can be exhaustively truth-tabled; [`DrumIdentifier`]
/// layers hysteresis on top.
#[must_use]
pub fn classify(signals: &StageSignals) -> Constraint {
    let mut drum: Option<(Constraint, f64)> = None;
    for stage in Constraint::REAL {
        let signal = signals.get(stage);
        if !signal.is_constraint_candidate() {
            continue;
        }
        // `is_constraint_candidate` guarantees `rate` is `Some`.
        let rate = signal.rate.unwrap_or(f64::INFINITY);
        match drum {
            Some((_, best)) if rate >= best => {}
            _ => drum = Some((stage, rate)),
        }
    }
    drum.map_or(Constraint::Unknown, |(stage, _)| stage)
}

/// Debounced drum designation: the State element of the governor.
///
/// Feed one [`observe`](Self::observe) per evaluation window; the committed
/// [`current`](Self::current) drum changes only after [`windows`](Self::new)
/// consecutive windows agree on a new challenger.
#[derive(Debug, Clone)]
pub struct DrumIdentifier {
    /// The committed drum designation.
    current: Constraint,
    /// The challenger accumulating consecutive agreeing windows.
    pending: Constraint,
    /// How many consecutive windows `pending` has held.
    streak: u8,
    /// Consecutive agreeing windows required to commit a change.
    windows: u8,
}

impl Default for DrumIdentifier {
    fn default() -> Self {
        Self::new()
    }
}

impl DrumIdentifier {
    /// Creates an identifier seeded at [`Constraint::Unknown`] using the default
    /// [`HYSTERESIS_WINDOWS`] debounce.
    #[must_use]
    pub fn new() -> Self {
        Self::with_windows(HYSTERESIS_WINDOWS)
    }

    /// Creates an identifier with an explicit hysteresis window count.
    ///
    /// # Panics
    ///
    /// Panics if `windows` is zero: a change would then commit with no agreeing
    /// windows, defeating the debounce. One or more is required.
    #[must_use]
    pub fn with_windows(windows: u8) -> Self {
        assert!(windows >= 1, "hysteresis window count must be >= 1");
        Self {
            current: Constraint::Unknown,
            pending: Constraint::Unknown,
            streak: 0,
            windows,
        }
    }

    /// The committed drum designation.
    #[must_use]
    pub fn current(&self) -> Constraint {
        self.current
    }

    /// Folds one window's `signals` into the designation and returns the
    /// committed drum after this window.
    pub fn observe(&mut self, signals: &StageSignals) -> Constraint {
        self.observe_candidate(classify(signals))
    }

    /// Folds one already-classified `candidate` into the designation.
    ///
    /// Split from [`observe`](Self::observe) so the hysteresis state machine can
    /// be tested independently of [`classify`]. A candidate equal to the
    /// incumbent re-affirms it and clears any pending challenge; a different
    /// candidate must repeat for [`windows`](Self::with_windows) consecutive
    /// calls before it is committed.
    pub fn observe_candidate(&mut self, candidate: Constraint) -> Constraint {
        if candidate == self.current {
            // Incumbent re-affirmed: abandon any in-progress challenge.
            self.pending = self.current;
            self.streak = 0;
            return self.current;
        }
        if candidate == self.pending {
            self.streak = self.streak.saturating_add(1);
        } else {
            self.pending = candidate;
            self.streak = 1;
        }
        if self.streak >= self.windows {
            self.current = self.pending;
            self.streak = 0;
        }
        self.current
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a one-stage snapshot for truth-table cases.
    fn signals_with(stage: Constraint, signal: StageSignal) -> StageSignals {
        let mut s = StageSignals::new();
        s.set(stage, signal);
        s
    }

    fn sig(rate: Option<f64>, input: f64, output: f64) -> StageSignal {
        StageSignal {
            rate,
            input_occupancy: input,
            output_occupancy: output,
        }
    }

    // --- classify: constraint-signature truth table --------------------------

    #[test]
    fn classify_full_input_empty_output_is_the_drum() {
        // high input, low output, sampled -> constraint.
        let s = signals_with(Constraint::Compute, sig(Some(1.0e6), 0.9, 0.1));
        assert_eq!(classify(&s), Constraint::Compute);
    }

    #[test]
    fn classify_requires_a_rate_estimate() {
        // Same occupancy signature but unsampled -> not a candidate.
        let s = signals_with(Constraint::Compute, sig(None, 0.9, 0.1));
        assert_eq!(classify(&s), Constraint::Unknown);
    }

    #[test]
    fn classify_high_input_but_high_output_is_a_victim_not_the_drum() {
        // Backed up at input AND output: throttled from downstream, not the
        // constraint itself.
        let s = signals_with(Constraint::WireWrite, sig(Some(1.0e6), 0.9, 0.9));
        assert_eq!(classify(&s), Constraint::Unknown);
    }

    #[test]
    fn classify_low_input_is_never_the_drum() {
        // Nothing piling up at the input: not starved-downstream constraint.
        let s = signals_with(Constraint::Read, sig(Some(1.0e6), 0.1, 0.0));
        assert_eq!(classify(&s), Constraint::Unknown);
    }

    #[test]
    fn classify_threshold_boundaries_are_inclusive() {
        // Exactly at the thresholds must qualify (>= / <=).
        let s = signals_with(
            Constraint::Compress,
            sig(Some(5.0e5), HIGH_OCCUPANCY, LOW_OCCUPANCY),
        );
        assert_eq!(classify(&s), Constraint::Compress);

        // A hair under HIGH input, or a hair over LOW output, disqualifies.
        let under_input = signals_with(
            Constraint::Compress,
            sig(Some(5.0e5), HIGH_OCCUPANCY - 1e-9, LOW_OCCUPANCY),
        );
        assert_eq!(classify(&under_input), Constraint::Unknown);
        let over_output = signals_with(
            Constraint::Compress,
            sig(Some(5.0e5), HIGH_OCCUPANCY, LOW_OCCUPANCY + 1e-9),
        );
        assert_eq!(classify(&over_output), Constraint::Unknown);
    }

    #[test]
    fn classify_breaks_ties_by_slowest_stage() {
        // Two stages match the signature; the slower (lower bytes/s) is the
        // pace-setter.
        let mut s = StageSignals::new();
        s.set(Constraint::Read, sig(Some(9.0e6), 0.8, 0.1));
        s.set(Constraint::WireWrite, sig(Some(1.0e6), 0.8, 0.1));
        assert_eq!(classify(&s), Constraint::WireWrite);

        // Reverse the rates: now Read is the slower one.
        let mut s2 = StageSignals::new();
        s2.set(Constraint::Read, sig(Some(1.0e6), 0.8, 0.1));
        s2.set(Constraint::WireWrite, sig(Some(9.0e6), 0.8, 0.1));
        assert_eq!(classify(&s2), Constraint::Read);
    }

    #[test]
    fn classify_first_match_wins_on_exact_rate_tie() {
        // Equal rates: the earlier pipeline stage (Read before WireWrite) keeps
        // the designation because a strictly-lower rate is required to displace.
        let mut s = StageSignals::new();
        s.set(Constraint::Read, sig(Some(1.0e6), 0.8, 0.1));
        s.set(Constraint::WireWrite, sig(Some(1.0e6), 0.8, 0.1));
        assert_eq!(classify(&s), Constraint::Read);
    }

    #[test]
    fn classify_empty_snapshot_is_unknown() {
        assert_eq!(classify(&StageSignals::new()), Constraint::Unknown);
    }

    #[test]
    fn classify_never_nominates_unknown_slot() {
        // Even a fully-loaded Unknown slot is ignored: REAL excludes it.
        let s = signals_with(Constraint::Unknown, sig(Some(1.0), 1.0, 0.0));
        assert_eq!(classify(&s), Constraint::Unknown);
    }

    // --- hysteresis: debounced transitions -----------------------------------

    #[test]
    fn drum_holds_until_n_windows_agree() {
        let mut drum = DrumIdentifier::with_windows(3);
        assert_eq!(drum.current(), Constraint::Unknown);
        // Two agreeing windows: still no commit.
        assert_eq!(
            drum.observe_candidate(Constraint::Compute),
            Constraint::Unknown
        );
        assert_eq!(
            drum.observe_candidate(Constraint::Compute),
            Constraint::Unknown
        );
        // Third consecutive window commits.
        assert_eq!(
            drum.observe_candidate(Constraint::Compute),
            Constraint::Compute
        );
    }

    #[test]
    fn interrupted_streak_resets_and_does_not_commit() {
        let mut drum = DrumIdentifier::with_windows(3);
        drum.observe_candidate(Constraint::Compute); // streak 1
        drum.observe_candidate(Constraint::Compute); // streak 2
        // A different candidate breaks the run before the 3rd agreeing window.
        drum.observe_candidate(Constraint::WireWrite); // streak 1 for WireWrite
        assert_eq!(drum.current(), Constraint::Unknown);
        // Compute must start over: two more are not enough.
        drum.observe_candidate(Constraint::Compute);
        drum.observe_candidate(Constraint::Compute);
        assert_eq!(drum.current(), Constraint::Unknown);
        drum.observe_candidate(Constraint::Compute);
        assert_eq!(drum.current(), Constraint::Compute);
    }

    #[test]
    fn incumbent_reaffirmation_clears_a_pending_challenger() {
        let mut drum = DrumIdentifier::with_windows(3);
        // Commit Compute first.
        for _ in 0..3 {
            drum.observe_candidate(Constraint::Compute);
        }
        assert_eq!(drum.current(), Constraint::Compute);
        // A challenger appears twice...
        drum.observe_candidate(Constraint::WireWrite);
        drum.observe_candidate(Constraint::WireWrite);
        // ...then the incumbent re-affirms, wiping the challenge.
        drum.observe_candidate(Constraint::Compute);
        assert_eq!(drum.current(), Constraint::Compute);
        // The challenger must restart from scratch.
        drum.observe_candidate(Constraint::WireWrite);
        drum.observe_candidate(Constraint::WireWrite);
        assert_eq!(drum.current(), Constraint::Compute);
        drum.observe_candidate(Constraint::WireWrite);
        assert_eq!(drum.current(), Constraint::WireWrite);
    }

    #[test]
    fn windows_one_commits_immediately() {
        let mut drum = DrumIdentifier::with_windows(1);
        assert_eq!(
            drum.observe_candidate(Constraint::Read),
            Constraint::Read,
            "a single window commits when hysteresis is 1"
        );
    }

    #[test]
    fn observe_folds_classification_through_hysteresis() {
        let mut drum = DrumIdentifier::with_windows(2);
        let s = {
            let mut s = StageSignals::new();
            s.set(Constraint::WireWrite, sig(Some(1.0e6), 0.9, 0.1));
            s
        };
        assert_eq!(drum.observe(&s), Constraint::Unknown, "first window pends");
        assert_eq!(
            drum.observe(&s),
            Constraint::WireWrite,
            "second agreeing window commits"
        );
    }

    #[test]
    fn default_uses_three_window_hysteresis() {
        let mut drum = DrumIdentifier::new();
        for _ in 0..HYSTERESIS_WINDOWS - 1 {
            assert_eq!(
                drum.observe_candidate(Constraint::Compute),
                Constraint::Unknown
            );
        }
        assert_eq!(
            drum.observe_candidate(Constraint::Compute),
            Constraint::Compute
        );
    }

    #[test]
    #[should_panic(expected = "hysteresis window count must be >= 1")]
    fn zero_windows_panics() {
        let _ = DrumIdentifier::with_windows(0);
    }

    #[test]
    fn set_output_occupancy_preserves_other_fields() {
        let mut s = StageSignals::new();
        s.set(Constraint::Read, sig(Some(2.0e6), 0.8, 0.0));
        s.set_output_occupancy(Constraint::Read, 0.4);
        let out = s.get(Constraint::Read);
        assert_eq!(out.rate, Some(2.0e6));
        assert!((out.input_occupancy - 0.8).abs() < 1e-12);
        assert!((out.output_occupancy - 0.4).abs() < 1e-12);
    }
}
