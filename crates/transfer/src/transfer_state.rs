//! Typed state machine for the transfer pipeline lifecycle.
//!
//! Models the linear progression of a transfer through its protocol phases:
//!
//! ```text
//! Handshake -> FilterExchange -> FileListTransfer -> DeltaTransfer -> Finalization -> Complete
//! ```
//!
//! Each state transition is validated at runtime. Invalid transitions (backward,
//! self, or out-of-order) return [`InvalidTransition`]. The state machine is
//! driven by the transfer orchestration thread and is not `Sync`.
//!
//! # Upstream Reference
//!
//! The phase sequence mirrors upstream rsync's `main.c:start_server()` and
//! `do_recv()` / `do_send()` orchestration, where the protocol proceeds
//! through handshake, filter exchange, file list transfer, delta transfer,
//! phase-done exchange, and goodbye in strict order.
//!
//! # Usage
//!
//! ```
//! use transfer::transfer_state::{TransferPipeline, TransferPhase};
//! use transfer::role::ServerRole;
//!
//! let mut pipeline = TransferPipeline::new(ServerRole::Receiver);
//! assert_eq!(pipeline.phase(), TransferPhase::Handshake);
//!
//! pipeline.advance_to(TransferPhase::FilterExchange).unwrap();
//! assert_eq!(pipeline.phase(), TransferPhase::FilterExchange);
//! ```

use thiserror::Error;

use crate::role::ServerRole;

/// Protocol phases in the transfer pipeline lifecycle.
///
/// Phases progress in a strict linear order. Each phase corresponds to a
/// distinct protocol operation documented in the upstream rsync source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TransferPhase {
    /// Binary or legacy ASCII protocol version exchange.
    ///
    /// Both sides advertise supported versions and negotiate a common one.
    /// (upstream: `compat.c:602-604`)
    Handshake,

    /// Filter and exclude list exchange.
    ///
    /// Client-mode or server-mode writes/reads the filter rule list over
    /// the wire. Multiplex I/O is activated before this phase.
    /// (upstream: `exclude.c:recv_filter_list()`, `main.c:1276`)
    FilterExchange,

    /// File list build, transmission, or reception.
    ///
    /// The generator walks the filesystem and sends the file list; the
    /// receiver reads and sanitizes it. INC_RECURSE sends the list
    /// incrementally as per-directory segments.
    /// (upstream: `flist.c:send_file_list()`, `flist.c:recv_file_list()`)
    FileListTransfer,

    /// Per-file delta transfer loop.
    ///
    /// The receiver generates signatures from basis files; the generator
    /// computes and sends deltas. Phase 1 uses short checksums, phase 2
    /// redo uses full-length checksums for collision resistance.
    /// (upstream: `receiver.c:recv_files()`, `sender.c:send_files()`)
    DeltaTransfer,

    /// Phase-done exchange, statistics, and goodbye handshake.
    ///
    /// NDX_DONE messages mark phase boundaries. Statistics are exchanged
    /// in client mode. Extended goodbye (protocol >= 32) adds echo rounds.
    /// (upstream: `main.c:read_final_goodbye()`, `main.c:handle_stats()`)
    Finalization,

    /// Terminal state - transfer completed successfully.
    ///
    /// No further transitions are valid from this state.
    Complete,
}

impl TransferPhase {
    /// Returns the numeric ordering index of this phase.
    ///
    /// Used internally for transition validation. Lower values precede
    /// higher values in the lifecycle.
    #[must_use]
    const fn ordinal(self) -> u8 {
        match self {
            Self::Handshake => 0,
            Self::FilterExchange => 1,
            Self::FileListTransfer => 2,
            Self::DeltaTransfer => 3,
            Self::Finalization => 4,
            Self::Complete => 5,
        }
    }

