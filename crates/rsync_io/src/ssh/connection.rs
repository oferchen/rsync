//! SSH connection management with split read/write support.
//!
//! This module provides [`SshConnection`] for managing SSH subprocess I/O,
//! with support for splitting into separate read/write halves for bidirectional
//! protocol communication. A background thread drains stderr from the child
//! process to prevent pipe-buffer deadlocks when the remote rsync writes error
//! messages.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, ExitStatus};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use logging::debug_log;

/// Maximum bytes retained in the stderr capture buffer.
///
/// When the buffer exceeds this limit, the oldest bytes are discarded to keep
/// memory usage bounded. 64 KB matches the typical OS pipe buffer size and is
/// sufficient to capture the tail of any error output from the remote process.
const STDERR_BUFFER_CAP: usize = 64 * 1024;

/// Owns an active SSH subprocess and exposes its stdio handles.
///
/// The connection can be used directly via the [`Read`] and [`Write`] traits,
/// or split into separate [`SshReader`] and [`SshWriter`] halves using [`split`](Self::split).
///
/// When stderr is available, a background thread is spawned at construction
/// time to drain it. This prevents deadlocks when the remote process writes
/// more than the OS pipe buffer capacity to stderr. The collected output is
/// retrievable via [`stderr_output`](Self::stderr_output).
pub struct SshConnection {
    /// Child process handle. Option allows safe extraction in split() without unsafe code.
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    stdout: Option<ChildStdout>,
    stderr_drain: Option<StderrDrain>,
    connect_watchdog: Option<ConnectWatchdog>,
}

impl SshConnection {
    /// Constructs a new connection from the spawned child process.
    ///
    /// If `stderr` is `Some`, a background thread is spawned immediately to
    /// drain it, preventing pipe-buffer deadlocks. If `connect_timeout` is
    /// `Some`, a watchdog thread is armed that will kill the child process
    /// if the connection is not established within the given duration. Call
    /// [`cancel_connect_watchdog`](Self::cancel_connect_watchdog) after
    /// the remote rsync version greeting is received to disarm it.
    pub(super) fn new(
        child: Child,
        stdin: Option<ChildStdin>,
        stdout: ChildStdout,
        stderr: Option<ChildStderr>,
        connect_timeout: Option<Duration>,
    ) -> Self {
        let stderr_drain = stderr.map(StderrDrain::spawn);
        let connect_watchdog = connect_timeout.map(|timeout| {
            let pid = child.id();
            ConnectWatchdog::arm(timeout, pid)
        });
        Self {
            child: Some(child),
            stdin,
            stdout: Some(stdout),
            stderr_drain,
            connect_watchdog,
        }
    }

    /// Disarms the connection establishment watchdog.
    ///
    /// Call this after the SSH connection is confirmed as established (e.g.,
    /// after receiving the remote rsync version greeting). If the watchdog has
    /// already fired, this returns an error indicating the timeout expired.
    /// If no watchdog was armed, this is a no-op.
    pub fn cancel_connect_watchdog(&mut self) -> io::Result<()> {
        if let Some(watchdog) = self.connect_watchdog.take() {
            watchdog.cancel()
        } else {
            Ok(())
        }
    }

    /// Returns the stderr output collected so far by the background drain thread.
    ///
    /// The returned bytes are bounded to the most recent [`STDERR_BUFFER_CAP`]
    /// bytes. Returns an empty `Vec` if no stderr handle was available.
    #[must_use]
    pub fn stderr_output(&self) -> Vec<u8> {
        self.stderr_drain
            .as_ref()
            .map_or_else(Vec::new, StderrDrain::collected)
    }

    /// Flushes and closes the stdin pipe, signalling EOF to the subprocess.
    pub fn close_stdin(&mut self) -> io::Result<()> {
        if let Some(mut stdin) = self.stdin.take() {
            stdin.flush()?;
        }
        Ok(())
    }

