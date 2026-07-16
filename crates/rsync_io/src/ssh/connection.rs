//! SSH connection management with split read/write support.
//!
//! This module provides [`SshConnection`] for managing SSH subprocess I/O,
//! with support for splitting into separate read/write halves for bidirectional
//! protocol communication. A background thread drains stderr from the child
//! process via a `StderrAuxChannel` to prevent pipe-buffer deadlocks when the
//! remote rsync writes error messages.

use std::io::{self, Read, Write};
use std::process::{Child, ChildStdin, ChildStdout, ExitStatus};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use logging::debug_log;

use super::aux_channel::BoxedStderrChannel;
use super::stall::{IoStallWatchdog, StallHandle, effective_io_timeout};

/// Applies stall detection to a completed read/write on a stall-tracked half.
///
/// Translates the watchdog's latched timeout into an [`io::ErrorKind::TimedOut`]
/// error (mapped by `core` to `ExitCode::Timeout`, upstream `RERR_TIMEOUT`) and
/// records forward progress on success so the shared clock is reset. When no
/// timeout is configured the result passes through untouched.
fn guard_io_result(stall: Option<&StallHandle>, res: io::Result<usize>) -> io::Result<usize> {
    let Some(handle) = stall else {
        return res;
    };
    // Checking after the (possibly blocking) call catches a watchdog that fired
    // mid-read: the child was killed to unblock the pipe, so the raw result is
    // an EOF or broken-pipe error that must surface as a timeout.
    if handle.timed_out() {
        return Err(handle.timeout_error());
    }
    if let Ok(n) = &res {
        handle.record(*n);
    }
    res
}

/// Arms the data-phase stall watchdog for a shared child handle.
///
/// Returns `(None, None)` when `io_timeout` is disabled. Otherwise arms a
/// watchdog whose abort action kills the child - closing its pipe endpoints and
/// unblocking any pending read/write - so the stalled I/O returns promptly and
/// the read/write half can surface a timeout error.
fn arm_io_stall(
    io_timeout: Option<Duration>,
    shared_child: &Arc<Mutex<Option<Child>>>,
) -> (Option<IoStallWatchdog>, Option<StallHandle>) {
    match effective_io_timeout(io_timeout) {
        Some(timeout) => {
            let kill_child = Arc::clone(shared_child);
            let abort = Box::new(move || {
                if let Ok(mut guard) = kill_child.lock() {
                    if let Some(child) = guard.as_mut() {
                        let _ = child.kill();
                    }
                }
            });
            let (watchdog, handle) = IoStallWatchdog::arm(timeout, abort);
            (Some(watchdog), Some(handle))
        }
        None => (None, None),
    }
}

/// Owns an active SSH subprocess and exposes its stdio handles.
///
/// The connection can be used directly via the [`Read`] and [`Write`] traits,
/// or split into separate [`SshReader`] and [`SshWriter`] halves using [`split`](Self::split).
///
/// When stderr is available, a background thread is spawned at construction
/// time to drain it via the configured `StderrAuxChannel`. This prevents
/// deadlocks when the remote process writes more than the OS pipe buffer
/// capacity to stderr. The collected output is retrievable via
/// [`stderr_output`](Self::stderr_output).
pub struct SshConnection {
    /// Child process handle shared with the connect watchdog thread.
    /// The watchdog needs access to call `Child::kill()` on timeout,
    /// so the handle is wrapped in `Arc<Mutex<Option<Child>>>`.
    child: Arc<Mutex<Option<Child>>>,
    stdin: Option<ChildStdin>,
    stdout: Option<ChildStdout>,
    stderr_drain: Option<BoxedStderrChannel>,
    connect_watchdog: Option<ConnectWatchdog>,
    /// Data-phase stall watchdog enforcing the negotiated `--timeout`. `None`
    /// when the timeout is disabled (`0`/absent), matching upstream's off state.
    io_stall_watchdog: Option<IoStallWatchdog>,
    /// Progress/timeout handle consulted by the read and write paths. Shares
    /// its clock and latch with [`Self::io_stall_watchdog`].
    stall: Option<StallHandle>,
}

