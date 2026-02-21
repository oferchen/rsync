// Daemon signal handling.
//
// Registers handlers for SIGPIPE (ignore), SIGHUP (config reload flag),
// and SIGTERM/SIGINT (graceful shutdown flag). On non-Unix platforms all
// operations are no-ops. Mirrors upstream rsync daemon signal handling
// (upstream: main.c, clientserver.c).

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
}

impl SignalFlags {
    /// Creates a new set of signal flags with all flags initially unset.
    fn new() -> Self {
        Self {
            reload_config: Arc::new(AtomicBool::new(false)),
            shutdown: Arc::new(AtomicBool::new(false)),
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
///
/// On non-Unix platforms this is a no-op that returns default (never-set) flags.
///
/// # Errors
///
/// Returns an I/O error if signal registration fails on Unix.
#[cfg(unix)]
pub(crate) fn register_signal_handlers() -> io::Result<SignalFlags> {
    use signal_hook::consts::{SIGHUP, SIGINT, SIGPIPE, SIGTERM};

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
    }

    #[test]
    fn signal_flags_can_be_set() {
        let flags = SignalFlags::new();
        flags.reload_config.store(true, Ordering::Relaxed);
        assert!(flags.reload_config.load(Ordering::Relaxed));
        flags.shutdown.store(true, Ordering::Relaxed);
        assert!(flags.shutdown.load(Ordering::Relaxed));
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
    }
}