    /// Waits for the subprocess to exit, consuming the connection.
    pub fn wait(mut self) -> io::Result<ExitStatus> {
        let _ = self.close_stdin();
        match self.child.take() {
            Some(mut child) => {
                let status = child.wait();
                if let Some(ref mut drain) = self.stderr_drain {
                    drain.join();
                }
                status
            }
            None => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "child process already taken",
            )),
        }
    }

    /// Waits for the subprocess to exit and returns the collected stderr output.
    ///
    /// This combines [`wait`](Self::wait) and [`stderr_output`](Self::stderr_output)
    /// into a single call, ensuring the drain thread is joined and all stderr is
    /// captured before the connection is consumed. This is the preferred method
    /// when callers need to surface SSH error messages to the user on failure.
    pub fn wait_with_stderr(mut self) -> io::Result<(ExitStatus, Vec<u8>)> {
        let _ = self.close_stdin();
        match self.child.take() {
            Some(mut child) => {
                let status = child.wait();
                if let Some(ref mut drain) = self.stderr_drain {
                    drain.join();
                }
                let stderr = self
                    .stderr_drain
                    .as_ref()
                    .map_or_else(Vec::new, StderrDrain::collected);
                status.map(|s| (s, stderr))
            }
            None => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "child process already taken",
            )),
        }
    }

    /// Attempts to retrieve the subprocess exit status without blocking.
    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        match self.child.as_mut() {
            Some(child) => child.try_wait(),
            None => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "child process already taken",
            )),
        }
    }

    /// Splits the connection into separate read and write halves for bidirectional I/O.
    ///
    /// This consumes the connection and returns:
    /// - A reader (stdout) for receiving data from the remote process
    /// - A writer (stdin) for sending data to the remote process
    /// - An owned handle for waiting on the child process
    ///
    /// The stderr drain thread (if running) is transferred to the child handle.
    ///
    /// # Returns
    ///
    /// Returns `(reader, writer, child_handle)` on success.
    /// Returns an error if stdin, stdout, or the child process has already been taken.
    pub fn split(mut self) -> io::Result<(SshReader, SshWriter, SshChildHandle)> {
        let stdin = self.stdin.take().ok_or_else(|| {
            io::Error::new(io::ErrorKind::BrokenPipe, "stdin has already been closed")
        })?;

        let stdout = self.stdout.take().ok_or_else(|| {
            io::Error::new(io::ErrorKind::BrokenPipe, "stdout has already been taken")
        })?;

        let child = self.child.take().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "child process already taken")
        })?;

        let stderr_drain = self.stderr_drain.take();
        let connect_watchdog = self.connect_watchdog.take();

        Ok((
            SshReader { stdout },
            SshWriter { stdin },
            SshChildHandle {
                child,
                stderr_drain,
                connect_watchdog,
            },
        ))
    }
}

/// Read half of an SSH connection (subprocess stdout).
#[derive(Debug)]
pub struct SshReader {
    stdout: ChildStdout,
}

impl Read for SshReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.stdout.read(buf)
    }
}

/// Write half of an SSH connection (subprocess stdin).
#[derive(Debug)]
pub struct SshWriter {
    stdin: ChildStdin,
}

impl Write for SshWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.stdin.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.stdin.flush()
    }
}

impl SshWriter {
    /// Flushes and closes the stdin pipe, signalling EOF to the subprocess.
    pub fn close(mut self) -> io::Result<()> {
        self.stdin.flush()
    }
}

/// Background thread that drains SSH subprocess stderr to prevent pipe deadlocks.
///
/// When an SSH child writes more than the OS pipe buffer capacity (typically
/// 64 KB) to stderr, it blocks until the buffer is drained. If nothing reads
/// stderr, the child stalls and the transfer deadlocks. This thread reads
/// stderr line-by-line using raw byte reads (tolerant of non-UTF-8 output),
/// forwards each line to the process stderr in real time (matching upstream
/// rsync's behavior of surfacing remote errors immediately), and collects
/// the output into a bounded buffer for programmatic retrieval via
/// [`SshConnection::stderr_output`] or [`SshChildHandle::stderr_output`].
struct StderrDrain {
    handle: Option<JoinHandle<()>>,
    buffer: Arc<Mutex<Vec<u8>>>,
}

impl StderrDrain {
    /// Spawns a background thread that drains `stderr` until EOF.
    fn spawn(stderr: ChildStderr) -> Self {
        let buffer = Arc::new(Mutex::new(Vec::new()));
        let thread_buffer = Arc::clone(&buffer);

        let handle = thread::Builder::new()
            .name("ssh-stderr-drain".into())
            .spawn(move || {
                Self::drain_loop(stderr, &thread_buffer);
            })
            .expect("failed to spawn ssh stderr drain thread");

        Self {
            handle: Some(handle),
            buffer,
        }
    }

