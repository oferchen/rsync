//! Signal handling for graceful shutdown and cleanup.
//!
//! # Safety
//!
//! This module uses unsafe code to install Unix signal handlers via libc.
//! All signal handlers are async-signal-safe and only set atomic flags.
#![allow(unsafe_code)]
//!
//! This module provides Unix signal handling that matches upstream rsync's behavior
//! for SIGINT, SIGTERM, SIGHUP, and SIGPIPE. It supports:
//!
//! - **Graceful shutdown**: First signal allows current file to complete
//! - **Forced termination**: Second signal causes immediate exit
//! - **Cleanup coordination**: Register callbacks for temp file cleanup
//! - **Thread-safe access**: Atomic flags for signal state checking
//!
//! # Signal Behavior
//!
//! - **SIGINT (Ctrl+C)**:
//!   - First: Sets graceful shutdown flag, allows current operation to complete
//!   - Second: Sets abort flag for immediate exit
//! - **SIGTERM**: Graceful shutdown on first signal, abort on second
//! - **SIGHUP**: Same as SIGTERM
//! - **SIGPIPE**: Treated as connection loss, causes immediate graceful shutdown
//!
//! # Upstream Reference
//!
//! This implementation mirrors the signal handling in upstream rsync's:
//! - `main.c` - Signal handler installation
//! - `cleanup.c` - Cleanup and temp file removal
//! - `rsync.h` - Exit code definitions
//!
//! # Examples
//!
//! ```no_run
//! use core::signal::{SignalHandler, CleanupManager};
//! use std::path::PathBuf;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! // Install signal handlers at program start
//! let handler = SignalHandler::install()?;
//!
//! // Register temp file for cleanup
//! CleanupManager::global().register_temp_file(PathBuf::from("/tmp/file.tmp"));
//!
//! // Check for shutdown during operations
//! if handler.is_shutdown_requested() {
//!     println!("Shutdown requested, cleaning up...");
//!     CleanupManager::global().cleanup();
//!     std::process::exit(20); // ExitCode::Signal
//! }
//!
//! // Unregister temp file when successfully completed
//! CleanupManager::global().unregister_temp_file("/tmp/file.tmp");
//! # Ok(())
//! # }
//! ```
//!
//! # Platform Support
//!
//! - **Unix**: Full signal handling with SIGINT, SIGTERM, SIGHUP, SIGPIPE
//! - **Windows**: Limited support (Ctrl+C only) with graceful degradation

#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub use unix::{ShutdownReason, SignalHandler, install_signal_handlers, wait_for_signal};

#[cfg(not(unix))]
mod stub;
#[cfg(not(unix))]
pub use stub::{ShutdownReason, SignalHandler, install_signal_handlers, wait_for_signal};

mod cleanup;
pub use cleanup::CleanupManager;

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

/// Global flag indicating a graceful shutdown has been requested.
///
/// This is set on the first SIGINT/SIGTERM/SIGHUP and should be checked
/// periodically during operations to allow clean completion of the current file.
static SHUTDOWN_REQUESTED: AtomicBool = AtomicBool::new(false);

/// Global flag indicating an immediate abort has been requested.
///
/// This is set on the second SIGINT/SIGTERM or immediately on critical errors.
/// When set, operations should terminate as quickly as possible after cleanup.
static ABORT_REQUESTED: AtomicBool = AtomicBool::new(false);

/// Global storage for the shutdown reason (encoded as u8).
///
/// 0 = None, 1 = Interrupted, 2 = Terminated, 3 = HangUp, 4 = PipeBroken, 5 = UserRequested
static SHUTDOWN_REASON_CODE: AtomicU8 = AtomicU8::new(0);

/// Sets the graceful shutdown flag.
///
/// This should be called from signal handlers or when a graceful shutdown
/// is requested programmatically.
///
/// # Examples
///
/// ```
/// use core::signal::{request_shutdown, ShutdownReason};
///
/// // Request a graceful shutdown
/// request_shutdown(ShutdownReason::UserRequested);
/// ```
#[inline]
pub fn request_shutdown(reason: ShutdownReason) {
    SHUTDOWN_REASON_CODE.store(reason as u8, Ordering::SeqCst);
    SHUTDOWN_REQUESTED.store(true, Ordering::SeqCst);
}

