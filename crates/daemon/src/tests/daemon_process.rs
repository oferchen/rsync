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
use std::io;
use std::net::{SocketAddr, TcpListener};
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

/// Binds a listening socket for a test daemon at `addr`, leaving
/// `SO_REUSEADDR` off.
///
/// `TcpListener::bind` sets `SO_REUSEADDR` on Unix. On macOS, as on the other
/// BSDs, that flag limits the bind-time conflict check to an exact address
/// match, so `127.0.0.1:0` can be given a port another test's daemon already
/// listens on at the wildcard (`0.0.0.0` or `[::]`), which that daemon binds
/// with `SO_REUSEADDR` as upstream does (socket.c:605-606). A connection to
/// `127.0.0.1` then reaches the more specific socket: the other test's client
/// lands in this test's backlog, a `--once` daemon serves it and exits, and
/// this test's own queued connection is reset before the greeting. Without
/// the flag the kernel counts wildcard holders as conflicts and picks another
/// port. Linux refuses that co-bind with or without the flag.
pub(super) fn bind_test_listener(addr: SocketAddr) -> io::Result<TcpListener> {
    use socket2::{Domain, Protocol, Socket, Type};

    let socket = Socket::new(Domain::for_address(addr), Type::STREAM, Some(Protocol::TCP))?;
    socket.bind(&addr.into())?;
    socket.listen(128)?;
    Ok(socket.into())
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
/// Another daemon's wildcard listener must keep a test listener off its port,
/// or that daemon's clients connect to the test's daemon instead.
#[cfg(unix)]
#[test]
fn test_listener_refuses_a_port_another_daemon_holds_on_the_wildcard() {
    use socket2::{Domain, Protocol, Socket, Type};
    use std::net::Ipv4Addr;

    // The other daemon's listener, set up as upstream socket.c:605-606 does.
    let daemon = Socket::new(Domain::IPV4, Type::STREAM, Some(Protocol::TCP)).expect("socket");
    daemon.set_reuse_address(true).expect("SO_REUSEADDR");
    daemon
        .bind(&SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0)).into())
        .expect("bind wildcard");
    daemon.listen(5).expect("listen");
    let port = daemon
        .local_addr()
        .expect("local addr")
        .as_socket()
        .expect("inet addr")
        .port();

    let error = bind_test_listener(SocketAddr::from((Ipv4Addr::LOCALHOST, port)))
        .expect_err("a loopback test listener must not share a port held on the wildcard");
    assert_eq!(error.kind(), io::ErrorKind::AddrInUse);
}
