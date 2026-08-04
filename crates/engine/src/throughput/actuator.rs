//! Actuator subscription surface for the throughput governor.
//!
//! In the drum-buffer-rope design the governor's Strategy phase publishes
//! tuning signals - buffer-pool pressure, semaphore ceilings, I/O in-flight
//! depth - that platform actuators consume through this subscription. This
//! module carries the **buffer-pool pressure** signal: the governor publishes a
//! soft-capacity ceiling and the buffer pool reads it through an
//! [`ActuatorHandle`], composing it with its local pressure tracker
//! (conservative-min) in
//! [`maybe_resize`](crate::local_copy::buffer_pool). The governor never touches
//! the pool directly - it only publishes into the shared signal (Mediator).
//!
//! The default build stays byte-identical to the pre-governor code: an
//! [`ActuatorHandle::inert`] handle (the one a
//! [`GovernorMode::Off`](super::GovernorMode) governor hands out) publishes no
//! ceiling, so every subscriber falls back to its existing static behaviour.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Sentinel meaning "the governor publishes no buffer-pool ceiling".
///
/// A soft capacity of zero is never a valid pool size (the adaptive resizer
/// floors at `MIN_CAPACITY = 2`), so zero unambiguously flags an unset signal
/// without colliding with any ceiling a producer could legitimately publish.
const NO_CEILING: usize = 0;

/// Shared buffer-pool ceiling published by the governor and read by the pool.
///
/// A single lock-free `AtomicUsize` behind an `Arc`: the governor thread stores
/// the current recommendation and every actuator subscription loads it without
/// blocking the sense loop. Cloning an [`ActuatorHandle`] shares this cell, so a
/// ceiling published once is visible to every subscriber.
#[derive(Debug)]
pub(super) struct BufferCeiling(AtomicUsize);

impl BufferCeiling {
    /// Creates a shared, initially-unpublished ceiling signal.
    pub(super) fn new() -> Arc<Self> {
        Arc::new(Self(AtomicUsize::new(NO_CEILING)))
    }

    /// Publishes a new buffer-pool soft-capacity ceiling, or clears it.
    ///
    /// `None` (or a `Some(0)`, which would be an invalid pool size) clears the
    /// signal so subscribers revert to local-only pressure. Uses `Relaxed`
    /// ordering: the ceiling is a heuristic hint and a subscriber reading a
    /// momentarily stale value only defers one resize decision by a window.
    pub(super) fn publish(&self, ceiling: Option<usize>) {
        let value = ceiling.filter(|&c| c != NO_CEILING).unwrap_or(NO_CEILING);
        self.0.store(value, Ordering::Relaxed);
    }

    /// Loads the current ceiling, or `None` when none is published.
    fn load(&self) -> Option<usize> {
        match self.0.load(Ordering::Relaxed) {
            NO_CEILING => None,
            value => Some(value),
        }
    }
}

/// A subscription to governor actuator signals.
///
/// Returned by
/// [`GovernorHandle::subscribe_actuator`](super::governor::GovernorHandle::subscribe_actuator).
/// An [`inert`](Self::inert) handle - the one an
/// [`GovernorMode::Off`](super::GovernorMode) governor hands out - carries no
/// signal, so an actuator wired to it falls back to its existing static
/// behaviour. An active handle carries the shared buffer-pool ceiling the
/// governor publishes.
#[derive(Debug, Clone)]
pub struct ActuatorHandle {
    /// Shared buffer-pool ceiling, or `None` for an inert handle. The `Arc` is
    /// shared with the governor (the writer) and every clone of this handle.
    buffer_ceiling: Option<Arc<BufferCeiling>>,
}

impl ActuatorHandle {
    /// Constructs the inert handle: no signal, no action.
    #[must_use]
    pub fn inert() -> Self {
        Self {
            buffer_ceiling: None,
        }
    }

    /// Constructs an active handle backed by a shared buffer-pool ceiling.
    ///
    /// Only [`GovernorHandle::subscribe_actuator`](super::governor::GovernorHandle::subscribe_actuator)
    /// calls this, handing every subscriber a clone of the governor's own
    /// signal cell.
    pub(super) fn active(buffer_ceiling: Arc<BufferCeiling>) -> Self {
        Self {
            buffer_ceiling: Some(buffer_ceiling),
        }
    }

    /// Whether the governor is currently publishing a buffer-pool ceiling.
    ///
    /// `false` for an inert handle and for an active handle before the governor
    /// has published any ceiling. Actuators can call this to decide whether to
    /// defer to governor guidance; a `false` answer keeps them exactly as they
    /// are today.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.buffer_pool_ceiling().is_some()
    }

    /// The governor's current buffer-pool soft-capacity ceiling, or `None`.
    ///
    /// `None` means "no governor guidance" - either an inert handle or an active
    /// one with nothing published yet - and the buffer pool then resizes on its
    /// local pressure alone. A `Some(ceiling)` is an upper bound the pool
    /// composes with its local grow target via conservative-min: it never grows
    /// more aggressively than this ceiling permits.
    #[must_use]
    pub fn buffer_pool_ceiling(&self) -> Option<usize> {
        self.buffer_ceiling.as_ref().and_then(|c| c.load())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inert_handle_reports_no_action() {
        let handle = ActuatorHandle::inert();
        assert!(!handle.is_active());
        assert_eq!(handle.buffer_pool_ceiling(), None);
        // Cloning an inert handle is cheap and stays inert.
        assert!(!handle.clone().is_active());
    }

    #[test]
    fn active_handle_is_inert_until_a_ceiling_is_published() {
        let signal = BufferCeiling::new();
        let handle = ActuatorHandle::active(Arc::clone(&signal));
        // Wired but unpublished: still no guidance.
        assert!(!handle.is_active());
        assert_eq!(handle.buffer_pool_ceiling(), None);

        signal.publish(Some(16));
        assert!(handle.is_active());
        assert_eq!(handle.buffer_pool_ceiling(), Some(16));
    }

    #[test]
    fn published_ceiling_is_visible_across_clones() {
        let signal = BufferCeiling::new();
        let handle = ActuatorHandle::active(Arc::clone(&signal));
        let clone = handle.clone();
        signal.publish(Some(32));
        assert_eq!(clone.buffer_pool_ceiling(), Some(32));
    }

    #[test]
    fn publishing_none_or_zero_clears_the_signal() {
        let signal = BufferCeiling::new();
        let handle = ActuatorHandle::active(Arc::clone(&signal));
        signal.publish(Some(8));
        assert_eq!(handle.buffer_pool_ceiling(), Some(8));
        signal.publish(None);
        assert_eq!(handle.buffer_pool_ceiling(), None);
        signal.publish(Some(0));
        assert_eq!(handle.buffer_pool_ceiling(), None);
    }
}
