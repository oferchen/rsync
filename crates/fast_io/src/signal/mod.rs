//! Safe wrappers around platform signal-handler installation.
//!
//! Unix uses `libc::sigaction`; non-Unix targets use a no-op stub. Callers
//! pass an `extern "C" fn(c_int)` handler that must be async-signal-safe
//! (atomic stores only, no allocation, no locking). The wrapper installs the
//! handler with `SA_RESTART` so interrupted syscalls auto-retry, matching
//! upstream rsync's `sigaction` setup in `main.c`.
//!
//! This module exists so the `core` crate can keep `#![deny(unsafe_code)]`
//! while still installing real Unix signal handlers. The unsafe FFI calls
//! are confined to this crate per the workspace unsafe-code policy.

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
/// Async-signal-safe: performs a single atomic store, so it
/// is safe to call from within a signal handler.
#[inline]
pub fn mark_shutdown() {
    SHUTDOWN_REQUESTED.store(true, Ordering::SeqCst);
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
}

#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub use unix::install_signal_handler;

#[cfg(not(unix))]
mod stub;
#[cfg(not(unix))]
pub use stub::install_signal_handler;
