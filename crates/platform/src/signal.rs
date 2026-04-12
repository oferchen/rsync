//! Signal handler registration and shared atomic flags.
//!
//! # Unix
//!
//! Uses `signal_hook` crate (safe Rust) for POSIX signal registration.
//!
//! # Windows
//!
//! Uses `SetConsoleCtrlHandler` for console control events.
//!
//! # Other
//!
//! No-op - returns flags that are never set.
//!
//! # Upstream Reference
//!
//! `main.c` - signal handler setup for SIGPIPE, SIGHUP, SIGTERM, SIGUSR1, SIGUSR2.

use std::io;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

/// Shared atomic flags checked by the server accept loop.
///
/// On Unix, signal handlers set these via `signal_hook`. On Windows,
/// `SetConsoleCtrlHandler` sets them from a console control callback.
///
/// upstream: main.c - signal handler setup for SIGPIPE, SIGHUP, SIGTERM,
/// SIGUSR1, SIGUSR2.
#[derive(Clone, Debug, Default)]
pub struct SignalFlags {
    /// Reload configuration (SIGHUP on Unix).
    pub reload_config: Arc<AtomicBool>,
    /// Stop accepting connections and drain workers (SIGTERM/SIGINT on Unix,
    /// CTRL_C/CTRL_CLOSE on Windows).
    pub shutdown: Arc<AtomicBool>,
    /// Finish current transfers then exit with code 19 (`RERR_SIGNAL1`).
    /// Unlike `shutdown`, allows in-progress transfers to complete.
    /// upstream: main.c - SIGUSR1 sets `got_xfer_error = 1`.
    pub graceful_exit: Arc<AtomicBool>,
    /// Log a progress summary of active connections (SIGUSR2 on Unix).
    /// Consumed (reset to `false`) after each dump.
    pub progress_dump: Arc<AtomicBool>,
}

impl SignalFlags {
    /// Creates a new set of signal flags with all flags initially unset.
    pub fn new() -> Self {
        Self {
            reload_config: Arc::new(AtomicBool::new(false)),
            shutdown: Arc::new(AtomicBool::new(false)),
            graceful_exit: Arc::new(AtomicBool::new(false)),
            progress_dump: Arc::new(AtomicBool::new(false)),
        }
    }
}

/// Registers platform signal handlers and returns shared flags.
///
/// On Unix, registers handlers for SIGPIPE (ignore), SIGHUP (reload),
/// SIGTERM/SIGINT (shutdown), SIGUSR1 (graceful exit), SIGUSR2 (progress dump).
///
/// On Windows, registers a console control handler mapping CTRL_C/CTRL_CLOSE
/// to shutdown and CTRL_BREAK to graceful exit.
///
/// upstream: main.c - SIGACT() calls for SIGPIPE, SIGHUP, SIGTERM, SIGUSR1, SIGUSR2.
#[cfg(unix)]
pub fn register_signal_handlers() -> io::Result<SignalFlags> {
    use signal_hook::consts::{SIGHUP, SIGINT, SIGPIPE, SIGTERM, SIGUSR1, SIGUSR2};

    let flags = SignalFlags::new();

    // upstream: main.c SIGACT(SIGPIPE, SIG_IGN) - prevent daemon termination
    // on broken client sockets; errors surface as EPIPE io::Error instead.
    let sigpipe_sink = Arc::new(AtomicBool::new(false));
    signal_hook::flag::register(SIGPIPE, Arc::clone(&sigpipe_sink))?;

    signal_hook::flag::register(SIGHUP, Arc::clone(&flags.reload_config))?;
    signal_hook::flag::register(SIGTERM, Arc::clone(&flags.shutdown))?;
    signal_hook::flag::register(SIGINT, Arc::clone(&flags.shutdown))?;
    signal_hook::flag::register(SIGUSR1, Arc::clone(&flags.graceful_exit))?;
    signal_hook::flag::register(SIGUSR2, Arc::clone(&flags.progress_dump))?;

    Ok(flags)
}

/// Registers Windows console control handlers via `SetConsoleCtrlHandler`.
///
/// Maps CTRL_C/CTRL_CLOSE to `shutdown` and CTRL_BREAK to `graceful_exit`.
/// Broken pipes surface as I/O errors natively on Windows (no SIGPIPE).
/// Config reload requires a named event (not yet implemented).
#[cfg(windows)]
#[allow(unsafe_code)]
pub fn register_signal_handlers() -> io::Result<SignalFlags> {
    use windows::Win32::System::Console::{
        CTRL_BREAK_EVENT, CTRL_C_EVENT, CTRL_CLOSE_EVENT, SetConsoleCtrlHandler,
    };

    let flags = SignalFlags::new();

    // OnceLock statics allow the extern "system" callback to access the flags.
    static SHUTDOWN: std::sync::OnceLock<Arc<AtomicBool>> = std::sync::OnceLock::new();
    static GRACEFUL: std::sync::OnceLock<Arc<AtomicBool>> = std::sync::OnceLock::new();

    SHUTDOWN
        .set(Arc::clone(&flags.shutdown))
        .map_err(|_| io::Error::new(io::ErrorKind::Other, "signal handlers already registered"))?;
    GRACEFUL
        .set(Arc::clone(&flags.graceful_exit))
        .map_err(|_| io::Error::new(io::ErrorKind::Other, "signal handlers already registered"))?;

    // SAFETY: The handler function signature matches what SetConsoleCtrlHandler expects.
    // The OnceLock statics ensure the Arc references remain valid for the process lifetime.
    unsafe extern "system" fn handler(ctrl_type: u32) -> windows::core::BOOL {
        use std::sync::atomic::Ordering;

        match ctrl_type {
            x if x == CTRL_C_EVENT || x == CTRL_CLOSE_EVENT => {
                if let Some(flag) = SHUTDOWN.get() {
                    flag.store(true, Ordering::Relaxed);
                }
                windows::core::BOOL(1)
            }
            x if x == CTRL_BREAK_EVENT => {
                if let Some(flag) = GRACEFUL.get() {
                    flag.store(true, Ordering::Relaxed);
                }
                windows::core::BOOL(1)
            }
            _ => windows::core::BOOL(0),
        }
    }

    unsafe { SetConsoleCtrlHandler(Some(handler), true) }.map_err(|e| {
        io::Error::new(
            io::ErrorKind::Other,
            format!("SetConsoleCtrlHandler failed: {e}"),
        )
    })?;

    Ok(flags)
}

