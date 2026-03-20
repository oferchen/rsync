/// Tracks the number of active client connections in the daemon server loop.
///
/// The counter uses an [`AtomicUsize`] for lock-free concurrent access from
/// worker threads. Each accepted connection increments the counter via
/// [`ConnectionCounter::acquire`], which returns a [`ConnectionGuard`] that
/// automatically decrements the counter when dropped.
///
/// The counter is wrapped in an `Arc` so it can be shared between the main
/// accept loop and spawned worker threads. This enables future max-connections
/// enforcement at the daemon level (as opposed to the per-module limits
/// already tracked by `ModuleRuntime::active_connections`).
///
/// upstream: clientserver.c - `count_connections()` tracks active children
/// for the `max connections` global directive.
#[derive(Debug)]
pub(crate) struct ConnectionCounter {
    active: Arc<AtomicUsize>,
}

impl ConnectionCounter {
    /// Creates a new connection counter with zero active connections.
    pub(crate) fn new() -> Self {
        Self {
            active: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// Returns the current number of active connections.
    ///
    /// Wired into the accept loop for future daemon-level max-connections
    /// enforcement.
    #[allow(dead_code)]
    pub(crate) fn active(&self) -> usize {
        self.active.load(Ordering::Acquire)
    }

    /// Increments the counter and returns an RAII guard that decrements it on drop.
    pub(crate) fn acquire(&self) -> ConnectionGuard {
        self.active.fetch_add(1, Ordering::AcqRel);
        ConnectionGuard {
            counter: Arc::clone(&self.active),
        }
    }
}

impl Default for ConnectionCounter {
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for ConnectionCounter {
    fn clone(&self) -> Self {
        Self {
            active: Arc::clone(&self.active),
        }
    }
}

/// RAII guard that decrements the parent [`ConnectionCounter`] when dropped.
///
/// Created by [`ConnectionCounter::acquire`]. The guard holds an `Arc` reference
/// to the shared atomic counter, ensuring the decrement occurs even if the
/// owning thread panics (since `Drop` runs during unwinding).
#[derive(Debug)]
pub(crate) struct ConnectionGuard {
    counter: Arc<AtomicUsize>,
}

impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        self.counter.fetch_sub(1, Ordering::AcqRel);
    }
}
