//! The daemon a test drives runs as a process of its own, as upstream's does.
//!
//! upstream: main.c:1954 `daemon_main()` runs the daemon as its own process and
//! socket.c:761 `start_accept_loop()` forks every session from it, so a client
//! is always another process and closing its socket delivers FIN.
//!
//! A daemon on a thread of the test process breaks that. Each session child is
//! then forked from the test process and inherits a duplicate of every client
//! socket open at fork time - including the one it serves - so a client that
//! closes never delivers EOF and the session waits out its I/O timeout after
//! the test has finished. Forking the daemon before any client socket exists
//! restores the upstream topology: the daemon, and every session it forks,
//! holds only its own descriptors. It also forks sessions from a
//! single-threaded parent, the precondition `platform::session_fork` states.
//!
//! The daemon's result crosses the process boundary through a file the handle
//! owns rather than a pipe: session children inherit a pipe's write end, so a
//! read to EOF would wait on them - the defect this module exists to remove.
//!
//! The parent module must have `DaemonConfig`, `DaemonConfigBuilder`,
//! `DaemonError` and `run_daemon` in scope: the unit tests include this module
//! directly and the integration tests through `#[path]`, so it names the
//! daemon API through `super::` rather than through either crate path.

#[cfg(not(unix))]
use super::DaemonConfigBuilder;
use super::{DaemonConfig, DaemonError, run_daemon};
use std::fmt;
#[cfg(unix)]
use std::fs;
#[cfg(unix)]
use std::path::PathBuf;

/// How a daemon run that returned failed: the `DaemonError` it returned,
/// reduced to what survives a process boundary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct DaemonFailure {
    pub(super) exit_code: i32,
    pub(super) message: String,
}

impl From<DaemonError> for DaemonFailure {
    fn from(error: DaemonError) -> Self {
        Self {
            exit_code: error.exit_code(),
            message: error.message().to_string(),
        }
    }
}

/// What `run_daemon` returned, carried out of the daemon process.
pub(super) type DaemonResult = Result<(), DaemonFailure>;

/// A daemon that never returned: it panicked, was killed, or exited by a path
/// that bypassed the harness.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct DaemonCrash(pub(super) String);

impl fmt::Display for DaemonCrash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The one encoding of a daemon's end, written by the daemon process and read
/// back by the test.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Outcome {
    Returned(DaemonResult),
    Panicked(String),
}

impl Outcome {
    #[cfg_attr(not(unix), allow(dead_code))]
    fn encode(&self) -> String {
        match self {
            Self::Returned(Ok(())) => String::from("ok"),
            Self::Returned(Err(failure)) => {
                format!("err {}\n{}", failure.exit_code, failure.message)
            }
            Self::Panicked(message) => format!("panic\n{message}"),
        }
    }

    #[cfg_attr(not(unix), allow(dead_code))]
    fn decode(text: &str) -> Option<Self> {
        if text == "ok" {
            return Some(Self::Returned(Ok(())));
        }
        let (head, body) = text.split_once('\n')?;
        match head.split_once(' ') {
            Some(("err", code)) => Some(Self::Returned(Err(DaemonFailure {
                exit_code: code.parse().ok()?,
                message: body.to_owned(),
            }))),
            None if head == "panic" => Some(Self::Panicked(body.to_owned())),
            _ => None,
        }
    }

    #[cfg_attr(not(unix), allow(dead_code))]
    fn exit_code(&self) -> i32 {
        match self {
            Self::Returned(Ok(())) => 0,
            Self::Returned(Err(failure)) => failure.exit_code,
            Self::Panicked(_) => PANIC_EXIT_CODE,
        }
    }

    fn into_join(self) -> Result<DaemonResult, DaemonCrash> {
        match self {
            Self::Returned(result) => Ok(result),
            Self::Panicked(message) => Err(DaemonCrash(format!("daemon panicked: {message}"))),
        }
    }
}

/// The status Rust gives a process whose main thread panicked.
#[cfg_attr(not(unix), allow(dead_code))]
const PANIC_EXIT_CODE: i32 = 101;

fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|text| (*text).to_owned())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| String::from("non-string panic payload"))
}

/// A running test daemon, in its own process group on Unix.
///
/// Dropping the handle kills the whole group - the daemon and any session it
/// still runs - and reaps the daemon, so no test leaves a process behind.
#[cfg(unix)]
pub(super) struct DaemonHandle {
    pid: i32,
    outcome_dir: tempfile::TempDir,
    reaped: bool,
}

#[cfg(unix)]
impl DaemonHandle {
    /// Forks a process that runs `run_daemon(config)` and records its end.
    ///
    /// Call it before opening any client socket: whatever the test process
    /// holds at this point, the daemon and all its sessions inherit.
    pub(super) fn spawn(config: DaemonConfig) -> Self {
        use platform::session_fork::{ForkSide, exit_child, fork_session, lead_process_group};

        let outcome_dir = tempfile::tempdir().expect("daemon outcome dir");
        let outcome_path = outcome_dir.path().join("outcome");
        match fork_session().expect("fork the test daemon") {
            ForkSide::Child => exit_child(run_in_child(config, outcome_path)),
            ForkSide::Parent { child_pid } => {
                // The parent's copy of the pre-bound listener goes with the
                // config; only the daemon may hold it.
                drop(config);
                lead_process_group(child_pid).expect("put the test daemon in its own group");
                Self {
                    pid: child_pid,
                    outcome_dir,
                    reaped: false,
                }
            }
        }
    }

