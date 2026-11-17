//! Safe wrappers around platform signal-handler installation.
//!
//! Unix uses `libc::sigaction`; non-Unix targets use a no-op stub. Callers
//! pass an `extern "C" fn(c_int)` handler that must be async-signal-safe
//! (atomic stores only, no allocation, no locking). The wrapper installs the
//! handler with `sa_flags == 0`, matching upstream rsync's `SIGACTION` macro
//! (`rsync.h:1294`) over its never-flagged file-static `sigact`, so a blocked
//! syscall fails with `EINTR` instead of restarting.
//!
//! This module exists so the `core` crate can keep `#![deny(unsafe_code)]`
//! while still installing real Unix signal handlers. The unsafe FFI calls
//! are confined to this crate per the workspace unsafe-code policy.

use std::fmt;
use std::io;
use std::os::raw::c_int;
use std::sync::atomic::{AtomicBool, Ordering};

/// Async-signal-safe handler function signature.
///
/// Handlers receive the signal number and must complete without allocating,
/// locking, or calling any non-async-signal-safe functions. Atomic stores
/// against `static AtomicBool`/`AtomicU8` are the canonical safe operation.
pub type SignalHandlerFn = extern "C" fn(c_int);

/// Process-global graceful-shutdown flag.
///
/// Set from an async-signal-safe handler (a plain atomic store) and polled by
/// long-running loops elsewhere in the workspace. It lives in `fast_io`
/// because that is the lowest crate both `core` (which installs the handlers)
/// and `engine` (which polls it mid-transfer) already depend on, so the flag
/// can be a single `'static` without a dependency cycle.
///
/// upstream: rsync's `sig_int`/`sig_term` handlers set `got_xfer_error` /
/// call `_exit_cleanup`; here the handler only flips this flag and the
/// transfer loop performs the cleanup in normal (non-signal) context.
static SHUTDOWN_REQUESTED: AtomicBool = AtomicBool::new(false);

/// Marks that a graceful shutdown has been requested.
///
/// Async-signal-safe: an atomic store, then, once [`arm_wake`] has created
/// the wake pipe, at most one non-blocking `write(2)` to it, so a transport
/// waiting on another thread returns (see [`wake_fd`]).
#[inline]
pub fn mark_shutdown() {
    SHUTDOWN_REQUESTED.store(true, Ordering::SeqCst);
    #[cfg(unix)]
    wake::ring();
}

/// Returns `true` once [`mark_shutdown`] has been called.
///
/// Polled by long-running copy loops so they can stop promptly mid-file and
/// let the normal-context cleanup path finalise any in-progress temp file.
#[inline]
#[must_use]
pub fn shutdown_requested() -> bool {
    SHUTDOWN_REQUESTED.load(Ordering::Relaxed)
}

/// Clears the shutdown flag. Intended for tests that must reset global state.
#[doc(hidden)]
pub fn reset_shutdown_for_testing() {
    SHUTDOWN_REQUESTED.store(false, Ordering::SeqCst);
    #[cfg(unix)]
    wake::drain();
}

/// The error a transfer stops with once a shutdown signal has arrived;
/// graded `RERR_SIGNAL` (20).
///
/// upstream: `io.c:600 handle_kill_signal()` -> `exit_cleanup(RERR_SIGNAL)`,
/// whose message is `log.c:102` "received SIGINT, SIGTERM, or SIGHUP".
#[derive(Debug)]
pub struct SignalShutdown;

impl fmt::Display for SignalShutdown {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("received SIGINT, SIGTERM, or SIGHUP")
    }
}

impl std::error::Error for SignalShutdown {}

/// Returns a [`SignalShutdown`] error.
///
/// The kind is `Other`, never `Interrupted`: an `Interrupted` error is
/// retried by `write_all`, `read_exact` and this crate's own retry loops, so
/// it would put a stopped transfer straight back into the call it left.
#[must_use]
pub fn shutdown_error() -> io::Error {
    io::Error::other(SignalShutdown)
}

#[cfg(unix)]
mod wake;
#[cfg(unix)]
pub use wake::{arm_wake, wake_fd};

#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub use unix::install_signal_handler;

#[cfg(not(unix))]
mod stub;
#[cfg(not(unix))]
pub use stub::install_signal_handler;