impl SshConnection {
    /// Constructs a new connection from the spawned child process.
    ///
    /// `stderr_channel` carries the auxiliary stderr drain (pipe- or
    /// socketpair-backed). When `Some`, the drain thread is already running
    /// and will continuously consume the child's stderr until EOF. If
    /// `connect_timeout` is `Some`, a watchdog thread is armed that will kill
    /// the child process if the connection is not established within the
    /// given duration. Call
    /// [`cancel_connect_watchdog`](Self::cancel_connect_watchdog) after
    /// the remote rsync version greeting is received to disarm it.
    ///
    /// `io_timeout` is the negotiated `--timeout` applied to the data channel.
    /// When `Some(non-zero)`, a stall watchdog is armed that aborts the transfer
    /// with a timeout error if no read/write makes progress for that long. `0`
    /// or `None` disables stall detection, matching upstream's `io_timeout == 0`
    /// off state. upstream: io.c `set_io_timeout` / `check_timeout`.
    pub(super) fn new(
        child: Child,
        stdin: Option<ChildStdin>,
        stdout: ChildStdout,
        stderr_channel: Option<BoxedStderrChannel>,
        connect_timeout: Option<Duration>,
        io_timeout: Option<Duration>,
    ) -> Self {
        let shared_child = Arc::new(Mutex::new(Some(child)));
        let connect_watchdog =
            connect_timeout.map(|timeout| ConnectWatchdog::arm(timeout, Arc::clone(&shared_child)));
        let (io_stall_watchdog, stall) = arm_io_stall(io_timeout, &shared_child);
        Self {
            child: shared_child,
            stdin,
            stdout: Some(stdout),
            stderr_drain: stderr_channel,
            connect_watchdog,
            io_stall_watchdog,
            stall,
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
    /// The returned bytes are bounded to the most recent 64 KiB. Returns an
    /// empty `Vec` if no stderr handle was available.
    #[must_use]
    pub fn stderr_output(&self) -> Vec<u8> {
        self.stderr_drain
            .as_ref()
            .map_or_else(Vec::new, |drain| drain.collected())
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
        let mut guard = self.child.lock().unwrap_or_else(|e| e.into_inner());
        match guard.take() {
            Some(mut child) => {
                drop(guard);
                let status = child.wait();
                // The child has exited; wake the stderr drain before joining
                // so we don't block waiting for an EOF that may never arrive
                // (an ssh helper subprocess can inherit the write end).
                if let Some(ref drain) = self.stderr_drain {
                    drain.shutdown_read();
                }
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
        let mut guard = self.child.lock().unwrap_or_else(|e| e.into_inner());
        match guard.take() {
            Some(mut child) => {
                drop(guard);
                let status = child.wait();
                if let Some(ref drain) = self.stderr_drain {
                    drain.shutdown_read();
                }
                if let Some(ref mut drain) = self.stderr_drain {
                    drain.join();
                }
                let stderr = self
                    .stderr_drain
                    .as_ref()
                    .map_or_else(Vec::new, |drain| drain.collected());
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
        let mut guard = self.child.lock().unwrap_or_else(|e| e.into_inner());
        match guard.as_mut() {
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

        {
            let guard = self.child.lock().unwrap_or_else(|e| e.into_inner());
            if guard.is_none() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "child process already taken",
                ));
            }
        }
        // Move the whole shared handle to the child owner so the stall watchdog
        // (which holds a clone of this same `Arc`) can still reach the child to
        // kill it on a timeout. Leaving an empty `Arc` behind keeps
        // `SshConnection::Drop` from reaping the child we just handed off.
        let child = std::mem::replace(&mut self.child, Arc::new(Mutex::new(None)));

        let stderr_drain = self.stderr_drain.take();
        let connect_watchdog = self.connect_watchdog.take();
        let io_stall_watchdog = self.io_stall_watchdog.take();
        let stall = self.stall.take();

        Ok((
            SshReader {
                stdout,
                stall: stall.clone(),
            },
            SshWriter { stdin, stall },
            SshChildHandle {
                child,
                stderr_drain,
                connect_watchdog,
                io_stall_watchdog,
            },
        ))
    }
}

/// Read half of an SSH connection (subprocess stdout).
#[derive(Debug)]
pub struct SshReader {
    stdout: ChildStdout,
    /// Shared stall handle; `None` when `--timeout` is disabled.
    stall: Option<StallHandle>,
}

impl Read for SshReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let res = self.stdout.read(buf);
        guard_io_result(self.stall.as_ref(), res)
    }
}

/// Write half of an SSH connection (subprocess stdin).
#[derive(Debug)]
pub struct SshWriter {
    stdin: ChildStdin,
    /// Shared stall handle; `None` when `--timeout` is disabled.
    stall: Option<StallHandle>,
}

impl Write for SshWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let res = self.stdin.write(buf);
        guard_io_result(self.stall.as_ref(), res)
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
    ///
    /// The `shared_child` handle allows the watchdog thread to call
    /// `Child::kill()` directly on timeout, which unblocks any pending
    /// read/write on the child's pipes.
    fn arm(timeout: Duration, shared_child: Arc<Mutex<Option<Child>>>) -> Self {
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

                // Timeout expired - set the fired flag and kill the child process.
                // Killing the child unblocks any pending read/write on its pipes,
                // preventing callers from hanging in blocking I/O.
                if result.timed_out() {
                    thread_fired.store(true, Ordering::Release);
                    debug_log!(
                        Connect,
                        1,
                        "ssh connect watchdog: timeout after {timeout:?}"
                    );
                    // Kill the child via the shared handle. Child::kill() is safe
                    // Rust - no unsafe code needed. Killing closes the child's
                    // pipe endpoints, unblocking any blocking read/write.
                    if let Ok(mut guard) = shared_child.lock() {
                        if let Some(ref mut child) = *guard {
                            let _ = child.kill();
                        }
                    }
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
/// When the connection is split, the stderr drain (spawned at connection
/// creation time) is transferred to this handle. The drain thread prevents
/// pipe-buffer deadlocks by continuously reading stderr and forwarding lines
/// to process stderr. Collected output is retrievable via
/// [`stderr_output`](Self::stderr_output).
pub struct SshChildHandle {
    /// Shared with the stall watchdog so both can reach the child; the watchdog
    /// kills it on an I/O timeout, while `wait`/`Drop` reap it normally.
    child: Arc<Mutex<Option<Child>>>,
    stderr_drain: Option<BoxedStderrChannel>,
    connect_watchdog: Option<ConnectWatchdog>,
    /// Data-phase stall watchdog transferred here on `split`. Dropped/cancelled
    /// before the child is reaped so it cannot fire during teardown.
    io_stall_watchdog: Option<IoStallWatchdog>,
}

impl std::fmt::Debug for SshChildHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SshChildHandle")
            .field("child", &self.child)
            .field(
                "stderr_drain",
                &self
                    .stderr_drain
                    .as_ref()
                    .map(|_| "StderrAuxChannel(active)"),
            )
            .field(
                "connect_watchdog",
                &self
                    .connect_watchdog
                    .as_ref()
                    .map(|_| "ConnectWatchdog(armed)"),
            )
            .field(
                "io_stall_watchdog",
                &self
                    .io_stall_watchdog
                    .as_ref()
                    .map(|_| "IoStallWatchdog(armed)"),
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
    /// The returned bytes are bounded to the most recent 64 KiB. Returns an
    /// empty `Vec` if no stderr drain is active.
    ///
    /// This method can be called while the drain thread is still running to
    /// get a snapshot of the output collected up to that point.
    #[must_use]
    pub fn stderr_output(&self) -> Vec<u8> {
        self.stderr_drain
            .as_ref()
            .map_or_else(Vec::new, |drain| drain.collected())
    }

    /// Reaps the shared child handle, waiting for it to exit.
    ///
    /// Returns an error if the child was already taken by a prior wait/drop.
    fn take_and_wait_child(&mut self) -> io::Result<ExitStatus> {
        let mut guard = self.child.lock().unwrap_or_else(|e| e.into_inner());
        match guard.take() {
            Some(mut child) => {
                drop(guard);
                child.wait()
            }
            None => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "child process already taken",
            )),
        }
    }

    /// Waits for the subprocess to exit.
    ///
    /// Joins the stderr drain thread after the child exits to ensure all
    /// error output has been forwarded.
    pub fn wait(mut self) -> io::Result<ExitStatus> {
        // Cancel stall detection before reaping so it cannot kill the child
        // during a legitimate final lull.
        drop(self.io_stall_watchdog.take());
        let status = self.take_and_wait_child();
        if let Some(ref drain) = self.stderr_drain {
            drain.shutdown_read();
        }
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
        drop(self.io_stall_watchdog.take());
        let status = self.take_and_wait_child();
        // Wake the drain thread now that the child is reaped; a re-execed
        // ssh helper may still hold the write end open so EOF cannot be
        // relied on by itself.
        if let Some(ref drain) = self.stderr_drain {
            drain.shutdown_read();
        }
        if let Some(ref mut drain) = self.stderr_drain {
            drain.join();
        }
        let stderr = self
            .stderr_drain
            .as_ref()
            .map_or_else(Vec::new, |drain| drain.collected());
        status.map(|s| (s, stderr))
    }
}

impl Drop for SshChildHandle {
    fn drop(&mut self) {
        // Drop the watchdogs first so their background threads exit before we
        // touch the child handle.
        drop(self.connect_watchdog.take());
        drop(self.io_stall_watchdog.take());

        // Reap the child to prevent zombies. Unlike SshConnection::Drop,
        // stdin is not owned here (it lives in SshWriter) so we skip the
        // close_stdin step.
        let mut guard = self.child.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(mut child) = guard.take() {
            drop(guard);
            if let Ok(None) = child.try_wait() {
                let _ = child.kill();
            }
            let status = child.wait();

            // Surface collected stderr when the child exited with an error,
            // ensuring SSH diagnostics are visible even on abnormal control
            // flow (panic, early return) where the caller never calls
            // wait_with_stderr().
            if let Some(ref mut drain) = self.stderr_drain {
                drain.join_and_surface_on_error(&status);
            }
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
        let res = match self.stdout.as_mut() {
            Some(stdout) => stdout.read(buf),
            None => Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "stdout has already been taken",
            )),
        };
        guard_io_result(self.stall.as_ref(), res)
    }
}

impl Write for SshConnection {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let res = match self.stdin.as_mut() {
            Some(stdin) => stdin.write(buf),
            None => Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "stdin has already been closed",
            )),
        };
        guard_io_result(self.stall.as_ref(), res)
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
        // Drop the watchdogs first so their background threads exit before we
        // touch the child handle.
        drop(self.connect_watchdog.take());
        drop(self.io_stall_watchdog.take());

        let _ = self.close_stdin();

        let mut guard = self.child.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(ref mut child) = *guard {
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