    /// Returns the next phase in the lifecycle, or `None` if this is
    /// the terminal [`Complete`](Self::Complete) state.
    #[must_use]
    pub const fn next(self) -> Option<Self> {
        match self {
            Self::Handshake => Some(Self::FilterExchange),
            Self::FilterExchange => Some(Self::FileListTransfer),
            Self::FileListTransfer => Some(Self::DeltaTransfer),
            Self::DeltaTransfer => Some(Self::Finalization),
            Self::Finalization => Some(Self::Complete),
            Self::Complete => None,
        }
    }

    /// Returns `true` if this is the terminal state.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Complete)
    }

    /// Returns a human-readable label for this phase.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Handshake => "handshake",
            Self::FilterExchange => "filter-exchange",
            Self::FileListTransfer => "file-list-transfer",
            Self::DeltaTransfer => "delta-transfer",
            Self::Finalization => "finalization",
            Self::Complete => "complete",
        }
    }
}

impl std::fmt::Display for TransferPhase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

/// Error returned when an invalid state transition is attempted.
///
/// This indicates a logic error in the caller - the transfer pipeline
/// only supports forward transitions through the linear phase sequence.
#[derive(Debug, Clone, Error)]
#[error(
    "invalid transfer state transition: cannot move from {current} to {target} \
     (transitions must be forward-only through the phase sequence)"
)]
pub struct InvalidTransition {
    /// The phase the pipeline was in when the transition was attempted.
    pub current: TransferPhase,
    /// The phase the caller tried to transition to.
    pub target: TransferPhase,
}

/// Typed state machine tracking the transfer pipeline lifecycle.
///
/// Enforces the linear phase progression:
/// `Handshake -> FilterExchange -> FileListTransfer -> DeltaTransfer -> Finalization -> Complete`
///
/// Created with [`TransferPipeline::new`] in the [`Handshake`](TransferPhase::Handshake)
/// state. Transitions are validated by [`advance`](Self::advance) (next phase) or
/// [`advance_to`](Self::advance_to) (explicit target).
///
/// # Role
///
/// The pipeline carries a [`ServerRole`] for diagnostic context. Both Generator
/// and Receiver roles traverse the same state sequence - the role does not
/// affect which transitions are valid.
#[derive(Debug)]
pub struct TransferPipeline {
    phase: TransferPhase,
    role: ServerRole,
}

impl TransferPipeline {
    /// Creates a new pipeline in the [`Handshake`](TransferPhase::Handshake) state.
    #[must_use]
    pub const fn new(role: ServerRole) -> Self {
        Self {
            phase: TransferPhase::Handshake,
            role,
        }
    }

    /// Returns the current phase.
    #[must_use]
    pub const fn phase(&self) -> TransferPhase {
        self.phase
    }

    /// Returns the role associated with this pipeline.
    #[must_use]
    pub const fn role(&self) -> ServerRole {
        self.role
    }

    /// Returns `true` if the pipeline has reached the terminal state.
    #[must_use]
    pub const fn is_complete(&self) -> bool {
        self.phase.is_terminal()
    }

    /// Advances to the next phase in the lifecycle.
    ///
    /// Returns the new phase on success. Returns [`InvalidTransition`] if the
    /// pipeline is already in the terminal [`Complete`](TransferPhase::Complete)
    /// state.
    ///
    /// # Errors
    ///
    /// Returns `InvalidTransition` when called on a completed pipeline.
    pub fn advance(&mut self) -> Result<TransferPhase, InvalidTransition> {
        match self.phase.next() {
            Some(next) => {
                self.phase = next;
                Ok(next)
            }
            None => Err(InvalidTransition {
                current: self.phase,
                target: self.phase,
            }),
        }
    }

