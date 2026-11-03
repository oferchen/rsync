//! The daemon a test drives runs as a process of its own, through the shared
//! `test_support::forked_server` harness; see that module for why.
//!
//! This adapter only names the daemon API: it runs `run_daemon(config)` and
//! reduces a `DaemonError` to what crosses the process boundary.
//!
//! The parent module must have `DaemonConfig`, `DaemonConfigBuilder`,
//! `DaemonError` and `run_daemon` in scope: the unit tests include this module
//! directly and the integration tests through `#[path]`, so it names the
//! daemon API through `super::` rather than through either crate path.

#[cfg(not(unix))]
use super::DaemonConfigBuilder;
use super::{DaemonConfig, DaemonError, run_daemon};
use test_support::forked_server::ForkedServer;
pub(super) use test_support::forked_server::{
    RunCrash as DaemonCrash, RunFailure as DaemonFailure,
};

/// What `run_daemon` returned, carried out of the daemon process.
pub(super) type DaemonResult = Result<(), DaemonFailure>;

/// Reduces a returned `DaemonError` to what survives a process boundary.
fn failure(error: DaemonError) -> DaemonFailure {
    DaemonFailure {
        exit_code: error.exit_code(),
        message: error.message().to_string(),
    }
}

/// A running test daemon.
pub(super) struct DaemonHandle {
    server: ForkedServer,
    #[cfg(not(unix))]
    signals: platform::signal::SignalFlags,
}

impl DaemonHandle {
    /// Starts a process that runs `run_daemon(config)` and records its end.
    ///
    /// Call it before opening any client socket: whatever the test process
    /// holds at this point, the daemon and all its sessions inherit.
    #[cfg(unix)]
    pub(super) fn spawn(config: DaemonConfig) -> Self {
        Self {
            server: ForkedServer::spawn(move || run_daemon(config).map_err(failure)),
        }
    }

    /// Runs `run_daemon(config)` on a thread, with signal flags the handle can
    /// set in place of a signal.
    #[cfg(not(unix))]
    pub(super) fn spawn(config: DaemonConfig) -> Self {
        let signals = platform::signal::SignalFlags::new();
        let config = DaemonConfigBuilder::from(config)
            .signal_flags(signals.clone())
            .build();
        Self {
            server: ForkedServer::spawn(move || run_daemon(config).map_err(failure)),
            signals,
        }
    }

    /// Pid of the daemon process; only the Linux `/proc` fd audit needs it.
    #[cfg(target_os = "linux")]
    pub(super) fn pid(&self) -> u32 {
        self.server.pid()
    }

    /// Asks the daemon to shut down, as SIGTERM or a console CTRL_C does.
    #[allow(
        dead_code,
        reason = "only the integration-test binaries stop a daemon by signal"
    )]
    pub(super) fn terminate(&self) {
        #[cfg(unix)]
        self.server.terminate();
        #[cfg(not(unix))]
        self.signals
            .shutdown
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }

    /// Whether the daemon thread has returned.
    #[cfg(not(unix))]
    pub(super) fn is_finished(&self) -> bool {
        self.server.is_finished()
    }

    /// Blocks until the daemon ends, and reports what it returned.
    pub(super) fn join(self) -> Result<DaemonResult, DaemonCrash> {
        self.server.join()
    }
}
