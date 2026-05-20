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

/// Async-signal-safe handler function signature.
///
/// Handlers receive the signal number and must complete without allocating,
/// locking, or calling any non-async-signal-safe functions. Atomic stores
/// against `static AtomicBool`/`AtomicU8` are the canonical safe operation.
pub type SignalHandlerFn = extern "C" fn(c_int);

#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub use unix::install_signal_handler;

#[cfg(not(unix))]
mod stub;
#[cfg(not(unix))]
pub use stub::install_signal_handler;