/// No-op signal registration for platforms that are neither Unix nor Windows.
#[cfg(all(not(unix), not(windows)))]
pub fn register_signal_handlers() -> io::Result<SignalFlags> {
    Ok(SignalFlags::new())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

    #[test]
    fn signal_flags_default_to_false() {
        let flags = SignalFlags::new();
        assert!(!flags.reload_config.load(Ordering::Relaxed));
        assert!(!flags.shutdown.load(Ordering::Relaxed));
        assert!(!flags.graceful_exit.load(Ordering::Relaxed));
        assert!(!flags.progress_dump.load(Ordering::Relaxed));
    }

    #[test]
    fn signal_flags_can_be_set() {
        let flags = SignalFlags::new();
        flags.reload_config.store(true, Ordering::Relaxed);
        assert!(flags.reload_config.load(Ordering::Relaxed));
        flags.shutdown.store(true, Ordering::Relaxed);
        assert!(flags.shutdown.load(Ordering::Relaxed));
        flags.graceful_exit.store(true, Ordering::Relaxed);
        assert!(flags.graceful_exit.load(Ordering::Relaxed));
        flags.progress_dump.store(true, Ordering::Relaxed);
        assert!(flags.progress_dump.load(Ordering::Relaxed));
    }

    #[test]
    fn register_signal_handlers_succeeds() {
        let result = register_signal_handlers();
        assert!(result.is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn registered_flags_start_unset() {
        let flags = register_signal_handlers().expect("registration succeeds");
        assert!(!flags.reload_config.load(Ordering::Relaxed));
        assert!(!flags.shutdown.load(Ordering::Relaxed));
        assert!(!flags.graceful_exit.load(Ordering::Relaxed));
        assert!(!flags.progress_dump.load(Ordering::Relaxed));
    }

    #[test]
    fn graceful_exit_flag_independent_of_shutdown() {
        let flags = SignalFlags::new();
        flags.graceful_exit.store(true, Ordering::Relaxed);
        assert!(flags.graceful_exit.load(Ordering::Relaxed));
        assert!(
            !flags.shutdown.load(Ordering::Relaxed),
            "graceful_exit must not affect shutdown flag"
        );
    }

    #[test]
    fn progress_dump_consumed_on_swap() {
        let flags = SignalFlags::new();
        flags.progress_dump.store(true, Ordering::Relaxed);
        assert!(flags.progress_dump.swap(false, Ordering::Relaxed));
        assert!(
            !flags.progress_dump.load(Ordering::Relaxed),
            "swap must reset progress_dump to false"
        );
    }

    #[test]
    fn signal_flags_arcs_are_independent_clones() {
        let flags = SignalFlags::new();
        let shutdown_clone = Arc::clone(&flags.shutdown);
        shutdown_clone.store(true, Ordering::Relaxed);
        assert!(
            flags.shutdown.load(Ordering::Relaxed),
            "cloned arc must share state with original"
        );
        assert!(
            !flags.reload_config.load(Ordering::Relaxed),
            "other flags must remain unaffected"
        );
    }

    #[test]
    fn multiple_flags_can_be_set_simultaneously() {
        let flags = SignalFlags::new();
        flags.shutdown.store(true, Ordering::Relaxed);
        flags.graceful_exit.store(true, Ordering::Relaxed);
        flags.reload_config.store(true, Ordering::Relaxed);
        flags.progress_dump.store(true, Ordering::Relaxed);
        assert!(flags.shutdown.load(Ordering::Relaxed));
        assert!(flags.graceful_exit.load(Ordering::Relaxed));
        assert!(flags.reload_config.load(Ordering::Relaxed));
        assert!(flags.progress_dump.load(Ordering::Relaxed));
    }

    #[cfg(unix)]
    #[test]
    fn registered_flags_are_shared_via_arc() {
        let flags = register_signal_handlers().expect("registration succeeds");
        let shutdown_clone = Arc::clone(&flags.shutdown);
        shutdown_clone.store(true, Ordering::Relaxed);
        assert!(
            flags.shutdown.load(Ordering::Relaxed),
            "registered flag arcs must be shared"
        );
    }
}
