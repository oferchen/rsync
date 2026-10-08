//! An `oc-rsync --daemon` run as a separate process for daemon tests.
//!
//! # Why a process and not a thread
//!
//! The daemon forks a child per accepted connection (upstream
//! `socket.c:start_accept_loop()`). Run on a thread inside a test binary, that
//! fork copies a multithreaded process: the child inherits every lock another
//! test thread held at the instant of the fork, with no thread left to release
//! it, and can block on it forever. Upstream only ever forks from a
//! single-threaded daemon parent. Running the daemon as its own `oc-rsync`
//! process restores that invariant, so a daemon test exercises the production
//! fork rather than a hazard the production binary never meets.
//!
//! # What the harness owns
//!
//! - The binary, via [`crate::oc_rsync_bin`], which refuses a stale build.
//! - The port, via [`crate::spawn_daemon_on_free_port`], which confirms that
//!   *this* process is listening on it rather than trusting a bare connect.
//! - The daemon's lifetime, via [`ReapOnDrop`], so the process is killed and
//!   reaped on every exit path from the test, including a panic.
//! - Isolation from the host: an explicit `--config` suppresses the default
//!   config search, and the config and secrets override variables are removed
//!   from the child's environment.
//!
//! Every wait is bounded. Readiness is bounded by the port helper, client
//! reads and writes by [`CLIENT_IO_TIMEOUT`], and exit by the caller's budget
//! in [`DaemonProcess::wait_for_exit`].

use std::ffi::OsStr;
use std::io::{self, Read};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::{ReapOnDrop, oc_rsync_bin, spawn_daemon_on_free_port};

/// Read and write timeout applied to every client stream from [`DaemonProcess::connect`].
///
/// Generous against startup latency on a loaded runner, but finite, so a
/// wedged session fails the test instead of hanging it until the nextest
/// slow-timeout.
pub const CLIENT_IO_TIMEOUT: Duration = Duration::from_secs(30);

/// Interval between exit probes in [`DaemonProcess::wait_for_exit`].
const EXIT_POLL_INTERVAL: Duration = Duration::from_millis(10);

/// Environment variables that would point the daemon at host configuration.
///
/// `--config` already overrides the config variables; they are removed anyway
/// so a test never depends on argument precedence it does not state. The
/// secrets variables are consulted independently of `--config`.
const HOST_OVERRIDE_ENV: [&str; 4] = [
    "OC_RSYNC_CONFIG",
    "RSYNCD_CONFIG",
    "OC_RSYNC_SECRETS",
    "RSYNCD_SECRETS",
];

/// Environment that disables delegation to a system `rsync`, matching the
/// in-process daemon tests this harness replaces.
const NO_FALLBACK_ENV: [(&str, &str); 2] = [
    ("OC_RSYNC_DAEMON_FALLBACK", "0"),
    ("OC_RSYNC_FALLBACK", "0"),
];

/// A running `oc-rsync --daemon` process bound to a loopback port.
///
/// Dropping the value kills and reaps the process.
#[derive(Debug)]
pub struct DaemonProcess {
    child: ReapOnDrop,
    port: u16,
    stderr: Arc<Mutex<Vec<u8>>>,
    stderr_drain: Option<JoinHandle<()>>,
}

impl DaemonProcess {
    /// Starts `oc-rsync --daemon --no-detach --config <config> --port <p>` with
    /// `args` appended, and returns once the process is listening on `p`.
    ///
    /// `--no-detach` is always passed: a detaching daemon forks and its parent
    /// exits, leaving an unreaped grandchild this harness cannot own.
    ///
    /// # Errors
    ///
    /// Returns an error if the process cannot be spawned, exits before it
    /// binds, or does not bind within the port helper's deadline. The error
    /// carries whatever the daemon wrote to stderr.
    ///
    /// # Panics
    ///
    /// Panics if `args` contains `--detach`, and if the `oc-rsync` binary is
    /// missing or stale (see [`crate::workspace_bin`]).
    pub fn spawn<I, S>(config: &Path, args: I) -> io::Result<Self>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let args: Vec<_> = args.into_iter().map(|a| a.as_ref().to_owned()).collect();
        assert!(
            !args.iter().any(|a| a == "--detach"),
            "a detached daemon outlives its test; the harness always runs --no-detach"
        );
        let bin = oc_rsync_bin();
        let stderr = Arc::new(Mutex::new(Vec::new()));
        let mut stderr_drain = None;

