//! Telemetry sample and constraint-stage taxonomy for the throughput governor.
//!
//! A [`StageSample`] is the atom of the Observer half of the drum-buffer-rope
//! (DBR) governor: each instrumented pipeline stage publishes one sample per
//! unit of work it completes, and the governor folds those samples into
//! per-stage throughput estimates. The sample carries only plain data - no
//! locks, no allocation - so publishing it is a single lock-free queue push.

use std::time::Duration;

/// A pipeline stage that can become the throughput constraint (the DBR "drum").
///
/// The governor classifies which stage is the slowest sustained producer of
/// bytes; that stage is the drum. The variants cover every stage the pipeline
/// can bottleneck on, ordered from the source of data (`Read`) to its sink
/// (`Consumer`):
///
/// - `Read` - pulling basis/source bytes off disk or the wire.
/// - `Compute` - rolling/strong checksum and delta application on worker threads.
/// - `Compress` - token compression (sender-side; instrumented when that path
///   is tapped in a later step).
/// - `WireWrite` - emitting reordered results toward the socket or output file.
/// - `Consumer` - the reorder/commit drain that delivers results in NDX order.
/// - `Unknown` - no stage has yet accumulated enough samples to classify.
///
/// `Unknown` is deliberately last and is never emitted by an instrumented
/// stage; it is the governor's initial drum designation before any telemetry
/// arrives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Constraint {
    /// Reading basis or source bytes (disk or wire).
    Read,
    /// Checksum and delta computation on worker threads.
    Compute,
    /// Token compression on the sender path.
    Compress,
    /// Writing reordered output toward the wire or destination file.
    WireWrite,
    /// The reorder/commit drain delivering results in submission order.
    Consumer,
    /// No stage classified yet (initial governor state only).
    Unknown,
}

impl Constraint {
    /// Every constraint variant, in declaration order.
    ///
    /// Used to size per-stage aggregate arrays and to iterate stages in a
    /// stable order. Kept in lockstep with the enum by [`Constraint::index`],
    /// which the aggregate array indexes with.
    pub const ALL: [Constraint; 6] = [
        Constraint::Read,
        Constraint::Compute,
        Constraint::Compress,
        Constraint::WireWrite,
        Constraint::Consumer,
        Constraint::Unknown,
    ];

    /// Every schedulable pipeline stage, in data-flow order, excluding
    /// [`Constraint::Unknown`].
    ///
    /// `Unknown` is the governor's initial, unclassified drum designation and
    /// is never a real pipeline stage, so drum identification iterates this
    /// list rather than [`Constraint::ALL`]. The order matches the physical
    /// pipeline (`Read → Compute → Compress → WireWrite → Consumer`) so
    /// [`Constraint::successor`] can walk it.
    pub const REAL: [Constraint; 5] = [
        Constraint::Read,
        Constraint::Compute,
        Constraint::Compress,
        Constraint::WireWrite,
        Constraint::Consumer,
    ];

    /// Number of distinct constraint stages.
    ///
    /// Equal to `Constraint::ALL.len()`; exposed as a `const` so fixed-size
    /// per-stage arrays can be declared without referencing the array length
    /// at a non-const site.
    pub const COUNT: usize = Self::ALL.len();

    /// The stage immediately downstream of `self` in the transfer pipeline, or
    /// `None` for the terminal [`Constraint::Consumer`] and the non-stage
    /// [`Constraint::Unknown`].
    ///
    /// Drum identification uses this to read a stage's *output* queue occupancy:
    /// the successor's input queue is, by construction, this stage's output.
    /// A stage that is piling work at its own input while its successor's input
    /// stays empty is the classic Theory-of-Constraints bottleneck signature.
    #[must_use]
    pub const fn successor(self) -> Option<Constraint> {
        match self {
            Constraint::Read => Some(Constraint::Compute),
            Constraint::Compute => Some(Constraint::Compress),
            Constraint::Compress => Some(Constraint::WireWrite),
            Constraint::WireWrite => Some(Constraint::Consumer),
            Constraint::Consumer | Constraint::Unknown => None,
        }
    }

    /// Reconstructs a stage from its dense [`index`](Constraint::index).
    ///
    /// Returns [`Constraint::Unknown`] for any value outside `[0, COUNT)`, so a
    /// round-trip through an atomic `u8` slot can never yield an invalid stage.
    #[must_use]
    pub const fn from_index(index: usize) -> Constraint {
        match index {
            0 => Constraint::Read,
            1 => Constraint::Compute,
            2 => Constraint::Compress,
            3 => Constraint::WireWrite,
            4 => Constraint::Consumer,
            _ => Constraint::Unknown,
        }
    }

