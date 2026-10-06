//! Contract tests for `DaemonProcess`, the out-of-process daemon harness.
//!
//! Each test pins one property the daemon tests will lean on once they stop
//! running the daemon in-process: the reported port is the one this process
//! serves, a `--once` daemon's exit status is observable, every wait is
//! bounded, and the process never outlives the test - not even one that panics.

#![cfg(any(target_os = "linux", target_os = "macos"))]

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use tempfile::TempDir;
use test_support::{DaemonProcess, daemon_listen_port};

/// Bound for a `--once` daemon to finish after its single session.
const EXIT_BUDGET: Duration = Duration::from_secs(20);

/// Writes a config exporting one listable module, `probe`, rooted in `dir`.
fn write_config(dir: &TempDir) -> PathBuf {
    let module = dir.path().join("module");
    fs::create_dir_all(&module).expect("create module dir");
    let config = dir.path().join("rsyncd.conf");
    fs::write(
        &config,
        format!(
            "use chroot = false\n[probe]\n    path = {}\n    comment = harness probe\n",
            module.display()
        ),
    )
    .expect("write config");
    config
}

/// Whether `pid` still names a process, reaped or not.
///
/// `kill -0` also succeeds for a zombie, so a `false` here proves the process
/// was both terminated and reaped.
fn pid_exists(pid: u32) -> bool {
    Command::new("kill")
        .arg("-0")
        .arg(pid.to_string())
        .status()
        .expect("run kill -0")
        .success()
}

/// Completes the handshake and requests the module listing, returning every
/// line up to and including `@RSYNCD: EXIT`.
fn list_modules(daemon: &DaemonProcess) -> Vec<String> {
    let mut stream = daemon.connect().expect("connect");
    let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));
    let mut greeting = String::new();
    reader.read_line(&mut greeting).expect("read greeting");
    assert!(
        greeting.starts_with("@RSYNCD: "),
        "expected a daemon greeting, got {greeting:?}"
    );
    // upstream: clientserver.c - the daemon reads the client's own version
    // line before any request; echoing the server's greeting is a valid one.
    stream
        .write_all(greeting.as_bytes())
        .expect("send greeting");
    stream.write_all(b"#list\n").expect("send list request");

    let mut lines = Vec::new();
    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line).expect("read listing");
        assert!(n > 0, "daemon closed before @RSYNCD: EXIT; got {lines:?}");
        let done = line == "@RSYNCD: EXIT\n";
        lines.push(line);
        if done {
            return lines;
        }
    }
}

#[test]
fn reported_port_is_served_by_this_daemon_process() {
    let dir = TempDir::new().expect("tempdir");
    let daemon = DaemonProcess::spawn(&write_config(&dir), ["--once"]).expect("spawn daemon");

    assert_eq!(
        daemon_listen_port(daemon.id()),
        Some(daemon.port()),
        "the port must belong to the spawned pid, not merely accept connections"
    );
    let listing = list_modules(&daemon);
    assert!(
        listing
            .iter()
            .any(|l| l.starts_with("probe") && l.contains("harness probe")),
        "module from --config missing from listing: {listing:?}"
    );
}

#[test]
fn once_daemon_exits_successfully_after_its_session() {
    let dir = TempDir::new().expect("tempdir");
    let daemon = DaemonProcess::spawn(&write_config(&dir), ["--once"]).expect("spawn daemon");
    list_modules(&daemon);

    let status = daemon
        .wait_for_exit(EXIT_BUDGET)
        .expect("--once daemon exits");
    assert!(status.success(), "daemon exit status {status}");
}

#[test]
fn wait_for_exit_is_bounded_and_reaps_a_daemon_that_keeps_serving() {
    let dir = TempDir::new().expect("tempdir");
    let daemon = DaemonProcess::spawn(&write_config(&dir), std::iter::empty::<&str>())
        .expect("spawn daemon");
    let pid = daemon.id();

    let error = daemon
        .wait_for_exit(Duration::from_millis(200))
        .expect_err("a daemon without --once must still be serving");
    assert_eq!(error.kind(), std::io::ErrorKind::TimedOut, "{error}");
    assert!(!pid_exists(pid), "timed-out daemon {pid} was not reaped");
}

#[test]
fn a_panicking_test_does_not_orphan_its_daemon() {
    let dir = TempDir::new().expect("tempdir");
    let config = write_config(&dir);
    let mut pid = None;

    let outcome = panic::catch_unwind(AssertUnwindSafe(|| {
        let daemon =
            DaemonProcess::spawn(&config, std::iter::empty::<&str>()).expect("spawn daemon");
        pid = Some(daemon.id());
        panic!("simulated assertion failure while the daemon runs");
    }));

    assert!(outcome.is_err(), "the closure must have unwound");
    let pid = pid.expect("daemon was spawned before the panic");
    assert!(!pid_exists(pid), "daemon {pid} outlived the panicking test");
}

#[test]
fn startup_failure_reports_the_daemon_stderr() {
    let dir = TempDir::new().expect("tempdir");
    let missing: &Path = &dir.path().join("absent.conf");

    let error = DaemonProcess::spawn(missing, ["--once"])
        .expect_err("a daemon with an unreadable config must not become ready");
    let text = error.to_string();
    assert!(
        text.contains("absent.conf"),
        "error must carry the daemon's own diagnostic naming the config: {text}"
    );
}

#[test]
#[should_panic(expected = "always runs --no-detach")]
fn detach_is_refused() {
    let dir = TempDir::new().expect("tempdir");
    let _ = DaemonProcess::spawn(&write_config(&dir), ["--detach"]);
}