/// Sets the abort flag for immediate termination.
///
/// This should be called on the second signal or when immediate termination
/// is required.
///
/// # Examples
///
/// ```
/// use core::signal::request_abort;
///
/// // Request immediate termination
/// request_abort();
/// ```
#[inline]
pub fn request_abort() {
    ABORT_REQUESTED.store(true, Ordering::SeqCst);
}

/// Checks if a graceful shutdown has been requested.
///
/// Returns `true` if any signal handler has set the shutdown flag.
/// Operations should complete the current file and exit cleanly.
///
/// # Examples
///
/// ```
/// use core::signal;
///
/// fn process_files() {
///     for file in files() {
///         if signal::is_shutdown_requested() {
///             println!("Shutdown requested, stopping after current file");
///             break;
///         }
///         process_file(file);
///     }
/// }
/// # fn files() -> Vec<String> { vec![] }
/// # fn process_file(_: String) {}
/// ```
#[inline]
#[must_use]
pub fn is_shutdown_requested() -> bool {
    SHUTDOWN_REQUESTED.load(Ordering::Relaxed)
}

/// Checks if an immediate abort has been requested.
///
/// Returns `true` if a second signal was received or abort was explicitly requested.
/// Operations should terminate immediately after minimal cleanup.
///
/// # Examples
///
/// ```
/// use core::signal;
///
/// fn transfer_file() {
///     for chunk in chunks() {
///         if signal::is_abort_requested() {
///             println!("Abort requested, terminating immediately");
///             return;
///         }
///         transfer_chunk(chunk);
///     }
/// }
/// # fn chunks() -> Vec<Vec<u8>> { vec![] }
/// # fn transfer_chunk(_: Vec<u8>) {}
/// ```
#[inline]
#[must_use]
pub fn is_abort_requested() -> bool {
    ABORT_REQUESTED.load(Ordering::Relaxed)
}

/// Returns the reason for shutdown, if any.
///
/// Returns `None` if no shutdown has been requested.
///
/// # Examples
///
/// ```
/// use core::signal::{shutdown_reason, ShutdownReason};
///
/// if let Some(reason) = shutdown_reason() {
///     match reason {
///         ShutdownReason::Interrupted => println!("Interrupted by user"),
///         ShutdownReason::Terminated => println!("Terminated by system"),
///         _ => println!("Other shutdown reason"),
///     }
/// }
/// ```
#[must_use]
pub fn shutdown_reason() -> Option<ShutdownReason> {
    let code = SHUTDOWN_REASON_CODE.load(Ordering::Relaxed);
    ShutdownReason::from_u8(code)
}

/// Resets all signal flags.
///
/// This is primarily useful for testing. In production code, signal flags
/// should not be reset once set.
#[doc(hidden)]
pub fn reset_for_testing() {
    SHUTDOWN_REQUESTED.store(false, Ordering::SeqCst);
    ABORT_REQUESTED.store(false, Ordering::SeqCst);
    SHUTDOWN_REASON_CODE.store(0, Ordering::SeqCst);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shutdown_flags_start_false() {
        reset_for_testing();
        assert!(!is_shutdown_requested());
        assert!(!is_abort_requested());
        assert!(shutdown_reason().is_none());
    }

    #[test]
    fn request_shutdown_sets_flags() {
        reset_for_testing();
        request_shutdown(ShutdownReason::Interrupted);
        assert!(is_shutdown_requested());
        assert!(!is_abort_requested());
        assert_eq!(shutdown_reason(), Some(ShutdownReason::Interrupted));
    }

    #[test]
    fn request_abort_sets_flag() {
        reset_for_testing();
        request_abort();
        assert!(is_abort_requested());
    }

    #[test]
    fn shutdown_reason_roundtrips() {
        reset_for_testing();
        for reason in [
            ShutdownReason::Interrupted,
            ShutdownReason::Terminated,
            ShutdownReason::HangUp,
            ShutdownReason::PipeBroken,
            ShutdownReason::UserRequested,
        ] {
            request_shutdown(reason);
            assert_eq!(shutdown_reason(), Some(reason));
            reset_for_testing();
        }
    }

    #[test]
    fn reset_clears_all_flags() {
        request_shutdown(ShutdownReason::Terminated);
        request_abort();
        reset_for_testing();
        assert!(!is_shutdown_requested());
        assert!(!is_abort_requested());
        assert!(shutdown_reason().is_none());
    }
}
