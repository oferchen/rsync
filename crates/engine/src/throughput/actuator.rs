//! Actuator subscription surface for the throughput governor.
//!
//! In the drum-buffer-rope design the governor's Strategy phase publishes
//! tuning signals - buffer-pool pressure, semaphore ceilings, I/O in-flight
//! depth - that platform actuators consume through this subscription. G2 is
//! inert: [`ActuatorHandle::inert`] is the only constructor and every query
//! reports "no action", so subscribing changes nothing. The type exists now so
//! consumers can wire the subscription and later steps can add real signals
//! without changing call sites.

/// A subscription to governor actuator signals.
///
/// Returned by
/// [`GovernorHandle::subscribe_actuator`](super::governor::GovernorHandle::subscribe_actuator).
/// The inert handle answers every query with the "take no action" value, so an
/// actuator wired to it falls back to its existing static behaviour.
#[derive(Debug, Clone)]
pub struct ActuatorHandle {
    /// Reserved for the shared signal state added when real actuation lands.
    /// A unit field keeps the struct non-exhaustive in spirit and the
    /// constructor meaningful without exposing internals.
    _private: (),
}

impl ActuatorHandle {
    /// Constructs the inert handle: no signals, no action.
    #[must_use]
    pub fn inert() -> Self {
        Self { _private: () }
    }

    /// Whether the governor is currently publishing any actuator signal.
    ///
    /// Always `false` in G2. Actuators call this to decide whether to defer to
    /// governor guidance or keep their static behaviour; a `false` answer keeps
    /// them exactly as they are today.
    #[must_use]
    pub fn is_active(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inert_handle_reports_no_action() {
        let handle = ActuatorHandle::inert();
        assert!(!handle.is_active());
        // Cloning an inert handle is cheap and stays inert.
        assert!(!handle.clone().is_active());
    }
}
