//! Typed error variants for the [`super::DeleteContext`] shutdown path.
//!
//! The phase-2 drain (`emit_one` / `emit_all`) hands the shared
//! `DeletePlanMap` off to a freshly-built `DeleteEmitter` via
//! [`Arc::try_unwrap`]. When the receiver still holds a clone of the
//! plan-map handle, or the cursor receiver channel has already been
//! taken by a prior drain attempt, the drain cannot proceed.
//!
//! Previously the failure surfaced as
//! `io::Error::new(ErrorKind::Other, "...")` with no strong-count and
//! no machine-readable variant. ATU-3 (tracked in #2380, per the audit
//! at `docs/audits/arc-try-unwrap-classification.md`) introduces a
//! strongly typed enum so callers can match on the failure mode and
//! operators see the residual [`Arc::strong_count`] in diagnostics.
//!
//! The cursor side migrated to channel-shutdown semantics in ATU-4
//! (#2381), so the historical `CursorStillShared`/`CursorPoisoned`
//! variants are no longer reachable; the residual cursor-side failure
//! mode is `CursorReceiverAlreadyTaken`, which only fires if
//! [`super::DeleteContext::into_emitter`] is called twice on the same
//! context value.
//!
//! [`Arc::strong_count`]: std::sync::Arc::strong_count

use std::io;

use thiserror::Error;

/// Errors surfaced by [`super::DeleteContext::emit_one`] /
/// [`super::DeleteContext::emit_all`] when the context cannot release
/// ownership of its shared phase-1 state.
///
/// Every variant records enough context for an operator to diagnose
/// which invariant was violated. The strong-count field captures the
/// residual [`Arc::strong_count`] observed at the failure site so a
/// leaked clone is immediately visible in logs.
///
/// [`Arc::strong_count`]: std::sync::Arc::strong_count
#[derive(Debug, Error)]
pub enum DeleteError {
    /// The [`super::DeletePlanMap`] handle still has outstanding
    /// clones. The receiver (or another phase-1 worker) must release
    /// its `Arc` before the emitter can take ownership.
    #[error(
        "DeleteContext::into_emitter: DeletePlanMap still shared (strong_count={strong_count})"
    )]
    PlanMapStillShared {
        /// Observed [`Arc::strong_count`] at the failure site.
        ///
        /// Always `>= 2` when this variant is constructed.
        ///
        /// [`Arc::strong_count`]: std::sync::Arc::strong_count
        strong_count: usize,
    },
    /// The cursor observation receiver was already consumed by a prior
    /// `DeleteContext::into_emitter` call. Reachable only if a
    /// caller drained the same context twice (the public `emit_*` API
    /// consumes `self`, so production code cannot hit this).
    #[error("DeleteContext::into_emitter: cursor receiver already taken")]
    CursorReceiverAlreadyTaken,
}

impl From<DeleteError> for io::Error {
    /// Maps a [`DeleteError`] to an [`io::Error`] so existing callers
    /// (currently `local_copy::executor::cleanup`) keep their
    /// `io::Result`-shaped API. The full typed message - including the
    /// strong-count - is preserved as the `Display` payload of the
    /// returned error.
    fn from(value: DeleteError) -> Self {
        io::Error::other(value.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_map_still_shared_display_includes_strong_count() {
        let err = DeleteError::PlanMapStillShared { strong_count: 3 };
        let msg = err.to_string();
        assert!(msg.contains("DeletePlanMap"));
        assert!(msg.contains("strong_count=3"));
    }

    #[test]
    fn cursor_receiver_already_taken_display_is_descriptive() {
        let err = DeleteError::CursorReceiverAlreadyTaken;
        assert!(err.to_string().contains("cursor receiver"));
    }

    #[test]
    fn delete_error_converts_into_io_error_with_typed_message() {
        let err: io::Error = DeleteError::PlanMapStillShared { strong_count: 2 }.into();
        assert_eq!(err.kind(), io::ErrorKind::Other);
        let msg = err.to_string();
        assert!(msg.contains("DeletePlanMap"));
        assert!(msg.contains("strong_count=2"));
    }
}
