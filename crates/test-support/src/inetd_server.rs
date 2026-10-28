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
use std::net::{TcpListener, TcpStream};
use std::os::fd::OwnedFd;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;

/// The command each accepted connection runs.
#[derive(Debug)]
pub struct InetdCommand {
    binary: PathBuf,
    arguments: Vec<OsString>,
    stderr: File,
}

impl InetdCommand {
    /// `binary --daemon`, with every session's stderr appended to `stderr`.
    #[must_use]
    pub fn new(binary: impl Into<PathBuf>, stderr: File) -> Self {
        Self {
            binary: binary.into(),
            arguments: Vec::new(),
            stderr,
        }
    }

    /// Appends a daemon argument.
    #[must_use]
    pub fn arg(mut self, arg: impl Into<OsString>) -> Self {
        self.arguments.push(arg.into());
        self
    }

    fn spawn(&self, stream: TcpStream) -> io::Result<Child> {
        let stdout = OwnedFd::from(stream.try_clone()?);
        Command::new(&self.binary)
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

    /// The port the listener serves.
    #[must_use]
    pub fn port(&self) -> u16 {
        self.port
    }
}

impl Drop for InetdServer {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::Release);
        // Wakes the blocked accept so the thread sees the flag and drops the
        // listener; a later connect is then refused.
        let _ = TcpStream::connect(("127.0.0.1", self.port));
        if let Some(acceptor) = self.acceptor.take() {
            let _ = acceptor.join();
        }
        for mut child in lock(&self.sessions).drain(..) {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