    /// Reads stderr line-by-line, forwards to process stderr, and collects
    /// into the shared buffer (bounded to [`STDERR_BUFFER_CAP`]).
    ///
    /// Uses `read_until(b'\n')` instead of `lines()` to handle non-UTF-8
    /// output without prematurely terminating the drain. SSH or the remote
    /// process may emit binary data on stderr (e.g., locale-encoded error
    /// messages); dropping such lines would leave the pipe un-drained and
    /// risk the deadlock this thread exists to prevent.
    fn drain_loop(stderr: ChildStderr, buffer: &Mutex<Vec<u8>>) {
        let mut reader = BufReader::new(stderr);
        let mut line_buf = Vec::new();
        loop {
            line_buf.clear();
            match reader.read_until(b'\n', &mut line_buf) {
                Ok(0) => break, // EOF
                Ok(_) => {
                    // Forward the line to the local process stderr so the user
                    // sees SSH errors in real time - matching upstream rsync's
                    // behavior of surfacing remote errors immediately.
                    let text = String::from_utf8_lossy(&line_buf);
                    eprint!("{text}");
                    debug_log!(Connect, 3, "ssh stderr: {}", text.trim_end());
                    Self::append_bounded(buffer, &line_buf);
                }
                // I/O error (broken pipe, etc.) - child exited, stop draining.
                Err(_) => break,
            }
        }
    }

    /// Appends `data` to the shared buffer, discarding the oldest bytes when
    /// the total exceeds [`STDERR_BUFFER_CAP`].
    fn append_bounded(buffer: &Mutex<Vec<u8>>, data: &[u8]) {
        let Ok(mut buf) = buffer.lock() else {
            return;
        };
        buf.extend_from_slice(data);
        let len = buf.len();
        if len > STDERR_BUFFER_CAP {
            let excess = len - STDERR_BUFFER_CAP;
            buf.drain(..excess);
        }
    }

    /// Returns a snapshot of the collected stderr output.
    fn collected(&self) -> Vec<u8> {
        self.buffer
            .lock()
            .map_or_else(|_| Vec::new(), |buf| buf.clone())
    }

    /// Joins the drain thread, blocking until it finishes.
    fn join(&mut self) {
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }

    /// Joins the drain thread and prints collected stderr when `status`
    /// indicates a non-zero exit.
    ///
    /// This is the single implementation of the "surface stderr on error in
    /// Drop" logic used by both [`SshChildHandle::drop`] and
    /// [`SshConnection::drop`]. The drain thread is joined first so all
    /// output is available before the buffer is checked.
    fn join_and_surface_on_error(&mut self, status: &io::Result<ExitStatus>) {
        self.join();

        if let Ok(exit) = status {
            if !exit.success() {
                let stderr = self.collected();
                if !stderr.is_empty() {
                    let text = String::from_utf8_lossy(&stderr);
                    let trimmed = text.trim();
                    if !trimmed.is_empty() {
                        eprintln!("ssh process exited with status {exit}:\n{trimmed}");
                    }
                }
            }
        }
    }
}

impl Drop for StderrDrain {
    fn drop(&mut self) {
        self.join();
    }
}

/// Background watchdog that kills the SSH child process if the connection
/// is not established within a configurable timeout.
///
/// The watchdog thread sleeps on a condvar until either the timeout expires
/// or the watchdog is cancelled. If the timeout fires, the thread sets the
/// `fired` flag. The caller (or Drop) is responsible for killing the child
/// process when the watchdog fires - the watchdog itself only signals that
/// the timeout expired.
///
/// The condvar-based design avoids busy polling and allows the cancel path
/// to wake the thread immediately.
struct ConnectWatchdog {
    cancelled: Arc<AtomicBool>,
    fired: Arc<AtomicBool>,
    condvar_pair: Arc<(Mutex<bool>, Condvar)>,
    handle: Option<JoinHandle<()>>,
    timeout: Duration,
}

