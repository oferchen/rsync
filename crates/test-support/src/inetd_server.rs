//! An rsync daemon binary served in inetd mode on a listener the test binds.
//!
//! A binary cannot take a pre-bound listener, so each accepted connection
//! starts `<binary> --daemon <args>` with the socket as its stdin and stdout,
//! which upstream serves as one session. upstream: clientserver.c:1748
//! `is_inetd_socket(STDIN_FILENO)`.
//!
//! The listener exists before any daemon runs, so a client connect queues in
//! the kernel backlog: there is no port race, no readiness wait and no restart
//! loop. A slow daemon start - a cold exec on a loaded host - only delays the
//! client's first read. This is the arm for upstream binaries; the oc-rsync
//! daemon runs as a library on a pre-bound listener instead.

use std::ffi::OsString;
use std::fs::File;
use std::io;
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::os::fd::OwnedFd;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;

use crate::forked_server::bind_test_listener;

/// The command each accepted connection runs.
#[derive(Debug)]
pub struct InetdCommand {
    binary: PathBuf,
    wrapper: Vec<OsString>,
    arguments: Vec<OsString>,
    env: Vec<(OsString, OsString)>,
    stderr: File,
}

impl InetdCommand {
    /// `binary --daemon`, with every session's stderr appended to `stderr`.
    #[must_use]
    pub fn new(binary: impl Into<PathBuf>, stderr: File) -> Self {
        Self {
            binary: binary.into(),
            wrapper: Vec::new(),
            arguments: Vec::new(),
            env: Vec::new(),
            stderr,
        }
    }

    /// `binary --daemon`, with every session's stderr discarded.
    ///
    /// # Errors
    ///
    /// When `/dev/null` cannot be opened.
    pub fn discarding_stderr(binary: impl Into<PathBuf>) -> io::Result<Self> {
        let null = std::fs::OpenOptions::new().write(true).open("/dev/null")?;
        Ok(Self::new(binary, null))
    }

    /// Runs each session as `wrapper... binary --daemon <args>`, such as under
    /// `sudo -n` for a daemon that must start as root.
    #[must_use]
    pub fn wrapped_by<I, S>(mut self, wrapper: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        self.wrapper = wrapper.into_iter().map(Into::into).collect();
        self
    }

    /// Sets an environment variable in every session.
    #[must_use]
    pub fn env(mut self, key: impl Into<OsString>, value: impl Into<OsString>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    /// Appends a daemon argument.
    #[must_use]
    pub fn arg(mut self, arg: impl Into<OsString>) -> Self {
        self.arguments.push(arg.into());
        self
    }

    fn spawn(&self, stream: TcpStream) -> io::Result<Child> {
        let stdout = OwnedFd::from(stream.try_clone()?);
        let mut command = match self.wrapper.split_first() {
            Some((program, rest)) => {
                let mut command = Command::new(program);
                command.args(rest).arg(&self.binary);
                command
            }
            None => Command::new(&self.binary),
        };
        command
            .envs(self.env.iter().map(|(key, value)| (key, value)))
            .arg("--daemon")
            .args(&self.arguments)
            .stdin(Stdio::from(OwnedFd::from(stream)))
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(self.stderr.try_clone()?))
            .spawn()
    }
}

/// The accept thread and the daemon sessions it started.
///
/// Dropping it stops accepting, after which a connect is refused, and kills
/// and reaps every session.
#[derive(Debug)]
pub struct InetdServer {
    port: u16,
    stopping: Arc<AtomicBool>,
    sessions: Arc<Mutex<Vec<Child>>>,
    acceptor: Option<thread::JoinHandle<()>>,
}

impl InetdServer {
    /// Serves `listener`, running `command` once per accepted connection.
    ///
    /// # Errors
    ///
    /// When the listener has no local address.
    pub fn start(listener: TcpListener, command: InetdCommand) -> io::Result<Self> {
        let port = listener.local_addr()?.port();
        let stopping = Arc::new(AtomicBool::new(false));
        let sessions = Arc::new(Mutex::new(Vec::new()));
        let acceptor = {
            let stopping = Arc::clone(&stopping);
            let sessions = Arc::clone(&sessions);
            thread::spawn(move || {
                for stream in listener.incoming() {
                    if stopping.load(Ordering::Acquire) {
                        return;
                    }
                    // A failed accept or spawn drops only that connection; the
                    // client then reads EOF and its test reports it.
                    let Ok(stream) = stream else { continue };
                    match command.spawn(stream) {
                        Ok(child) => lock(&sessions).push(child),
                        Err(error) => eprintln!("InetdServer: could not start a session: {error}"),
                    }
                }
            })
        };
        Ok(Self {
            port,
            stopping,
            sessions,
            acceptor: Some(acceptor),
        })
    }

    /// Serves the listener [`reserve_port`] bound for `port`.
    ///
    /// # Errors
    ///
    /// When no reserved listener holds `port`.
    pub fn start_reserved(port: u16, command: InetdCommand) -> io::Result<Self> {
        Self::start(take_reserved(port)?, command)
    }

    /// The port the listener serves.
    #[must_use]
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Stops accepting, then waits for every session to end on its own, so
    /// everything a session writes on its way out, such as its log lines, is
    /// on disk when this returns.
    ///
    /// # Errors
    ///
    /// When waiting on a session fails.
    pub fn finish(mut self) -> io::Result<()> {
        self.stop_accepting();
        for mut child in lock(&self.sessions).drain(..) {
            child.wait()?;
        }
        Ok(())
    }

    /// Stops the accept thread. Every session it started is then recorded.
    fn stop_accepting(&mut self) {
        self.stopping.store(true, Ordering::Release);
        // Wakes the blocked accept so the thread sees the flag and drops the
        // listener; a later connect is then refused.
        let _ = TcpStream::connect(("127.0.0.1", self.port));
        if let Some(acceptor) = self.acceptor.take() {
            let _ = acceptor.join();
        }
    }
}

impl Drop for InetdServer {
    fn drop(&mut self) {
        self.stop_accepting();
        for mut child in lock(&self.sessions).drain(..) {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Listeners bound by [`reserve_port`] and not yet served.
static RESERVED: Mutex<Vec<TcpListener>> = Mutex::new(Vec::new());

/// Binds a loopback listener and keeps it open until
/// [`InetdServer::start_reserved`] serves it, so a test can write the port into
/// a config before the daemon exists without ever releasing the port.
///
/// # Errors
///
/// When binding the listener fails.
pub fn reserve_port() -> io::Result<u16> {
    let listener = bind_test_listener(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))?;
    let port = listener.local_addr()?.port();
    lock(&RESERVED).push(listener);
    Ok(port)
}

/// Takes the listener [`reserve_port`] bound for `port`.
pub(crate) fn take_reserved(port: u16) -> io::Result<TcpListener> {
    let mut reserved = lock(&RESERVED);
    let index = reserved
        .iter()
        .position(|listener| listener.local_addr().is_ok_and(|addr| addr.port() == port))
        .ok_or_else(|| io::Error::other(format!("no listener reserved for port {port}")))?;
    Ok(reserved.swap_remove(index))
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
