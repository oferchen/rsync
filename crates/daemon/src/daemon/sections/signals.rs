/// Shared atomic flags checked by the server accept loop.
///
/// Thin wrapper around `platform::signal::SignalFlags` that provides
/// `pub(crate)` visibility within the daemon crate.
///
/// upstream: main.c - signal handler setup for SIGPIPE, SIGHUP, SIGTERM,
/// SIGUSR1, SIGUSR2.
pub(crate) struct SignalFlags {
    /// Reload configuration (SIGHUP on Unix).
    pub(crate) reload_config: Arc<AtomicBool>,
    /// Stop accepting connections and drain workers (SIGTERM/SIGINT on Unix,
    /// CTRL_C/CTRL_CLOSE on Windows).
    pub(crate) shutdown: Arc<AtomicBool>,
    /// Finish current transfers then exit with code 19 (`RERR_SIGNAL1`).
    /// Unlike `shutdown`, allows in-progress transfers to complete.
    /// upstream: main.c - SIGUSR1 sets `got_xfer_error = 1`.
    pub(crate) graceful_exit: Arc<AtomicBool>,
    /// Log a progress summary of active connections (SIGUSR2 on Unix).
    /// Consumed (reset to `false`) after each dump.
    pub(crate) progress_dump: Arc<AtomicBool>,
}

impl SignalFlags {
    /// Creates a new set of signal flags with all flags initially unset.
    #[cfg(test)]
    fn new() -> Self {
        Self {
            reload_config: Arc::new(AtomicBool::new(false)),
            shutdown: Arc::new(AtomicBool::new(false)),
            graceful_exit: Arc::new(AtomicBool::new(false)),
            progress_dump: Arc::new(AtomicBool::new(false)),
        }
    }
}

impl From<platform::signal::SignalFlags> for SignalFlags {
    fn from(pf: platform::signal::SignalFlags) -> Self {
        Self {
            reload_config: pf.reload_config,
            shutdown: pf.shutdown,
            graceful_exit: pf.graceful_exit,
            progress_dump: pf.progress_dump,
        }
    }
}

/// Registers platform signal handlers and returns shared flags.
///
/// Delegates to `platform::signal::register_signal_handlers()`.
///
/// upstream: main.c - SIGACT() calls for SIGPIPE, SIGHUP, SIGTERM, SIGUSR1, SIGUSR2.
pub(crate) fn register_signal_handlers() -> io::Result<SignalFlags> {
    platform::signal::register_signal_handlers().map(SignalFlags::from)
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