impl ConnectWatchdog {
    /// Arms a watchdog that will fire after `timeout`.
    fn arm(timeout: Duration, child_pid: u32) -> Self {
        let cancelled = Arc::new(AtomicBool::new(false));
        let fired = Arc::new(AtomicBool::new(false));
        let condvar_pair = Arc::new((Mutex::new(false), Condvar::new()));

        let thread_cancelled = Arc::clone(&cancelled);
        let thread_fired = Arc::clone(&fired);
        let thread_pair = Arc::clone(&condvar_pair);

        let handle = thread::Builder::new()
            .name("ssh-connect-watchdog".into())
            .spawn(move || {
                let (lock, cvar) = &*thread_pair;
                let guard = lock.lock().unwrap_or_else(|e| e.into_inner());
                // Wait until timeout or cancellation signal.
                let (_guard, result) = cvar
                    .wait_timeout_while(guard, timeout, |notified| !*notified)
                    .unwrap_or_else(|e| e.into_inner());

                // If we were cancelled, exit quietly.
                if thread_cancelled.load(Ordering::Acquire) {
                    return;
                }

                // Timeout expired - set the fired flag. The child will be killed
                // by the connection's Drop impl or when the caller checks the flag.
                if result.timed_out() {
                    thread_fired.store(true, Ordering::Release);
                    debug_log!(
                        Connect,
                        1,
                        "ssh connect watchdog: timeout after {timeout:?} for pid {child_pid}"
                    );
                }
            })
            .expect("failed to spawn ssh connect watchdog thread");

        Self {
            cancelled,
            fired,
            condvar_pair,
            handle: Some(handle),
            timeout,
        }
    }

    /// Returns `true` if the watchdog timeout has fired.
    fn has_fired(&self) -> bool {
        self.fired.load(Ordering::Acquire)
    }

    /// Cancels the watchdog, preventing it from firing.
    ///
    /// Returns `Ok(())` if the watchdog was successfully cancelled before it
    /// fired. Returns an `io::Error` with `ErrorKind::TimedOut` if the
    /// watchdog already fired (meaning the timeout expired).
    fn cancel(mut self) -> io::Result<()> {
        self.cancelled.store(true, Ordering::Release);

        // Wake the watchdog thread so it exits immediately.
        let (lock, cvar) = &*self.condvar_pair;
        if let Ok(mut notified) = lock.lock() {
            *notified = true;
            cvar.notify_one();
        }

        // Join the watchdog thread.
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }

        if self.fired.load(Ordering::Acquire) {
            Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!(
                    "ssh connection establishment timed out after {} seconds",
                    self.timeout.as_secs()
                ),
            ))
        } else {
            Ok(())
        }
    }
}

impl Drop for ConnectWatchdog {
    fn drop(&mut self) {
        // Signal cancellation so the thread exits if still running.
        self.cancelled.store(true, Ordering::Release);
        let (lock, cvar) = &*self.condvar_pair;
        if let Ok(mut notified) = lock.lock() {
            *notified = true;
            cvar.notify_one();
        }
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// Handle to wait for SSH subprocess completion.
///
/// When the connection is split, the stderr drain thread (spawned at connection
/// creation time) is transferred to this handle. The drain thread prevents
/// pipe-buffer deadlocks by continuously reading stderr and forwarding lines
/// to process stderr. Collected output is retrievable via
/// [`stderr_output`](Self::stderr_output).
pub struct SshChildHandle {
    child: Child,
    stderr_drain: Option<StderrDrain>,
    connect_watchdog: Option<ConnectWatchdog>,
}

impl std::fmt::Debug for SshChildHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SshChildHandle")
            .field("child", &self.child)
            .field(
                "stderr_drain",
                &self.stderr_drain.as_ref().map(|_| "StderrDrain(active)"),
            )
            .field(
                "connect_watchdog",
                &self
                    .connect_watchdog
                    .as_ref()
                    .map(|_| "ConnectWatchdog(armed)"),
            )
            .finish()
    }
}