    /// Pid of the daemon process.
    pub(super) fn pid(&self) -> u32 {
        self.pid.unsigned_abs()
    }

    /// Asks the daemon to shut down, as SIGTERM does to upstream's.
    #[allow(
        dead_code,
        reason = "only the integration-test binaries stop a daemon by signal"
    )]
    pub(super) fn terminate(&self) {
        platform::session_fork::terminate_process(self.pid).expect("signal the test daemon");
    }

    /// Blocks until the daemon process ends, and reports what it returned.
    ///
    /// The end is observed with `WNOWAIT` first, so the daemon stays an
    /// unreaped zombie - and its pid, which is the group id, stays unreusable -
    /// while any session it left behind is killed. Only then is it reaped.
    pub(super) fn join(mut self) -> Result<DaemonResult, DaemonCrash> {
        use platform::session_fork::{await_child_end, kill_process_group, wait_for_child};

        await_child_end(self.pid).expect("observe the daemon's end");
        kill_process_group(self.pid).expect("stop sessions the daemon left");
        let end = wait_for_child(self.pid).expect("reap the daemon");
        self.reaped = true;
        match fs::read_to_string(self.outcome_path()) {
            Ok(text) => Outcome::decode(&text)
                .ok_or_else(|| DaemonCrash(format!("unreadable daemon outcome {text:?}")))?
                .into_join(),
            Err(_) => Err(DaemonCrash(format!(
                "daemon ended without recording an outcome: {end:?}"
            ))),
        }
    }

    fn outcome_path(&self) -> PathBuf {
        self.outcome_dir.path().join("outcome")
    }
}

#[cfg(unix)]
impl Drop for DaemonHandle {
    fn drop(&mut self) {
        use platform::session_fork::{kill_process_group, wait_for_child};

        if self.reaped {
            return;
        }
        let stopped = kill_process_group(self.pid).and_then(|()| wait_for_child(self.pid));
        if let Err(error) = stopped
            && !std::thread::panicking()
        {
            panic!("stop the test daemon {}: {error}", self.pid);
        }
    }
}

/// Runs the daemon in the forked child and returns the status to exit with.
#[cfg(unix)]
fn run_in_child(config: DaemonConfig, outcome_path: PathBuf) -> i32 {
    use std::io::Write as _;
    use std::panic::{AssertUnwindSafe, catch_unwind};

    let outcome = match platform::session_fork::lead_process_group(0) {
        Err(error) => Outcome::Panicked(format!("could not lead a process group: {error}")),
        Ok(()) => match catch_unwind(AssertUnwindSafe(|| run_daemon(config))) {
            Ok(result) => Outcome::Returned(result.map_err(DaemonFailure::from)),
            Err(payload) => Outcome::Panicked(panic_message(payload.as_ref())),
        },
    };
    let recorded = fs::write(&outcome_path, outcome.encode());
    // `exit_child` skips the flush a normal exit would run.
    let _ = std::io::stdout().flush();
    let _ = std::io::stderr().flush();
    match recorded {
        Ok(()) => outcome.exit_code(),
        Err(_) => PANIC_EXIT_CODE,
    }
}

/// A running test daemon on a thread: Windows has no `fork`, and serves
/// sessions on threads, so a client close already reaches its session.
#[cfg(not(unix))]
pub(super) struct DaemonHandle {
    thread: std::thread::JoinHandle<Result<(), DaemonError>>,
    signals: platform::signal::SignalFlags,
}

#[cfg(not(unix))]
impl DaemonHandle {
    /// Runs `run_daemon(config)` on a thread, with signal flags the handle can
    /// set in place of a signal.
    pub(super) fn spawn(config: DaemonConfig) -> Self {
        let signals = platform::signal::SignalFlags::new();
        let config = DaemonConfigBuilder::from(config)
            .signal_flags(signals.clone())
            .build();
        Self {
            thread: std::thread::spawn(move || run_daemon(config)),
            signals,
        }
    }

    /// Asks the daemon to shut down, as a console CTRL_C does.
    #[allow(
        dead_code,
        reason = "only the integration-test binaries stop a daemon by signal"
    )]
    pub(super) fn terminate(&self) {
        self.signals
            .shutdown
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }

    /// Whether the daemon thread has returned.
    pub(super) fn is_finished(&self) -> bool {
        self.thread.is_finished()
    }

    /// Blocks until the daemon thread returns, and reports what it returned.
    pub(super) fn join(self) -> Result<DaemonResult, DaemonCrash> {
        self.thread
            .join()
            .map(|result| result.map_err(DaemonFailure::from))
            .map_err(|payload| Outcome::Panicked(panic_message(payload.as_ref())))
            .or_else(Outcome::into_join)
    }
}

#[test]
fn daemon_outcome_encoding_round_trips_every_variant() {
    let outcomes = [
        Outcome::Returned(Ok(())),
        Outcome::Returned(Err(DaemonFailure {
            exit_code: 10,
            message: String::from("failed\nacross lines"),
        })),
        Outcome::Panicked(String::from("boom")),
    ];
    for outcome in outcomes {
        assert_eq!(Outcome::decode(&outcome.encode()), Some(outcome));
    }
    assert_eq!(Outcome::decode("garbage"), None);
}
