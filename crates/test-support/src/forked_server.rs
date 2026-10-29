//! A server a test drives, run as a process of its own, as upstream's is.
//!
//! upstream: main.c:1954 `daemon_main()` runs the daemon as its own process and
//! socket.c:761 `start_accept_loop()` forks every session from it, so a client
//! is always another process and closing its socket delivers FIN.
//!
//! A server on a thread of the test process breaks that. Each session child is
//! then forked from the test process and inherits a duplicate of every client
//! socket open at fork time - including the one it serves - so a client that
//! closes never delivers EOF and the session waits out its I/O timeout after
//! the test has finished. Forking the server before any client socket exists
//! restores the upstream topology: the server, and every session it forks,
//! holds only its own descriptors. It also forks sessions from a
//! single-threaded parent, the precondition `platform::session_fork` states.
//!
//! The run is a closure, so the harness names no server API: the daemon's own
//! tests and the client tests that need a daemon both hand it
//! `move || run_daemon(config)`. Forking the test process also means no binary
//! is executed, so a cold page-in of a large debug executable never sits
//! between a test and its server.
//!
//! The run's result crosses the process boundary through a file the handle
//! owns rather than a pipe: session children inherit a pipe's write end, so a
//! read to EOF would wait on them - the defect this module exists to remove.

use std::fmt;

/// How a run that returned failed, reduced to what survives a process
/// boundary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunFailure {
    /// The exit code the server reported.
    pub exit_code: i32,
    /// The server's error message.
    pub message: String,
}

/// What the run returned, carried out of the server process.
pub type RunResult = Result<(), RunFailure>;

/// A run that never returned: it panicked, was killed, or exited by a path
/// that bypassed the harness.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunCrash(pub String);

