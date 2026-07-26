//! Registry of hooks that wake a thread blocked on network I/O.
//!
//! A signal handler can only flip an atomic; the transfer that must react to
//! it is usually parked inside a blocking `read()` on the transport socket and
//! will never look at the flag on its own. Upstream does not have this problem
//! because it is multi-process and single-threaded per process: the signal
//! interrupts the one `select()` in `perform_io()`, which `io.c:766-779`
//! handles exactly like a timeout before re-checking `got_kill_signal`
//! (`io.c:750`). In a multi-threaded process the kernel delivers the signal to
//! an arbitrary thread, so `EINTR` alone cannot be relied on to reach the
//! thread that is actually blocked on the wire.
//!
//! The transport therefore registers a waker for the lifetime of the session
//! and the client's signal watcher fires it in normal (non-signal) context.
//! For a TCP transport the waker half-closes the socket, so the blocked read
//! returns immediately and the transfer unwinds through the same path a
//! dropped connection already takes - including the `--partial` /
//! `--partial-dir` retention in `disk_commit`.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

/// A hook that unblocks a thread parked in transport I/O.
///
/// Invoked from the signal-watcher thread, never from a signal handler, so it
/// is free to allocate, lock, and issue arbitrary syscalls. It must be
/// idempotent: a repeated interrupt request fires it again.
pub type IoWaker = Arc<dyn Fn() + Send + Sync>;

/// Registered wakers keyed by the token handed back to the registrant.
static WAKERS: OnceLock<Mutex<Vec<(u64, IoWaker)>>> = OnceLock::new();

/// Source of unique registration tokens.
static NEXT_TOKEN: AtomicU64 = AtomicU64::new(1);

fn wakers() -> &'static Mutex<Vec<(u64, IoWaker)>> {
    WAKERS.get_or_init(|| Mutex::new(Vec::new()))
}

/// RAII registration handle: deregisters its waker on drop.
///
/// Holding the guard for exactly the transfer's lifetime keeps the registry
/// from firing against a socket whose session has already finished.
#[derive(Debug)]
pub struct IoWakerGuard {
    token: u64,
}

impl Drop for IoWakerGuard {
    fn drop(&mut self) {
        if let Ok(mut list) = wakers().lock() {
            list.retain(|(token, _)| *token != self.token);
        }
    }
}

/// Registers `waker` until the returned guard is dropped.
///
/// Returns `None` when the registry mutex is poisoned; the caller simply runs
/// without a wake hook rather than failing the transfer.
#[must_use]
pub fn register_io_waker(waker: IoWaker) -> Option<IoWakerGuard> {
    let token = NEXT_TOKEN.fetch_add(1, Ordering::Relaxed);
    let mut list = wakers().lock().ok()?;
    list.push((token, waker));
    Some(IoWakerGuard { token })
}

/// Fires every registered waker.
///
/// Called by the client's signal watcher once a shutdown has been requested,
/// so a transfer blocked on the wire observes the request instead of parking
/// until the peer or the `--timeout` deadline releases it.
pub fn wake_blocked_io() {
    let hooks: Vec<IoWaker> = match wakers().lock() {
        Ok(list) => list.iter().map(|(_, waker)| Arc::clone(waker)).collect(),
        Err(_) => return,
    };
    for hook in hooks {
        hook();
    }
}

/// Number of currently registered wakers. Diagnostics and tests only.
#[must_use]
pub fn registered_waker_count() -> usize {
    wakers().lock().map(|list| list.len()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    /// The registry is process-global and `wake_blocked_io` fires every entry,
    /// so concurrent tests would see each other's wakers.
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn waker_fires_while_registered_and_not_after_drop() {
        let _serial = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let hits = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&hits);
        let guard = register_io_waker(Arc::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
        }))
        .expect("registry available");

        wake_blocked_io();
        assert_eq!(hits.load(Ordering::SeqCst), 1);

        drop(guard);
        wake_blocked_io();
        assert_eq!(
            hits.load(Ordering::SeqCst),
            1,
            "a dropped guard must leave no waker behind"
        );
    }

    #[test]
    fn wake_is_repeatable_for_a_live_registration() {
        let _serial = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let hits = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&hits);
        let _guard = register_io_waker(Arc::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
        }))
        .expect("registry available");

        wake_blocked_io();
        wake_blocked_io();
        assert_eq!(hits.load(Ordering::SeqCst), 2);
    }
}
