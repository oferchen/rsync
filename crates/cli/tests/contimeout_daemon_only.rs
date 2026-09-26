//! `--contimeout` is a daemon connection timeout.
//!
//! upstream: main.c:1623-1627 start_client() - `if (connect_timeout &&
//! !daemon_connection)` rejects the option with RERR_SYNTAX, so it is refused
//! for a local copy and for a plain remote-shell transfer. A daemon reached
//! through `--rsh` still counts as a daemon connection (`daemon_connection ==
//! 1`), so it keeps the option, and main.c:1655-1670 then bounds spawning the
//! helper, its connect and the daemon greeting with it, exiting
//! RERR_CONTIMEOUT (35) once the deadline passes (io.c:169-173).
//!
//! These cells drive the real binary because the rejection and the deadline
//! live on two different entry points (option validation and the
//! daemon-over-rsh transport).

use std::process::{Command, Output};
#[cfg(unix)]
use std::time::{Duration, Instant};

use tempfile::TempDir;
use test_support::oc_rsync_bin;

const REJECTED: &str = "may only be used when connecting to an rsync daemon";

fn combined(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn run(args: &[String]) -> Output {
    Command::new(oc_rsync_bin())
        .args(args)
        .output()
        .expect("spawn oc-rsync")
}

#[test]
fn local_copy_rejects_contimeout() {
    let tmp = TempDir::new().expect("tempdir");
    let output = run(&[
        "--contimeout=5".to_owned(),
        tmp.path().join("src").display().to_string(),
        tmp.path().join("dst").display().to_string(),
    ]);
    let text = combined(&output);
    assert_eq!(output.status.code(), Some(1), "got: {text}");
    assert!(text.contains(REJECTED), "got: {text}");
}

#[test]
fn remote_shell_transfer_rejects_contimeout() {
    // upstream: testsuite/contimeout-rsh_test.py - the guard runs before any
    // remote shell is spawned, so no ssh is needed to observe it.
    let tmp = TempDir::new().expect("tempdir");
    let output = run(&[
        "--contimeout=5".to_owned(),
        "-a".to_owned(),
        tmp.path().join("src").display().to_string(),
        format!("localhost:{}", tmp.path().join("dst").display()),
    ]);
    let text = combined(&output);
    assert_eq!(output.status.code(), Some(1), "got: {text}");
    assert!(text.contains(REJECTED), "got: {text}");
}

#[test]
fn zero_contimeout_is_not_rejected() {
    // upstream tests `connect_timeout` for non-zero, so `--contimeout=0` (no
    // timeout) is accepted everywhere.
    let tmp = TempDir::new().expect("tempdir");
    let src = tmp.path().join("src");
    std::fs::create_dir(&src).expect("create src");
    std::fs::write(src.join("f"), b"x").expect("write");
    let output = run(&[
        "--contimeout=0".to_owned(),
        "-r".to_owned(),
        format!("{}/", src.display()),
        tmp.path().join("dst").display().to_string(),
    ]);
    assert!(output.status.success(), "got: {}", combined(&output));
}

/// Writes an executable `--rsh` helper with the given shell body.
#[cfg(unix)]
fn rsh_helper(dir: &std::path::Path, name: &str, body: &str) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("write helper");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    path
}

#[cfg(unix)]
#[test]
fn daemon_over_rsh_keeps_contimeout() {
    // rsync-ssl's shape of call: a daemon URL reached through --rsh. The
    // helper fails at once, so the run fails, but not on the option guard.
    let tmp = TempDir::new().expect("tempdir");
    let helper = rsh_helper(tmp.path(), "fail-rsh", "exit 1");
    let output = run(&[
        "--contimeout=5".to_owned(),
        format!("--rsh={}", helper.display()),
        "-a".to_owned(),
        "rsync://127.0.0.1:9/mod/".to_owned(),
        tmp.path().join("dest").display().to_string(),
    ]);
    let text = combined(&output);
    assert!(!output.status.success(), "got: {text}");
    assert!(
        !text.contains(REJECTED),
        "--contimeout was rejected for a daemon-via-rsh connection: {text}"
    );
}

#[cfg(unix)]
#[test]
fn daemon_over_rsh_times_out_a_helper_that_never_answers() {
    // upstream: testsuite/contimeout-rsh_test.py - the helper sleeps instead of
    // connecting, so only --contimeout can end the run. Its `sleep` child keeps
    // the pipe open after the shell itself is killed, so a deadline that merely
    // killed the spawned process would still hang here.
    let tmp = TempDir::new().expect("tempdir");
    let helper = rsh_helper(tmp.path(), "hang-rsh", "exec 2>/dev/null\nsleep 30");
    let start = Instant::now();
    let output = run(&[
        "--contimeout=1".to_owned(),
        format!("--rsh={}", helper.display()),
        "-a".to_owned(),
        "rsync://127.0.0.1:9/mod/".to_owned(),
        tmp.path().join("dest").display().to_string(),
    ]);
    let elapsed = start.elapsed();
    let text = combined(&output);
    assert_eq!(output.status.code(), Some(35), "got: {text}");
    assert!(
        text.contains("connection timed out -- exiting"),
        "expected upstream's timeout text, got: {text}"
    );
    assert!(
        elapsed < Duration::from_secs(15),
        "--contimeout=1 took {elapsed:?}; the deadline did not bound the connection"
    );
}
