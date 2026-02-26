// Daemon signal handling.
//
// Registers handlers for SIGPIPE (ignore), SIGHUP (config reload flag),
// SIGTERM/SIGINT (graceful shutdown flag), SIGUSR1 (graceful exit after
// current transfers complete), and SIGUSR2 (progress summary dump).
// On non-Unix platforms all operations are no-ops. Mirrors upstream rsync
// daemon signal handling (upstream: main.c, clientserver.c).

/// Shared atomic flags checked by the server accept loop.
///
/// On Unix, signal handlers set these flags asynchronously. On non-Unix
/// platforms the flags exist but are never set by signal handlers.
pub(crate) struct SignalFlags {
    /// Set when SIGHUP is received, indicating the daemon should reload
    /// its configuration file before accepting the next connection.
    pub(crate) reload_config: Arc<AtomicBool>,
    /// Set when SIGTERM or SIGINT is received, indicating the daemon
    /// should stop accepting new connections and drain existing workers.
    pub(crate) shutdown: Arc<AtomicBool>,
    /// Set when SIGUSR1 is received, indicating the daemon should finish
    /// serving current transfers and then exit cleanly.
    ///
    /// Unlike `shutdown` which stops accepting immediately, this flag allows
    /// in-progress transfers to complete before the daemon exits.
    /// upstream: main.c — SIGUSR1 handler sets `got_xfer_error = 1` and
    /// the daemon exits with code 19 (`RERR_SIGNAL1`) after draining.
    pub(crate) graceful_exit: Arc<AtomicBool>,
    /// Set when SIGUSR2 is received, indicating the daemon should log a
    /// progress summary of active connections.
    ///
    /// The flag is consumed (reset to `false`) after each summary dump so
    /// repeated SIGUSR2 signals each produce a new snapshot.
    /// upstream: main.c — SIGUSR2 outputs transfer statistics.
    pub(crate) progress_dump: Arc<AtomicBool>,
}

impl SignalFlags {
    /// Creates a new set of signal flags with all flags initially unset.
    fn new() -> Self {
        Self {
            reload_config: Arc::new(AtomicBool::new(false)),
            shutdown: Arc::new(AtomicBool::new(false)),
            graceful_exit: Arc::new(AtomicBool::new(false)),
            progress_dump: Arc::new(AtomicBool::new(false)),
        }
    }
}

/// Registers Unix signal handlers and returns the shared flags.
///
/// - **SIGPIPE** is captured into a never-checked flag so the default
///   termination action is replaced with a no-op. Writes to closed client
///   sockets then surface as `EPIPE` I/O errors instead of killing the
///   daemon process.
/// - **SIGHUP** sets `reload_config` to `true`.
/// - **SIGTERM** and **SIGINT** set `shutdown` to `true`.
/// - **SIGUSR1** sets `graceful_exit` to `true`. The daemon stops accepting
///   new connections and exits cleanly once all active transfers finish.
/// - **SIGUSR2** sets `progress_dump` to `true`. The daemon logs a summary
///   of active connections on the next loop iteration.
///
/// On non-Unix platforms this is a no-op that returns default (never-set) flags.
///
/// # Errors
///
/// Returns an I/O error if signal registration fails on Unix.
#[cfg(unix)]
pub(crate) fn register_signal_handlers() -> io::Result<SignalFlags> {
    use signal_hook::consts::{SIGHUP, SIGINT, SIGPIPE, SIGTERM, SIGUSR1, SIGUSR2};

    let flags = SignalFlags::new();

    // Ignore SIGPIPE by installing a handler that sets a flag we never read.
    // This replaces the default termination action so broken-pipe errors
    // surface as io::Error instead of killing the process.
    // upstream: main.c SIGACT(SIGPIPE, SIG_IGN)
    let sigpipe_sink = Arc::new(AtomicBool::new(false));
    signal_hook::flag::register(SIGPIPE, Arc::clone(&sigpipe_sink))?;

    // SIGHUP triggers a config reload on the next loop iteration.
    signal_hook::flag::register(SIGHUP, Arc::clone(&flags.reload_config))?;

    // SIGTERM and SIGINT trigger graceful shutdown.
    signal_hook::flag::register(SIGTERM, Arc::clone(&flags.shutdown))?;
    signal_hook::flag::register(SIGINT, Arc::clone(&flags.shutdown))?;

    // SIGUSR1 triggers graceful exit: stop accepting new connections, drain
    // active transfers, then exit with code 19 (RERR_SIGNAL1).
    // upstream: main.c — rsync_panic_handler catches SIGUSR1 and sets
    // got_xfer_error, causing exit after current transfer completes.
    signal_hook::flag::register(SIGUSR1, Arc::clone(&flags.graceful_exit))?;

    // SIGUSR2 triggers a progress summary dump: log active connection count
    // and transfer statistics.
    // upstream: main.c — SIGUSR2 handler outputs transfer progress info.
    signal_hook::flag::register(SIGUSR2, Arc::clone(&flags.progress_dump))?;

    Ok(flags)
}

/// No-op signal registration for non-Unix platforms.
///
/// Returns default flags that are never set by signal handlers.
#[cfg(not(unix))]
pub(crate) fn register_signal_handlers() -> io::Result<SignalFlags> {
    Ok(SignalFlags::new())
}

#[cfg(test)]
mod signal_tests {
    use super::*;

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
}
