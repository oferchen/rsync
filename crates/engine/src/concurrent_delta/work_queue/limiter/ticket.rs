//! RAII ticket guard and overload classification.
//!
//! A [`Ticket`] reserves an in-flight slot on an [`AimdLimiter`]. The
//! ticket schedules release back to the limiter when consumed via
//! `record_success`, `record_overload`, or `record_error`. Dropping a
//! ticket without recording (panic path) decrements `in_flight` without
//! touching the target so the limiter stays consistent.

use std::io;
use std::sync::atomic::Ordering;

use super::rate::AimdLimiter;

/// Reason a [`Ticket`] was released as an overload signal.
///
/// Maps to the four overload sources in design section 3.4.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverloadReason {
    /// Completion latency exceeded `rtt_ema + 2 * sqrt(rtt_var)`.
    RttSpike,
    /// The bounded work queue rejected `try_send` with `Full`.
    QueueSaturated,
    /// The disk-commit writer reported high-water-mark pressure.
    DiskCommitPressure,
    /// The rolling error rate crossed the threshold (more than `target / 8`
    /// transient errors in the last `target` completions).
    ErrorRate,
}

/// RAII slot guard returned by [`AimdLimiter::try_acquire`].
///
/// Callers must consume the ticket via one of [`Ticket::record_success`],
/// [`Ticket::record_overload`], or [`Ticket::record_error`]. Dropping the
/// ticket without recording (panic case) decrements `in_flight` without
/// touching `target` so the limiter stays consistent.
#[must_use = "the ticket reserves a slot; record success/overload/error to release it correctly"]
#[derive(Debug)]
pub struct Ticket<'a> {
    limiter: &'a AimdLimiter,
    acquired_at: u64,
    consumed: bool,
}

impl<'a> Ticket<'a> {
    /// Internal constructor used by [`AimdLimiter::try_acquire`].
    pub(super) fn new(limiter: &'a AimdLimiter, acquired_at: u64) -> Self {
        Self {
            limiter,
            acquired_at,
            consumed: false,
        }
    }

    /// Records a successful completion and releases the slot.
    pub fn record_success(mut self) {
        self.consumed = true;
        self.limiter.release_success(self.acquired_at);
    }

    /// Records an overload completion and releases the slot.
    pub fn record_overload(mut self, reason: OverloadReason) {
        self.consumed = true;
        self.limiter.release_overload(self.acquired_at, reason);
    }

    /// Records an `io::Error` completion. Transient kinds (`WouldBlock`,
    /// `Interrupted`, `TimedOut`) classify as [`OverloadReason::ErrorRate`];
    /// deterministic kinds (`NotFound`, `PermissionDenied`, etc.) are treated
    /// as success so that filesystem state does not collapse `target`.
    /// See design section 3.4 item (3).
    pub fn record_error(self, kind: io::ErrorKind) {
        if matches!(
            kind,
            io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted | io::ErrorKind::TimedOut
        ) {
            self.record_overload(OverloadReason::ErrorRate);
        } else {
            self.record_success();
        }
    }
}

impl Drop for Ticket<'_> {
    fn drop(&mut self) {
        if !self.consumed {
            // Panic case: release the slot without disturbing target/EMA so the
            // limiter does not think we leaked a slot. We deliberately do NOT
            // call `release_success` here because we have no successful sample
            // to feed into the EMA.
            self.limiter.in_flight.fetch_sub(1, Ordering::AcqRel);
        }
    }
}
