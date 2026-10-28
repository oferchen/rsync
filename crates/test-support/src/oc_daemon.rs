//! The oc-rsync daemon a client test talks to.
//!
//! [`OcDaemon`] binds `127.0.0.1:0` itself and forks the daemon library onto
//! that listener through [`ForkedServer`], so a client connect queues in the
//! kernel backlog from the moment the port is known: there is no free-port
//! probe, no readiness wait and no retry. Nothing is executed, so a cold
//! page-in of the debug binary never sits between a test and its daemon.
//!
//! The daemon parses its argument vector and `rsyncd.conf` exactly as
//! `oc-rsync --daemon` would; only the TCP listener is handed over. `--port`
//! is set to the bound port, so everything derived from it - the `listening on
//! port` log line - agrees with the listener. A daemon built with
//! [`OcDaemonBuilder::quic`] gets a pre-bound UDP socket the same way, so the
//! QUIC port is owned before the daemon starts too.

use std::ffi::OsString;
use std::io;
use std::net::{Ipv4Addr, TcpListener, UdpSocket};
use std::path::{Path, PathBuf};

use crate::forked_server::{ForkedServer, RunCrash, RunFailure, RunResult};

/// A running oc-rsync daemon on a loopback port.
///
/// Dropping it kills the daemon and every session it forked.
#[derive(Debug)]
pub struct OcDaemon {
    server: ForkedServer,
    port: u16,
    quic_port: Option<u16>,
}

/// Configures an [`OcDaemon`] before it starts.
#[derive(Debug)]
pub struct OcDaemonBuilder {
    arguments: Vec<OsString>,
    env: Vec<(&'static str, OsString)>,
    current_dir: Option<PathBuf>,
    quic: bool,
}

impl OcDaemon {
    /// A daemon serving `config`, as `oc-rsync --daemon --no-detach --config
    /// <config>` would.
    #[must_use]
    pub fn builder(config: impl AsRef<Path>) -> OcDaemonBuilder {
        OcDaemonBuilder {
            arguments: vec![
                OsString::from("--config"),
                config.as_ref().as_os_str().to_owned(),
            ],
            env: Vec::new(),
            current_dir: None,
            quic: false,
        }
    }

    /// The loopback port the daemon serves.
    #[must_use]
    pub fn port(&self) -> u16 {
        self.port
    }

    /// The loopback UDP port the daemon serves QUIC on.
    ///
    /// # Panics
    ///
    /// When the daemon was not built with [`OcDaemonBuilder::quic`].
    #[must_use]
    pub fn quic_port(&self) -> u16 {
        self.quic_port
            .expect("quic_port() needs a daemon built with OcDaemonBuilder::quic()")
    }

    /// Pid of the daemon process.
    #[must_use]
    pub fn pid(&self) -> u32 {
        self.server.pid()
    }

    /// Asks the daemon to shut down, as SIGTERM does.
    pub fn terminate(&self) {
        self.server.terminate();
    }

    /// Blocks until the daemon ends, and reports what `run_daemon` returned.
    ///
    /// # Errors
    ///
    /// [`RunCrash`] when the daemon panicked or ended without returning.
    pub fn join(self) -> Result<RunResult, RunCrash> {
        self.server.join()
    }
}

impl OcDaemonBuilder {
    /// Appends a daemon argument, as it would follow `--config` on the
    /// command line.
    #[must_use]
    pub fn arg(mut self, arg: impl Into<OsString>) -> Self {
        self.arguments.push(arg.into());
        self
    }

    /// Sets an environment variable in the daemon process only.
    #[must_use]
    pub fn env(mut self, key: &'static str, value: impl Into<OsString>) -> Self {
        self.env.push((key, value.into()));
        self
    }

    /// Runs the daemon from `dir`, as `Command::current_dir` would.
    #[must_use]
    pub fn current_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.current_dir = Some(dir.into());
        self
    }

    /// Hands the daemon a pre-bound UDP socket on `127.0.0.1` for its QUIC
    /// listener, which then binds none of its own; see
    /// `DaemonConfigBuilder::pre_bound_quic_socket`. The config must still
    /// enable QUIC (`quic cert file` and `quic key file`).
    #[must_use]
    pub fn quic(mut self) -> Self {
        self.quic = true;
        self
    }

    /// Binds the loopback listener and forks the daemon onto it.
    ///
    /// # Errors
    ///
    /// When the listener cannot be bound. A daemon that refuses its
    /// configuration is reported by [`OcDaemon::join`], and a client sees its
    /// connection closed.
    pub fn spawn(self) -> io::Result<OcDaemon> {
        let (ports, run) = self.bind()?;
        Ok(OcDaemon {
            server: ForkedServer::spawn(run),
            port: ports.tcp,
            quic_port: ports.quic,
        })
    }

    /// [`spawn`](Self::spawn), with the daemon's standard error readable from
    /// the returned pipe; see [`ForkedServer::spawn_capturing_stderr`].
    ///
    /// # Errors
    ///
    /// As [`spawn`](Self::spawn).
    pub fn spawn_capturing_stderr(self) -> io::Result<(OcDaemon, std::io::PipeReader)> {
        let (ports, run) = self.bind()?;
        let (server, stderr) = ForkedServer::spawn_capturing_stderr(run);
        let daemon = OcDaemon {
            server,
            port: ports.tcp,
            quic_port: ports.quic,
        };
        Ok((daemon, stderr))
    }

    fn bind(self) -> io::Result<(Ports, impl FnOnce() -> RunResult)> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        let port = listener.local_addr()?.port();
        let quic_socket = if self.quic {
            Some(UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))?)
        } else {
            None
        };
        let ports = Ports {
            tcp: port,
            quic: quic_socket
                .as_ref()
                .map(UdpSocket::local_addr)
                .transpose()?
                .map(|addr| addr.port()),
        };
        let mut arguments = vec![
            OsString::from("--no-detach"),
            OsString::from("--port"),
            OsString::from(port.to_string()),
        ];
        arguments.extend(self.arguments);
        let mut config = daemon::DaemonConfig::builder()
            .arguments(arguments)
            .pre_bound_listener(listener);
        if let Some(socket) = quic_socket {
            config = config.pre_bound_quic_socket(socket);
        }
        let config = config.build();
        let (env, current_dir) = (self.env, self.current_dir);
        Ok((ports, move || {
            // The forked daemon is single-threaded, so setting its
            // environment cannot race another thread's read.
            let _env: Vec<platform::env::EnvGuard> = env
                .iter()
                .map(|(key, value)| platform::env::EnvGuard::set(key, value))
                .collect();
            if let Some(dir) = current_dir {
                std::env::set_current_dir(&dir).expect("enter the daemon's working directory");
            }
            daemon::run_daemon(config).map_err(|error| RunFailure {
                exit_code: error.exit_code(),
                message: error.message().to_string(),
            })
        }))
    }
}

/// The ports a spawned daemon serves, known before it starts.
struct Ports {
    tcp: u16,
    quic: Option<u16>,
}
