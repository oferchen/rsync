//! The bounded drain-and-wait contract, pinned on the two shapes that break a
//! naive `try_wait` poll over undrained pipes.
//!
//! Both were measured against the pre-fix code on macOS and on Linux before
//! this owner existed: a child writing 256 KiB per stream burned the entire
//! budget and was reported as a timeout, and a run whose output never reached
//! EOF did not return at all until its descendant died.
//!
//! `#![cfg(unix)]` - the fixtures drive `/bin/sh`. The primitive itself is
//! platform-neutral.
#![cfg(unix)]

use std::process::Command;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use test_support::{Deadlined, OcRsyncCliRunner, RunnerError, run_deadlined};

/// Output per stream, comfortably past the 64 KiB pipe buffer on both Linux and
/// macOS, so the child MUST block unless the parent is reading.
const FLOOD: usize = 256 * 1024;
/// Budget for the flooding cells. Long enough that `/bin/sh` writing a few
/// hundred KiB cannot race it.
const FLOOD_BUDGET: Duration = Duration::from_secs(10);
/// Budget for the descendant cells. Short, because the point is that it fires.
const SHORT_BUDGET: Duration = Duration::from_secs(2);

/// Run `body` on a worker thread, failing if it does not return within `bound`.
///
/// The bound has to live outside the code under test: a regression in the
/// primitive blocks its caller forever, so an assertion placed after the call
/// would never be reached and the test would hang CI instead of failing it.
fn within<T: Send + 'static>(bound: Duration, body: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(body());
    });
    rx.recv_timeout(bound)
        .unwrap_or_else(|_| panic!("call did not return within {bound:?}"))
}

/// A child writing far past the pipe buffer on both streams.
fn flood_script() -> String {
    format!(
        "yes 0123456789abcdef | head -c {FLOOD}\n\
         yes 0123456789abcdef | head -c {FLOOD} >&2\n"
    )
}

/// `sh` exits immediately while a backgrounded child keeps both pipes open well
/// past any budget here, so the output never reaches EOF.
const ORPHAN_SCRIPT: &str = "sleep 30 & exit 0";

#[test]
fn run_deadlined_collects_a_child_that_outwrites_the_pipe_buffer() {
    let script = flood_script();
    let outcome = within(FLOOD_BUDGET * 3, move || {
        let mut command = Command::new("/bin/sh");
        command.arg("-c").arg(&script);
        run_deadlined(&mut command, FLOOD_BUDGET)
    })
    .expect("spawn");

    match outcome {
        Deadlined::Finished {
            status,
            stdout,
            stderr,
        } => {
            assert!(status.success(), "flooding child exited non-zero");
            assert_eq!(stdout.len(), FLOOD, "stdout was truncated");
            assert_eq!(stderr.len(), FLOOD, "stderr was truncated");
        }
        Deadlined::Expired { budget, .. } => {
            panic!("flooding child was starved and reported as a timeout after {budget:?}")
        }
    }
}

/// The budget must bound the whole run, not just the process wait.
///
/// `try_wait` reports the exit on the first poll, so the deadline is never
/// consulted again - but the output still cannot reach EOF. This is the
/// remote-shell shape (client -> `lsh-stub` -> `--server`), and it does not
/// return at all unless the collection is deadlined too.
#[test]
fn run_deadlined_expires_when_a_descendant_holds_the_pipe_open() {
    let started = Instant::now();
    let outcome = within(FLOOD_BUDGET * 3, move || {
        let mut command = Command::new("/bin/sh");
        command.arg("-c").arg(ORPHAN_SCRIPT);
        run_deadlined(&mut command, SHORT_BUDGET)
    })
    .expect("spawn");

    match outcome {
        Deadlined::Expired { budget, .. } => assert_eq!(budget, SHORT_BUDGET),
        Deadlined::Finished { .. } => {
            panic!("a run whose output never reaches EOF must expire")
        }
    }
    let elapsed = started.elapsed();
    assert!(
        elapsed >= SHORT_BUDGET,
        "returned before the budget expired: {elapsed:?}",
    );
    assert!(
        elapsed < FLOOD_BUDGET,
        "the budget did not bound the collection: {elapsed:?}",
    );
}

/// [`OcRsyncCliRunner`] inherits the drain: a verbose run is collected in full
/// rather than starved into its own timeout.
#[test]
fn cli_runner_collects_a_child_that_outwrites_the_pipe_buffer() {
    let script = flood_script();
    let out = within(FLOOD_BUDGET * 3, move || {
        OcRsyncCliRunner::new()
            .binary("/bin/sh")
            .args(["-c", script.as_str()])
            .timeout(FLOOD_BUDGET)
            .run()
    })
    .expect("flooding child must not be reported as a timeout");

    assert_eq!(out.stdout.len(), FLOOD, "stdout was truncated");
    assert_eq!(out.stderr.len(), FLOOD, "stderr was truncated");
}

/// ...and inherits the bound, mapping an overrun onto its own typed error
/// instead of blocking in the collection.
#[test]
fn cli_runner_reports_a_timeout_when_a_descendant_holds_the_pipe_open() {
    let started = Instant::now();
    let err = within(FLOOD_BUDGET * 3, move || {
        OcRsyncCliRunner::new()
            .binary("/bin/sh")
            .args(["-c", ORPHAN_SCRIPT])
            .timeout(SHORT_BUDGET)
            .run()
            .err()
    })
    .expect("a run whose output never reaches EOF must time out");

    match err {
        RunnerError::Timeout { after, .. } => assert_eq!(after, SHORT_BUDGET),
        other => panic!("expected a timeout, got: {other}"),
    }
    assert!(
        started.elapsed() < FLOOD_BUDGET,
        "the timeout did not bound the collection",
    );
}