    /// Dense array index for this stage in `[0, COUNT)`.
    ///
    /// The mapping matches [`Constraint::ALL`] so an array indexed by this
    /// value can be paired back to its stage by `Constraint::ALL[index]`.
    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Constraint::Read => 0,
            Constraint::Compute => 1,
            Constraint::Compress => 2,
            Constraint::WireWrite => 3,
            Constraint::Consumer => 4,
            Constraint::Unknown => 5,
        }
    }
}

/// A single throughput observation published by one pipeline stage.
///
/// The governor consumes a stream of these and folds each into the owning
/// stage's exponentially-weighted moving average. All fields are plain values
/// so a sample can be pushed onto the lock-free telemetry bus without
/// allocating or blocking the data path.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StageSample {
    /// The stage that produced this observation.
    pub stage: Constraint,
    /// Bytes of useful work completed in this observation window.
    pub bytes: u64,
    /// Wall-clock time the stage spent producing `bytes`.
    pub duration: Duration,
    /// Depth of the stage's input queue at sampling time.
    ///
    /// A high input occupancy paired with a low downstream occupancy is the
    /// classic constraint signature the governor looks for. For stages without
    /// an explicit queue (e.g. per-file compute) this is `0`.
    pub queue_occupancy: usize,
}

impl StageSample {
    /// Constructs a sample for `stage` describing `bytes` produced over
    /// `duration` with the given input `queue_occupancy`.
    #[must_use]
    pub const fn new(
        stage: Constraint,
        bytes: u64,
        duration: Duration,
        queue_occupancy: usize,
    ) -> Self {
        Self {
            stage,
            bytes,
            duration,
            queue_occupancy,
        }
    }

    /// Instantaneous throughput in bytes per second for this observation.
    ///
    /// Returns `None` when `duration` is zero (no elapsed time yields no
    /// meaningful rate) so callers never divide by zero. A zero-byte sample
    /// over a non-zero duration returns `Some(0.0)`, which correctly drags a
    /// stage's average toward a stall.
    #[must_use]
    pub fn bytes_per_sec(&self) -> Option<f64> {
        let secs = self.duration.as_secs_f64();
        if secs > 0.0 {
            Some(self.bytes as f64 / secs)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_is_dense_and_bijective_with_all() {
        for (i, stage) in Constraint::ALL.iter().enumerate() {
            assert_eq!(stage.index(), i, "index must match ALL position");
        }
        assert_eq!(Constraint::COUNT, 6);
    }

    #[test]
    fn real_stages_exclude_unknown_and_stay_in_flow_order() {
        assert_eq!(Constraint::REAL.len(), Constraint::COUNT - 1);
        assert!(!Constraint::REAL.contains(&Constraint::Unknown));
        // REAL is a prefix of ALL (declaration/data-flow order).
        for (i, stage) in Constraint::REAL.iter().enumerate() {
            assert_eq!(*stage, Constraint::ALL[i]);
        }
    }

    #[test]
    fn successor_walks_the_pipeline_to_a_terminal() {
        assert_eq!(Constraint::Read.successor(), Some(Constraint::Compute));
        assert_eq!(Constraint::Compute.successor(), Some(Constraint::Compress));
        assert_eq!(
            Constraint::Compress.successor(),
            Some(Constraint::WireWrite)
        );
        assert_eq!(
            Constraint::WireWrite.successor(),
            Some(Constraint::Consumer)
        );
        assert_eq!(Constraint::Consumer.successor(), None);
        assert_eq!(Constraint::Unknown.successor(), None);
    }

    #[test]
    fn from_index_round_trips_and_saturates_to_unknown() {
        for stage in Constraint::ALL {
            assert_eq!(Constraint::from_index(stage.index()), stage);
        }
        // Out-of-range indices collapse to Unknown rather than panic.
        assert_eq!(
            Constraint::from_index(Constraint::COUNT),
            Constraint::Unknown
        );
        assert_eq!(Constraint::from_index(usize::MAX), Constraint::Unknown);
    }

    #[test]
    fn bytes_per_sec_computes_rate() {
        let s = StageSample::new(Constraint::Read, 1_000, Duration::from_millis(500), 0);
        let rate = s.bytes_per_sec().expect("non-zero duration");
        assert!((rate - 2_000.0).abs() < 1e-6, "got {rate}");
    }

    #[test]
    fn bytes_per_sec_zero_duration_is_none() {
        let s = StageSample::new(Constraint::Compute, 1_000, Duration::ZERO, 4);
        assert_eq!(s.bytes_per_sec(), None);
    }

    #[test]
    fn zero_bytes_is_a_stall_not_none() {
        let s = StageSample::new(Constraint::WireWrite, 0, Duration::from_secs(1), 0);
        assert_eq!(s.bytes_per_sec(), Some(0.0));
    }
}
