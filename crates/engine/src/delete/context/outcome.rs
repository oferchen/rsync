//! [`DrainOutcome`] - the value returned by every `DeleteContext::emit_*`
//! call. Bundles the consumed [`DeleteFs`] dispatcher, the running stats
//! the drain accumulated, and the mapped exit code so callers can both
//! inspect the outcome (in tests) and surface it to the user.

#[cfg(not(feature = "parallel-delete-consumer"))]
use super::super::emitter::DeleteEmitter;
use super::super::emitter::DeleteFs;

/// Result of draining one or more directories through the emitter.
///
/// Owns the [`DeleteFs`] so test callers using
/// [`super::super::RecordingDeleteFs`] can inspect the recorded event
/// sequence after the drain returns.
#[derive(Debug)]
pub struct DrainOutcome<F: DeleteFs> {
    /// The filesystem dispatcher the emitter consumed. Production code
    /// drops this; tests inspect `events()` on a `RecordingDeleteFs`.
    pub fs: F,
    /// Running deletion statistics, mutated only inside the drain.
    pub stats: protocol::DeleteStats,
    /// Accumulated `io_error` bitmask the caller maps to an exit code.
    pub io_error: i32,
    /// Mapped exit code (`0`, `23`, or `24`) for the run.
    pub exit_code: i32,
}

impl<F: DeleteFs> DrainOutcome<F> {
    /// Builds an outcome by snapshotting `emitter`'s post-drain state
    /// (stats, io_error, exit code) and taking ownership of the
    /// underlying [`DeleteFs`]. Used by `DeleteContext::emit_one` once
    /// `DeleteEmitter::emit_all` returns.
    #[cfg(not(feature = "parallel-delete-consumer"))]
    pub(super) fn from_emitter(emitter: DeleteEmitter<F>) -> Self {
        let stats = emitter.stats();
        let io_error = emitter.io_error();
        let exit_code = emitter.exit_code();
        let fs = emitter.into_fs();
        Self {
            fs,
            stats,
            io_error,
            exit_code,
        }
    }
}
