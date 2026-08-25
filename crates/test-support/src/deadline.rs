//! Bounded drain-and-wait for a spawned child process.
//!
//! Every harness in this workspace that runs a child under a wall-clock cap
//! needs the same two properties, and a poll loop over `try_wait()` gives
//! neither for free:
//!
//! 1. **Both pipes are drained from the moment the child is spawned.** A child
//!    that writes past the pipe buffer - 64 KiB is typical on Linux and macOS -
//!    blocks in `write` until someone reads. A parent that only polls
//!    `try_wait()` never reads, so the child never exits, and the harness
//!    reports its own back-pressure as the hang it exists to detect. Measured:
//!    a child writing 256 KiB per stream burns the entire budget on both
//!    platforms before being killed.
//!
//! 2. **The budget bounds every wait, not just the process wait.** A descendant
//!    that outlives the child keeps the pipe write end open, so a final
//!    read-to-EOF never returns - `Child::wait_with_output()` blocks there
//!    forever, past a deadline that has already been checked for the last time.
//!    Measured: a 5 s budget against `sh -c 'sleep N & exit 0'` returns after
//!    exactly N seconds, so the wait is bounded by the descendant's lifetime
//!    and not at all by the deadline.
//!
//! [`run_deadlined`] is the single owner of both. Callers pass a configured
//! [`Command`] and a budget and get back a [`Deadlined`]; a run that overruns
//! its budget still returns whatever the child managed to write, which is
//! strictly better diagnostics than a lost buffer.

use std::io::{self, Read, Write};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

/// Interval between `try_wait()` probes while the child runs.
///
/// The probe is non-blocking and the pipes are drained on their own threads, so
/// this only bounds how promptly an exit is noticed.
const POLL_INTERVAL: Duration = Duration::from_millis(10);

/// Outcome of one deadlined run.
///
/// Both variants carry the output collected so far: on [`Deadlined::Expired`]
/// that is everything the child wrote before it was killed, not an empty
/// buffer.
#[derive(Debug)]
pub enum Deadlined {
    /// The child exited and both streams reached EOF within the budget.
    Finished {
        /// The child's exit status.
        status: ExitStatus,
        /// Everything the child wrote to stdout.
        stdout: Vec<u8>,
        /// Everything the child wrote to stderr.
        stderr: Vec<u8>,
    },
    /// The budget expired. The child has been killed and reaped.
    ///
    /// Reached either because the child was still running, or because it had
    /// exited but a surviving descendant still held a pipe open so the output
    /// never reached EOF.
    Expired {
        /// The budget that was exceeded, for the caller's diagnostic.
        budget: Duration,
        /// Whatever reached stdout before the budget ran out.
        stdout: Vec<u8>,
        /// Whatever reached stderr before the budget ran out.
        stderr: Vec<u8>,
    },
}

/// Run `command` under `budget`, draining both pipes throughout.
///
/// The child's stdio is configured by this function - stdin null, stdout and
/// stderr piped - so a caller's own `stdin`/`stdout`/`stderr` settings are
/// overridden. Everything else (argv, env, cwd, `pre_exec`) is the caller's.
///
/// # Errors
///
/// Returns the spawn error if the child cannot be started, or a `try_wait`
/// error. Overrunning the budget is *not* an error: it comes back as
/// [`Deadlined::Expired`] so the caller can attach its own diagnostic.
pub fn run_deadlined(command: &mut Command, budget: Duration) -> io::Result<Deadlined> {
    run_deadlined_with_stdin(command, budget, None)
}

/// [`run_deadlined`], additionally feeding `stdin` to the child.
///
/// The bytes are written on a scratch thread so a child that never reads its
/// stdin cannot block the caller; the budget still reaps such a child.
///
/// # Errors
///
/// As [`run_deadlined`].
pub fn run_deadlined_with_stdin(
    command: &mut Command,
    budget: Duration,
    stdin: Option<&[u8]>,
) -> io::Result<Deadlined> {
    command.stdin(if stdin.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    });
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());

    let mut child = command.spawn()?;
    let deadline = Instant::now() + budget;

    if let Some(data) = stdin {
        let mut sink = child.stdin.take().expect("stdin was piped");
        let data = data.to_vec();
        // Detached on purpose: the thread ends when the child consumes the
        // input or when the pipe closes on child exit. Dropping `sink` there
        // signals EOF.
        thread::spawn(move || {
            let _ = sink.write_all(&data);
        });
    }

    let stdout = Drain::spawn(child.stdout.take().expect("stdout was piped"));
    let stderr = Drain::spawn(child.stderr.take().expect("stderr was piped"));

    let status = loop {
        match child.try_wait()? {
            Some(status) => break Some(status),
            None if Instant::now() >= deadline => break None,
            None => thread::sleep(POLL_INTERVAL),
        }
    };

    // Both streams must reach EOF before the collected output is complete. A
    // descendant holding a pipe open makes that never happen, so it is bounded
    // against the same instant as the poll above.
    let drained =
        status.is_some() && stdout.wait_for_eof(deadline) && stderr.wait_for_eof(deadline);

    match status {
        Some(status) if drained => Ok(Deadlined::Finished {
            status,
            stdout: stdout.snapshot(),
            stderr: stderr.snapshot(),
        }),
        _ => {
            let _ = child.kill();
            let _ = child.wait();
            Ok(Deadlined::Expired {
                budget,
                stdout: stdout.snapshot(),
                stderr: stderr.snapshot(),
            })
        }
    }
}

/// One child pipe being read to EOF on its own thread.
///
/// The bytes land in a shared buffer rather than a channel payload so the
/// parent can snapshot partial output at any moment - including after giving up
/// on a stream that will never reach EOF.
struct Drain {
    buf: Arc<Mutex<Vec<u8>>>,
    eof: mpsc::Receiver<()>,
}

impl Drain {
    fn spawn<R: Read + Send + 'static>(mut pipe: R) -> Self {
        let buf = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&buf);
        let (tx, eof) = mpsc::channel();
        thread::spawn(move || {
            let mut chunk = [0u8; 8192];
            loop {
                match pipe.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => sink
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .extend_from_slice(&chunk[..n]),
                }
            }
            let _ = tx.send(());
        });
        Self { buf, eof }
    }

    /// Wait until the pipe reaches EOF, giving up at `deadline`.
    fn wait_for_eof(&self, deadline: Instant) -> bool {
        self.eof
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .is_ok()
    }

    fn snapshot(&self) -> Vec<u8> {
        self.buf
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}
