//! Regression coverage for the `TestDaemon` allocate-then-bind port race.
//!
//! `allocate_test_port` binds `127.0.0.1:0`, reads the OS-assigned port, then
//! drops the listener before the daemon binds it. That drop->bind gap is a
//! TOCTOU window: under parallel test load (macOS CI runners especially)
//! another process can claim the port first, the daemon logs `Address already
//! in use` and never becomes ready, and the readiness probe times out with
//! `daemon did not become ready`. Before the retry, that surfaced as a flaky
//! failure. `TestDaemon::start` now retries on a fresh port, but ONLY for that
//! specific bind conflict - any other startup failure must still propagate so a
//! real bug is never masked. The classifier `is_addr_in_use` is what draws that
//! line, so its correctness is the load-bearing invariant these tests pin.

use std::io;

mod common;

use common::is_addr_in_use;

/// The daemon-log line an rsync daemon emits when it loses the port race, as it
/// appears embedded in the readiness-timeout error (`wait_ready`). WHY it must
/// be retryable: losing the OS port to another process is transient and a fresh
/// port resolves it; refusing to retry here reinstates the original flake.
#[test]
fn addr_in_use_timeout_is_retryable() {
    let log = "IPv4 bind for 0.0.0.0:12345 failed: Address already in use \
               (os error 48); continuing with remaining address families";
    let err = io::Error::new(
        io::ErrorKind::TimedOut,
        format!("daemon on port 12345 did not become ready within 5s\nLog: {log}"),
    );
    assert!(
        is_addr_in_use(&err),
        "a readiness timeout whose embedded log shows a bind conflict must be retried"
    );
}

/// A bare EADDRINUSE error must classify as retryable too - the message text,
/// not the timeout wrapper, is what the classifier keys on.
#[test]
fn bare_eaddrinuse_is_retryable() {
    let err = io::Error::from(io::ErrorKind::AddrInUse);
    assert!(
        is_addr_in_use(&err),
        "EADDRINUSE is the port race by definition"
    );
}

/// A daemon that exits immediately for an unrelated reason must NOT be retried:
/// retrying would spin five times over a real, deterministic failure and then
/// report a misleading `AddrInUse` instead of the true cause. WHY this matters:
/// the retry exists to hide a race, never to hide a bug.
#[test]
fn unrelated_immediate_exit_is_not_retryable() {
    let err = io::Error::other(
        "daemon exited immediately with status: exit status: 1\n\
         Stderr: rsync: failed to parse config file: syntax error",
    );
    assert!(
        !is_addr_in_use(&err),
        "a genuine startup failure must propagate, not be masked by a port retry"
    );
}

/// A readiness timeout with no bind conflict in the log (e.g. the daemon is
/// simply slow, or the log is unavailable) must NOT be retried on a new port:
/// a fresh port cannot fix a daemon that binds fine but never answers.
#[test]
fn clean_timeout_is_not_retryable() {
    let err = io::Error::new(
        io::ErrorKind::TimedOut,
        "daemon on port 12345 did not become ready within 5s\nLog: (log unavailable)",
    );
    assert!(
        !is_addr_in_use(&err),
        "a timeout without a bind conflict is not a port race and must not retry"
    );
}

/// Matching is case-insensitive so a platform that capitalises the message
/// differently still triggers the retry.
#[test]
fn matching_is_case_insensitive() {
    let err = io::Error::other("bind failed: ADDRESS ALREADY IN USE");
    assert!(is_addr_in_use(&err));
}

/// Happy-path smoke test of the retry loop: with the oc-rsync binary built,
/// `TestDaemon::start` returns a daemon that is actually accepting connections.
/// This exercises the loop's success arm end-to-end. Skips gracefully when the
/// binary has not been built (local Mac runs), per the harness's degrade-not-
/// fail convention. Unix-only: daemon mode is unsupported on Windows
/// (`daemon.rs`: "daemon mode is not supported on this platform"), so this
/// cannot run there; the classifier tests above stay cross-platform.
#[cfg(unix)]
#[test]
fn start_yields_a_ready_daemon() {
    use std::path::Path;

    use common::{DaemonBinary, TestDaemon};

    let bin = test_support::oc_rsync_bin();
    if !Path::new(&bin).exists() {
        eprintln!("skipping: oc-rsync binary not built at {}", bin.display());
        return;
    }
    let daemon = TestDaemon::start(DaemonBinary::OcRsync).expect("daemon should start");
    assert_ne!(daemon.port(), 0, "daemon must report its bound port");
    assert!(
        daemon.url().contains(&daemon.port().to_string()),
        "url must reference the bound port"
    );
}