    /// Advances to a specific target phase.
    ///
    /// The target must be exactly one step ahead of the current phase. Use
    /// [`advance_through`](Self::advance_through) to skip multiple phases at once.
    ///
    /// # Errors
    ///
    /// Returns `InvalidTransition` if:
    /// - `target` is the same as the current phase (no self-transitions)
    /// - `target` precedes the current phase (no backward transitions)
    /// - `target` is more than one step ahead (use `advance_through` instead)
    pub fn advance_to(&mut self, target: TransferPhase) -> Result<(), InvalidTransition> {
        if target.ordinal() != self.phase.ordinal() + 1 {
            return Err(InvalidTransition {
                current: self.phase,
                target,
            });
        }
        self.phase = target;
        Ok(())
    }

    /// Advances through all phases up to and including `target`.
    ///
    /// Permits multi-step jumps by iterating through intermediate phases.
    /// The target must be strictly ahead of the current phase.
    ///
    /// # Errors
    ///
    /// Returns `InvalidTransition` if `target` is at or before the current phase.
    pub fn advance_through(&mut self, target: TransferPhase) -> Result<(), InvalidTransition> {
        if target.ordinal() <= self.phase.ordinal() {
            return Err(InvalidTransition {
                current: self.phase,
                target,
            });
        }
        while self.phase != target {
            self.advance()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phase_ordinals_are_monotonically_increasing() {
        let phases = [
            TransferPhase::Handshake,
            TransferPhase::FilterExchange,
            TransferPhase::FileListTransfer,
            TransferPhase::DeltaTransfer,
            TransferPhase::Finalization,
            TransferPhase::Complete,
        ];
        for window in phases.windows(2) {
            assert!(
                window[0].ordinal() < window[1].ordinal(),
                "{} should precede {}",
                window[0],
                window[1]
            );
        }
    }

    #[test]
    fn phase_next_follows_linear_sequence() {
        assert_eq!(
            TransferPhase::Handshake.next(),
            Some(TransferPhase::FilterExchange)
        );
        assert_eq!(
            TransferPhase::FilterExchange.next(),
            Some(TransferPhase::FileListTransfer)
        );
        assert_eq!(
            TransferPhase::FileListTransfer.next(),
            Some(TransferPhase::DeltaTransfer)
        );
        assert_eq!(
            TransferPhase::DeltaTransfer.next(),
            Some(TransferPhase::Finalization)
        );
        assert_eq!(
            TransferPhase::Finalization.next(),
            Some(TransferPhase::Complete)
        );
        assert_eq!(TransferPhase::Complete.next(), None);
    }

    #[test]
    fn only_complete_is_terminal() {
        assert!(!TransferPhase::Handshake.is_terminal());
        assert!(!TransferPhase::FilterExchange.is_terminal());
        assert!(!TransferPhase::FileListTransfer.is_terminal());
        assert!(!TransferPhase::DeltaTransfer.is_terminal());
        assert!(!TransferPhase::Finalization.is_terminal());
        assert!(TransferPhase::Complete.is_terminal());
    }

    #[test]
    fn phase_labels_are_non_empty() {
        let phases = [
            TransferPhase::Handshake,
            TransferPhase::FilterExchange,
            TransferPhase::FileListTransfer,
            TransferPhase::DeltaTransfer,
            TransferPhase::Finalization,
            TransferPhase::Complete,
        ];
        for phase in phases {
            assert!(!phase.label().is_empty(), "{phase:?} has empty label");
        }
    }

    #[test]
    fn phase_display_matches_label() {
        let phases = [
            TransferPhase::Handshake,
            TransferPhase::FilterExchange,
            TransferPhase::FileListTransfer,
            TransferPhase::DeltaTransfer,
            TransferPhase::Finalization,
            TransferPhase::Complete,
        ];
        for phase in phases {
            assert_eq!(format!("{phase}"), phase.label());
        }
    }

    #[test]
    fn new_pipeline_starts_at_handshake() {
        let pipeline = TransferPipeline::new(ServerRole::Receiver);
        assert_eq!(pipeline.phase(), TransferPhase::Handshake);
        assert!(!pipeline.is_complete());
    }

    #[test]
    fn pipeline_preserves_role() {
        let recv = TransferPipeline::new(ServerRole::Receiver);
        assert_eq!(recv.role(), ServerRole::Receiver);

        let generator = TransferPipeline::new(ServerRole::Generator);
        assert_eq!(generator.role(), ServerRole::Generator);
    }

    #[test]
    fn advance_walks_through_all_phases() {
        let mut pipeline = TransferPipeline::new(ServerRole::Receiver);
        let expected = [
            TransferPhase::FilterExchange,
            TransferPhase::FileListTransfer,
            TransferPhase::DeltaTransfer,
            TransferPhase::Finalization,
            TransferPhase::Complete,
        ];
        for &expected_phase in &expected {
            let result = pipeline.advance().unwrap();
            assert_eq!(result, expected_phase);
            assert_eq!(pipeline.phase(), expected_phase);
        }
        assert!(pipeline.is_complete());
    }

    #[test]
    fn advance_past_complete_returns_error() {
        let mut pipeline = TransferPipeline::new(ServerRole::Generator);
        // Walk to Complete
        for _ in 0..5 {
            pipeline.advance().unwrap();
        }
        assert!(pipeline.is_complete());

        let err = pipeline.advance().unwrap_err();
        assert_eq!(err.current, TransferPhase::Complete);
    }

    #[test]
    fn advance_to_each_successor_succeeds() {
        let mut pipeline = TransferPipeline::new(ServerRole::Receiver);

        pipeline.advance_to(TransferPhase::FilterExchange).unwrap();
        assert_eq!(pipeline.phase(), TransferPhase::FilterExchange);

        pipeline
            .advance_to(TransferPhase::FileListTransfer)
            .unwrap();
        assert_eq!(pipeline.phase(), TransferPhase::FileListTransfer);

        pipeline.advance_to(TransferPhase::DeltaTransfer).unwrap();
        assert_eq!(pipeline.phase(), TransferPhase::DeltaTransfer);

        pipeline.advance_to(TransferPhase::Finalization).unwrap();
        assert_eq!(pipeline.phase(), TransferPhase::Finalization);

        pipeline.advance_to(TransferPhase::Complete).unwrap();
        assert_eq!(pipeline.phase(), TransferPhase::Complete);
    }

    #[test]
    fn advance_to_same_phase_is_invalid() {
        let mut pipeline = TransferPipeline::new(ServerRole::Receiver);
        let err = pipeline.advance_to(TransferPhase::Handshake).unwrap_err();
        assert_eq!(err.current, TransferPhase::Handshake);
        assert_eq!(err.target, TransferPhase::Handshake);
        // Phase should not change on error
        assert_eq!(pipeline.phase(), TransferPhase::Handshake);
    }

    #[test]
    fn advance_to_prior_phase_is_invalid() {
        let mut pipeline = TransferPipeline::new(ServerRole::Receiver);
        pipeline.advance_to(TransferPhase::FilterExchange).unwrap();

        let err = pipeline.advance_to(TransferPhase::Handshake).unwrap_err();
        assert_eq!(err.current, TransferPhase::FilterExchange);
        assert_eq!(err.target, TransferPhase::Handshake);
        assert_eq!(pipeline.phase(), TransferPhase::FilterExchange);
    }

    #[test]
    fn advance_to_skipping_phase_is_invalid() {
        let mut pipeline = TransferPipeline::new(ServerRole::Receiver);
        // Try to skip from Handshake to FileListTransfer (skipping FilterExchange)
        let err = pipeline
            .advance_to(TransferPhase::FileListTransfer)
            .unwrap_err();
        assert_eq!(err.current, TransferPhase::Handshake);
        assert_eq!(err.target, TransferPhase::FileListTransfer);
        assert_eq!(pipeline.phase(), TransferPhase::Handshake);
    }

    #[test]
    fn advance_to_from_complete_is_invalid() {
        let mut pipeline = TransferPipeline::new(ServerRole::Receiver);
        for _ in 0..5 {
            pipeline.advance().unwrap();
        }
        let err = pipeline.advance_to(TransferPhase::Handshake).unwrap_err();
        assert_eq!(err.current, TransferPhase::Complete);
    }

    #[test]
    fn advance_through_multi_step_succeeds() {
        let mut pipeline = TransferPipeline::new(ServerRole::Generator);
        pipeline
            .advance_through(TransferPhase::DeltaTransfer)
            .unwrap();
        assert_eq!(pipeline.phase(), TransferPhase::DeltaTransfer);
    }

    #[test]
    fn advance_through_single_step_succeeds() {
        let mut pipeline = TransferPipeline::new(ServerRole::Receiver);
        pipeline
            .advance_through(TransferPhase::FilterExchange)
            .unwrap();
        assert_eq!(pipeline.phase(), TransferPhase::FilterExchange);
    }

    #[test]
    fn advance_through_to_complete() {
        let mut pipeline = TransferPipeline::new(ServerRole::Receiver);
        pipeline.advance_through(TransferPhase::Complete).unwrap();
        assert!(pipeline.is_complete());
    }

    #[test]
    fn advance_through_same_phase_is_invalid() {
        let mut pipeline = TransferPipeline::new(ServerRole::Receiver);
        let err = pipeline
            .advance_through(TransferPhase::Handshake)
            .unwrap_err();
        assert_eq!(err.current, TransferPhase::Handshake);
        assert_eq!(err.target, TransferPhase::Handshake);
    }

    #[test]
    fn advance_through_backward_is_invalid() {
        let mut pipeline = TransferPipeline::new(ServerRole::Receiver);
        pipeline
            .advance_through(TransferPhase::DeltaTransfer)
            .unwrap();

        let err = pipeline
            .advance_through(TransferPhase::FilterExchange)
            .unwrap_err();
        assert_eq!(err.current, TransferPhase::DeltaTransfer);
        assert_eq!(err.target, TransferPhase::FilterExchange);
        assert_eq!(pipeline.phase(), TransferPhase::DeltaTransfer);
    }

    #[test]
    fn full_lifecycle_receiver() {
        let mut pipeline = TransferPipeline::new(ServerRole::Receiver);

        assert_eq!(pipeline.phase(), TransferPhase::Handshake);
        pipeline.advance().unwrap();

        assert_eq!(pipeline.phase(), TransferPhase::FilterExchange);
        pipeline.advance().unwrap();

        assert_eq!(pipeline.phase(), TransferPhase::FileListTransfer);
        pipeline.advance().unwrap();

        assert_eq!(pipeline.phase(), TransferPhase::DeltaTransfer);
        pipeline.advance().unwrap();

        assert_eq!(pipeline.phase(), TransferPhase::Finalization);
        pipeline.advance().unwrap();

        assert_eq!(pipeline.phase(), TransferPhase::Complete);
        assert!(pipeline.is_complete());
    }

    #[test]
    fn full_lifecycle_generator() {
        let mut pipeline = TransferPipeline::new(ServerRole::Generator);

        pipeline.advance_to(TransferPhase::FilterExchange).unwrap();
        pipeline
            .advance_to(TransferPhase::FileListTransfer)
            .unwrap();
        pipeline.advance_to(TransferPhase::DeltaTransfer).unwrap();
        pipeline.advance_to(TransferPhase::Finalization).unwrap();
        pipeline.advance_to(TransferPhase::Complete).unwrap();

        assert!(pipeline.is_complete());
        assert_eq!(pipeline.role(), ServerRole::Generator);
    }

    #[test]
    fn double_advance_to_same_target_fails() {
        let mut pipeline = TransferPipeline::new(ServerRole::Receiver);
        pipeline.advance_to(TransferPhase::FilterExchange).unwrap();
        let err = pipeline
            .advance_to(TransferPhase::FilterExchange)
            .unwrap_err();
        assert_eq!(err.current, TransferPhase::FilterExchange);
        assert_eq!(err.target, TransferPhase::FilterExchange);
    }

    #[test]
    fn error_display_is_descriptive() {
        let err = InvalidTransition {
            current: TransferPhase::DeltaTransfer,
            target: TransferPhase::Handshake,
        };
        let msg = format!("{err}");
        assert!(msg.contains("delta-transfer"), "missing current: {msg}");
        assert!(msg.contains("handshake"), "missing target: {msg}");
        assert!(msg.contains("forward-only"), "missing explanation: {msg}");
    }

    #[test]
    fn every_invalid_backward_transition_is_rejected() {
        let all_phases = [
            TransferPhase::Handshake,
            TransferPhase::FilterExchange,
            TransferPhase::FileListTransfer,
            TransferPhase::DeltaTransfer,
            TransferPhase::Finalization,
            TransferPhase::Complete,
        ];

        for (i, &current) in all_phases.iter().enumerate() {
            for &target in &all_phases[..i] {
                let mut pipeline = TransferPipeline::new(ServerRole::Receiver);
                pipeline.advance_through(current).ok();

                // Reconstruct a fresh pipeline at `current` for non-Handshake
                let mut pipeline = TransferPipeline::new(ServerRole::Receiver);
                if current != TransferPhase::Handshake {
                    pipeline.advance_through(current).unwrap();
                }

                let result = pipeline.advance_to(target);
                assert!(
                    result.is_err(),
                    "backward transition from {current} to {target} should fail"
                );
            }
        }
    }

    #[test]
    fn every_self_transition_is_rejected() {
        let all_phases = [
            TransferPhase::Handshake,
            TransferPhase::FilterExchange,
            TransferPhase::FileListTransfer,
            TransferPhase::DeltaTransfer,
            TransferPhase::Finalization,
            TransferPhase::Complete,
        ];

        for &phase in &all_phases {
            let mut pipeline = TransferPipeline::new(ServerRole::Receiver);
            if phase != TransferPhase::Handshake {
                pipeline.advance_through(phase).unwrap();
            }

            let result = pipeline.advance_to(phase);
            assert!(result.is_err(), "self-transition at {phase} should fail");
        }
    }

    #[test]
    fn advance_through_from_complete_is_invalid() {
        let mut pipeline = TransferPipeline::new(ServerRole::Receiver);
        pipeline.advance_through(TransferPhase::Complete).unwrap();

        // Every phase target should fail from Complete
        let all_phases = [
            TransferPhase::Handshake,
            TransferPhase::FilterExchange,
            TransferPhase::FileListTransfer,
            TransferPhase::DeltaTransfer,
            TransferPhase::Finalization,
            TransferPhase::Complete,
        ];

        for &target in &all_phases {
            let mut p = TransferPipeline::new(ServerRole::Receiver);
            p.advance_through(TransferPhase::Complete).unwrap();
            assert!(
                p.advance_through(target).is_err(),
                "advance_through({target}) from Complete should fail"
            );
        }
    }

    #[test]
    fn advance_past_complete_is_idempotently_rejected() {
        let mut pipeline = TransferPipeline::new(ServerRole::Receiver);
        pipeline.advance_through(TransferPhase::Complete).unwrap();

        // Multiple attempts to advance past Complete all fail
        for _ in 0..3 {
            assert!(pipeline.advance().is_err());
            assert_eq!(pipeline.phase(), TransferPhase::Complete);
        }
    }

    #[test]
    fn pipeline_debug_format_includes_phase_and_role() {
        let pipeline = TransferPipeline::new(ServerRole::Receiver);
        let debug = format!("{pipeline:?}");
        assert!(debug.contains("Handshake"), "debug missing phase: {debug}");
        assert!(debug.contains("Receiver"), "debug missing role: {debug}");
    }
}