        let spawned = spawn_daemon_on_free_port(|port| {
            let mut command = Command::new(&bin);
            command
                .arg("--daemon")
                .arg("--no-detach")
                .arg("--config")
                .arg(config)
                .arg("--port")
                .arg(port.to_string())
                .args(&args)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::piped());
            for name in HOST_OVERRIDE_ENV {
                command.env_remove(name);
            }
            command.envs(NO_FALLBACK_ENV);
            let mut child = command.spawn()?;
            // Every attempt drains into the same buffer, so a daemon that lost a
            // port race and was retried still leaves its diagnostic behind.
            if let Some(pipe) = child.stderr.take() {
                stderr_drain = Some(drain(pipe, Arc::clone(&stderr)));
            }
            Ok(child)
        });

        match spawned {
            Ok((child, port)) => Ok(Self {
                child: ReapOnDrop::new(child),
                port,
                stderr,
                stderr_drain,
            }),
            Err(error) => {
                if let Some(handle) = stderr_drain {
                    let _ = handle.join();
                }
                let text = String::from_utf8_lossy(&lock(&stderr)).into_owned();
                Err(io::Error::new(
                    error.kind(),
                    format!("{error}; daemon stderr: {text:?}"),
                ))
            }
        }
    }

    /// The loopback port the daemon is listening on.
    #[must_use]
    pub fn port(&self) -> u16 {
        self.port
    }

    /// The daemon's process id.
    #[must_use]
    pub fn id(&self) -> u32 {
        self.child.id()
    }

    /// Opens a client connection to the daemon with [`CLIENT_IO_TIMEOUT`]
    /// applied to reads and writes.
    ///
    /// # Errors
    ///
    /// Returns the connect or socket-option error.
    pub fn connect(&self) -> io::Result<TcpStream> {
        let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, self.port));
        let stream = TcpStream::connect_timeout(&addr, CLIENT_IO_TIMEOUT)?;
        stream.set_read_timeout(Some(CLIENT_IO_TIMEOUT))?;
        stream.set_write_timeout(Some(CLIENT_IO_TIMEOUT))?;
        Ok(stream)
    }

    /// Everything the daemon has written to stderr so far.
    #[must_use]
    pub fn stderr(&self) -> Vec<u8> {
        lock(&self.stderr).clone()
    }

    /// Waits up to `budget` for the daemon to exit on its own, as it does after
    /// `--once` or a session cap, and returns its status.
    ///
    /// # Errors
    ///
    /// Returns [`io::ErrorKind::TimedOut`] if the daemon is still running when
    /// the budget expires; the daemon is then killed and reaped by `Drop`.
    pub fn wait_for_exit(mut self, budget: Duration) -> io::Result<ExitStatus> {
        let deadline = Instant::now() + budget;
        loop {
            if let Some(status) = self.child.try_wait()? {
                if let Some(handle) = self.stderr_drain.take() {
                    let _ = handle.join();
                }
                return Ok(status);
            }
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!(
                        "daemon pid {} still running after {budget:?}; stderr: {:?}",
                        self.id(),
                        String::from_utf8_lossy(&self.stderr())
                    ),
                ));
            }
            thread::sleep(EXIT_POLL_INTERVAL);
        }
    }
}

/// Copies `pipe` into `sink` until EOF, so the daemon never blocks on a full
/// stderr pipe.
fn drain(mut pipe: impl Read + Send + 'static, sink: Arc<Mutex<Vec<u8>>>) -> JoinHandle<()> {
    thread::spawn(move || {
        let mut buf = [0u8; 4096];
        while let Ok(n) = pipe.read(&mut buf) {
            if n == 0 {
                break;
            }
            lock(&sink).extend_from_slice(&buf[..n]);
        }
    })
}

/// Poison-tolerant lock: a panicking reader must not hide the daemon's output.
fn lock(buffer: &Mutex<Vec<u8>>) -> std::sync::MutexGuard<'_, Vec<u8>> {
    buffer
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