impl SshChildHandle {
    /// Disarms the connection establishment watchdog.
    ///
    /// Call this after the SSH connection is confirmed as established (e.g.,
    /// after receiving the remote rsync version greeting). If the watchdog has
    /// already fired, this returns an error indicating the timeout expired.
    /// If no watchdog was armed, this is a no-op.
    pub fn cancel_connect_watchdog(&mut self) -> io::Result<()> {
        if let Some(watchdog) = self.connect_watchdog.take() {
            watchdog.cancel()
        } else {
            Ok(())
        }
    }

    /// Returns the stderr output collected so far by the background drain thread.
    ///
    /// The returned bytes are bounded to the most recent [`STDERR_BUFFER_CAP`]
    /// bytes. Returns an empty `Vec` if no stderr drain is active.
    ///
    /// This method can be called while the drain thread is still running to
    /// get a snapshot of the output collected up to that point.
    #[must_use]
    pub fn stderr_output(&self) -> Vec<u8> {
        self.stderr_drain
            .as_ref()
            .map_or_else(Vec::new, StderrDrain::collected)
    }

    /// Waits for the subprocess to exit.
    ///
    /// Joins the stderr drain thread after the child exits to ensure all
    /// error output has been forwarded.
    pub fn wait(mut self) -> io::Result<ExitStatus> {
        let status = self.child.wait();
        if let Some(ref mut drain) = self.stderr_drain {
            drain.join();
        }
        status
    }

    /// Waits for the subprocess to exit and returns the collected stderr output.
    ///
    /// This combines [`wait`](Self::wait) and [`stderr_output`](Self::stderr_output)
    /// into a single call, ensuring all stderr is captured before the handle
    /// is consumed.
    pub fn wait_with_stderr(mut self) -> io::Result<(ExitStatus, Vec<u8>)> {
        let status = self.child.wait();
        if let Some(ref mut drain) = self.stderr_drain {
            drain.join();
        }
        let stderr = self
            .stderr_drain
            .as_ref()
            .map_or_else(Vec::new, StderrDrain::collected);
        status.map(|s| (s, stderr))
    }
}

impl Drop for SshChildHandle {
    fn drop(&mut self) {
        // Drop the watchdog first to stop the background thread.
        drop(self.connect_watchdog.take());

        // Reap the child process to prevent zombies.
        // Unlike SshConnection::Drop, stdin is not owned here (it lives in
        // SshWriter) so we skip the close_stdin step.
        if let Ok(None) = self.child.try_wait() {
            let _ = self.child.kill();
        }
        let status = self.child.wait();

        // Surface collected stderr when the child exited with an error,
        // ensuring SSH diagnostics are visible even on abnormal control
        // flow (panic, early return) where the caller never calls
        // wait_with_stderr().
        if let Some(ref mut drain) = self.stderr_drain {
            drain.join_and_surface_on_error(&status);
        }
    }
}

impl Read for SshConnection {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        // Check whether the connect watchdog has fired before attempting a read.
        // When the watchdog fires, the child will be killed by Drop, and reads
        // would return EOF or a broken pipe error. Returning TimedOut gives the
        // caller a clear signal to map to the appropriate exit code.
        if let Some(ref watchdog) = self.connect_watchdog {
            if watchdog.has_fired() {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!(
                        "ssh connection establishment timed out after {} seconds",
                        watchdog.timeout.as_secs()
                    ),
                ));
            }
        }
        match self.stdout.as_mut() {
            Some(stdout) => stdout.read(buf),
            None => Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "stdout has already been taken",
            )),
        }
    }
}

impl Write for SshConnection {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self.stdin.as_mut() {
            Some(stdin) => stdin.write(buf),
            None => Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "stdin has already been closed",
            )),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self.stdin.as_mut() {
            Some(stdin) => stdin.flush(),
            None => Ok(()),
        }
    }
}

impl Drop for SshConnection {
    fn drop(&mut self) {
        // Drop the watchdog first to stop the background thread.
        drop(self.connect_watchdog.take());

        let _ = self.close_stdin();

        if let Some(ref mut child) = self.child {
            if let Ok(None) = child.try_wait() {
                let _ = child.kill();
            }
            let status = child.wait();

            // Surface collected stderr when the child exited with an error,
            // ensuring SSH diagnostics are visible even when the connection
            // is dropped without an explicit wait() call.
            if let Some(ref mut drain) = self.stderr_drain {
                drain.join_and_surface_on_error(&status);
            }
        }
    }
}
