//! Mutable PID state guarded by a single mutex.

use std::time::Instant;

/// Inner mutable state guarded by a single mutex.
///
/// Contains the integrator, the previous error sample, and the timestamp of
/// the previous sample. A single mutex is used because all three values are
/// updated atomically on every call to
/// [`AdaptiveBufferController::observe`](super::controller::AdaptiveBufferController::observe).
#[derive(Debug, Default)]
pub(super) struct ControllerState {
    pub(super) integral: f64,
    pub(super) prev_error: f64,
    pub(super) prev_sample_at: Option<Instant>,
}