impl fmt::Display for RunCrash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The one encoding of a run's end, written by the server process and read
/// back by the test.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Outcome {
    Returned(RunResult),
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
            Some(("err", code)) => Some(Self::Returned(Err(RunFailure {
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

    fn into_join(self) -> Result<RunResult, RunCrash> {
        match self {
            Self::Returned(result) => Ok(result),
            Self::Panicked(message) => Err(RunCrash(format!("server panicked: {message}"))),
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

/// A running test server, in its own process group on Unix.
///
/// Dropping the handle kills the whole group - the server and any session it
/// still runs - and reaps the server, so no test leaves a process behind.
#[cfg(unix)]
#[derive(Debug)]
pub struct ForkedServer {
    pid: i32,
    outcome_dir: tempfile::TempDir,
    reaped: bool,
}

#[cfg(unix)]
impl ForkedServer {
    /// Forks a process that runs `run` and records its end.
    ///
    /// Call it before opening any client socket: whatever the test process
    /// holds at this point, the server and all its sessions inherit.
    ///
    /// # Panics
    ///
    /// When the outcome directory cannot be created, the fork fails, or the
    /// server cannot be put in a process group of its own.
    pub fn spawn<F: FnOnce() -> RunResult>(run: F) -> Self {
        Self::fork(run, None)
    }

    /// [`spawn`](Self::spawn), with the server's standard error readable from
    /// the returned pipe - a log it writes to `/dev/stderr` included, since the
    /// kernel resolves that path through fd 2.
    ///
    /// The pipe reaches EOF once the server and every session it forked have
    /// exited, because each of them holds the write end as its fd 2.
    ///
    /// # Panics
    ///
    /// As [`spawn`](Self::spawn), and when the pipe cannot be created.
    pub fn spawn_capturing_stderr<F: FnOnce() -> RunResult>(run: F) -> (Self, std::io::PipeReader) {
        let (reader, writer) = std::io::pipe().expect("server stderr pipe");
        (Self::fork(run, Some(writer)), reader)
    }

    fn fork<F: FnOnce() -> RunResult>(run: F, stderr: Option<std::io::PipeWriter>) -> Self {
        use platform::session_fork::{
            ForkSide, exit_child, fork_session, lead_process_group, redirect_stderr,
        };

        let outcome_dir = tempfile::tempdir().expect("server outcome dir");
        let outcome_path = outcome_dir.path().join("outcome");
        match fork_session().expect("fork the test server") {
            ForkSide::Child => {
                let run = move || {
                    if let Some(writer) = stderr {
                        redirect_stderr(&writer).expect("route the server's stderr to its pipe");
                    }
                    run()
                };
                exit_child(run_in_child(run, &outcome_path))
            }
            ForkSide::Parent { child_pid } => {
                // The parent's copy of everything `run` captured - a pre-bound
                // listener included - goes here; only the server may hold it.
                // So does its copy of the stderr pipe's write end, or the
                // reader would never see EOF.
                drop(run);
                drop(stderr);
                lead_process_group(child_pid).expect("put the test server in its own group");
                Self {
                    pid: child_pid,
                    outcome_dir,
                    reaped: false,
                }
            }
        }
    }

    /// Pid of the server process.
    #[must_use]
    pub fn pid(&self) -> u32 {
        self.pid.unsigned_abs()
    }

    /// Asks the server to shut down, as SIGTERM does to upstream's daemon.
    ///
    /// # Panics
    ///
    /// When the signal cannot be sent.
    pub fn terminate(&self) {
        platform::session_fork::terminate_process(self.pid).expect("signal the test server");
    }

    /// Blocks until the server process ends, and reports what its run
    /// returned.
    ///
    /// The end is observed with `WNOWAIT` first, so the server stays an
    /// unreaped zombie - and its pid, which is the group id, stays unreusable -
    /// while any session it left behind is killed. Only then is it reaped.
    ///
    /// # Errors
    ///
    /// [`RunCrash`] when the run panicked or the process ended without
    /// recording an outcome.
    ///
    /// # Panics
    ///
    /// When the server cannot be waited for or its group cannot be stopped.
    pub fn join(mut self) -> Result<RunResult, RunCrash> {
        use platform::session_fork::{await_child_end, kill_process_group, wait_for_child};

        await_child_end(self.pid).expect("observe the server's end");
        kill_process_group(self.pid).expect("stop sessions the server left");
        let end = wait_for_child(self.pid).expect("reap the server");
        self.reaped = true;
        match std::fs::read_to_string(self.outcome_dir.path().join("outcome")) {
            Ok(text) => Outcome::decode(&text)
                .ok_or_else(|| RunCrash(format!("unreadable server outcome {text:?}")))?
                .into_join(),
            Err(_) => Err(RunCrash(format!(
                "server ended without recording an outcome: {end:?}"
            ))),
        }
    }
}

#[cfg(unix)]
impl Drop for ForkedServer {
    fn drop(&mut self) {
        use platform::session_fork::{kill_process_group, wait_for_child};

        if self.reaped {
            return;
        }
        let stopped = kill_process_group(self.pid).and_then(|()| wait_for_child(self.pid));
        match stopped {
            Err(error) if !std::thread::panicking() => {
                panic!("stop the test server {}: {error}", self.pid);
            }
            // A test that fails while talking to the server may be failing
            // because the server ended first, and the client's error rarely
            // says so: a client still queued on a listener whose last holder
            // exits reads ECONNRESET. Name how the server ended.
            Ok(end) if std::thread::panicking() => {
                if let Some(report) = self.early_end_report(&end) {
                    eprintln!("{report}");
                }
            }
            _ => {}
        }
    }
}

#[cfg(unix)]
impl ForkedServer {
    /// Describes how the server ended when it ended on its own, before the
    /// handle stopped it: `end` is what reaping it returned.
    ///
    /// A server the handle had to kill records no outcome and ends by the
    /// `SIGKILL` it was sent; anything else is an end of its own.
    fn early_end_report(&self, end: &platform::session_fork::ChildEnd) -> Option<String> {
        use platform::session_fork::ChildEnd;

        let pid = self.pid;
        match std::fs::read_to_string(self.outcome_dir.path().join("outcome")) {
            Ok(text) => Some(match Outcome::decode(&text) {
                Some(Outcome::Returned(Ok(()))) => {
                    format!("test server {pid} had already returned Ok ({end:?})")
                }
                Some(Outcome::Returned(Err(failure))) => format!(
                    "test server {pid} had already failed with exit {}: {}",
                    failure.exit_code, failure.message
                ),
                Some(Outcome::Panicked(message)) => {
                    format!("test server {pid} had already panicked: {message}")
                }
                None => format!("test server {pid} had already ended ({end:?}): {text:?}"),
            }),
            Err(_) if *end == ChildEnd::Signalled(libc::SIGKILL) => None,
            Err(_) => Some(format!(
                "test server {pid} had already ended without an outcome ({end:?})"
            )),
        }
    }
}

/// Runs `run` in the forked child and returns the status to exit with.
#[cfg(unix)]
fn run_in_child<F: FnOnce() -> RunResult>(run: F, outcome_path: &std::path::Path) -> i32 {
    use std::io::Write as _;
    use std::panic::{AssertUnwindSafe, catch_unwind};

    let outcome = match platform::session_fork::lead_process_group(0) {
        Err(error) => Outcome::Panicked(format!("could not lead a process group: {error}")),
        Ok(()) => match catch_unwind(AssertUnwindSafe(run)) {
            Ok(result) => Outcome::Returned(result),
            Err(payload) => Outcome::Panicked(panic_message(payload.as_ref())),
        },
    };
    let recorded = std::fs::write(outcome_path, outcome.encode());
    // `exit_child` skips the flush a normal exit would run.
    let _ = std::io::stdout().flush();
    let _ = std::io::stderr().flush();
    match recorded {
        Ok(()) => outcome.exit_code(),
        Err(_) => PANIC_EXIT_CODE,
    }
}

/// A running test server on a thread: Windows has no `fork`, and serves
/// sessions on threads, so a client close already reaches its session.
#[cfg(not(unix))]
#[derive(Debug)]
pub struct ForkedServer {
    thread: std::thread::JoinHandle<RunResult>,
}

#[cfg(not(unix))]
impl ForkedServer {
    /// Runs `run` on a thread.
    pub fn spawn<F: FnOnce() -> RunResult + Send + 'static>(run: F) -> Self {
        Self {
            thread: std::thread::spawn(run),
        }
    }

    /// Whether the server thread has returned.
    #[must_use]
    pub fn is_finished(&self) -> bool {
        self.thread.is_finished()
    }

    /// Blocks until the server thread returns, and reports what it returned.
    ///
    /// # Errors
    ///
    /// [`RunCrash`] when the run panicked.
    pub fn join(self) -> Result<RunResult, RunCrash> {
        self.thread
            .join()
            .map_err(|payload| Outcome::Panicked(panic_message(payload.as_ref())))
            .or_else(Outcome::into_join)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outcome_encoding_round_trips_every_variant() {
        let outcomes = [
            Outcome::Returned(Ok(())),
            Outcome::Returned(Err(RunFailure {
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

    #[test]
    fn a_run_that_returns_reports_its_result() {
        assert_eq!(ForkedServer::spawn(|| Ok(())).join(), Ok(Ok(())));
        let failure = RunFailure {
            exit_code: 12,
            message: String::from("refused"),
        };
        let expected = failure.clone();
        assert_eq!(
            ForkedServer::spawn(move || Err(failure)).join(),
            Ok(Err(expected))
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_captured_stderr_carries_what_the_server_writes_and_then_ends() {
        use std::io::Read as _;

        let (server, mut stderr) = ForkedServer::spawn_capturing_stderr(|| {
            eprintln!("server says hello");
            Ok(())
        });
        let mut text = String::new();
        stderr.read_to_string(&mut text).expect("read to EOF");
        assert_eq!(text, "server says hello\n");
        assert_eq!(server.join(), Ok(Ok(())));
    }

    /// A server that ended on its own is named, with its result, when the
    /// test fails: its client may only see ECONNRESET.
    #[cfg(unix)]
    #[test]
    fn a_server_that_ended_first_is_reported_with_its_result() {
        use platform::session_fork::{ChildEnd, await_child_end};

        let server = ForkedServer::spawn(|| {
            Err(RunFailure {
                exit_code: 5,
                message: String::from("could not start"),
            })
        });
        await_child_end(server.pid).expect("observe the end");
        let report = server
            .early_end_report(&ChildEnd::Exited(5))
            .expect("an early end is reported");
        assert!(report.contains("exit 5: could not start"), "{report}");
    }

    /// The failure the report exists for: a server that ends while a client is
    /// still queued on its listener leaves that client reading ECONNRESET,
    /// which says nothing about the server. The run waits until the client has
    /// connected, then returns without accepting.
    #[cfg(unix)]
    #[test]
    fn a_client_queued_on_a_server_that_ends_reads_a_reset() {
        use platform::session_fork::{ChildEnd, await_child_end};
        use std::io::{Read as _, Write as _};
        use std::net::{TcpListener, TcpStream};

        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind");
        let port = listener.local_addr().expect("local addr").port();
        let (mut go_reader, mut go_writer) = std::io::pipe().expect("pipe");
        let server = ForkedServer::spawn(move || {
            let _listener = listener;
            let mut byte = [0u8; 1];
            go_reader
                .read_exact(&mut byte)
                .expect("wait for the client");
            Err(RunFailure {
                exit_code: 10,
                message: String::from("ended before accepting"),
            })
        });
        let mut client = TcpStream::connect(("127.0.0.1", port)).expect("connect");
        go_writer.write_all(b"x").expect("release the server");
        await_child_end(server.pid).expect("observe the end");

        let error = client
            .read(&mut [0u8; 16])
            .expect_err("the queued connection is reset");
        assert_eq!(error.kind(), std::io::ErrorKind::ConnectionReset);
        let report = server
            .early_end_report(&ChildEnd::Exited(10))
            .expect("an early end is reported");
        assert!(report.contains("ended before accepting"), "{report}");
    }

    /// A server the handle had to stop ended by the handle's `SIGKILL`, which
    /// says nothing about the failure, so nothing is reported.
    #[cfg(unix)]
    #[test]
    fn a_server_stopped_by_the_handle_is_not_reported() {
        use platform::session_fork::ChildEnd;

        let server = ForkedServer::spawn(|| {
            loop {
                std::thread::park();
            }
        });
        assert_eq!(
            server.early_end_report(&ChildEnd::Signalled(libc::SIGKILL)),
            None
        );
    }

    #[test]
    fn a_run_that_panics_reports_the_panic() {
        let crash = ForkedServer::spawn(|| panic!("server blew up"))
            .join()
            .expect_err("a panic is a crash");
        assert!(crash.0.contains("server blew up"), "{crash}");
    }
}
