//! `spawn_daemon_on_free_port` retries a lost bind race and nothing else.
//!
//! A fresh port can only fix a failure caused by the port. Retrying any other
//! startup failure repeats the same error many times and hides it behind a
//! slow, noisy run, so each branch is pinned against the real daemon:
//!
//! | case | daemon exit | attempts | outcome |
//! |---|---|---|---|
//! | port already held | bind failure code | 2 | ready on a fresh port |
//! | unreadable config | other | 1 | the first error |

#![cfg(any(target_os = "linux", target_os = "macos"))]

use std::cell::Cell;
use std::fs;
use std::net::{Ipv4Addr, TcpListener};
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use tempfile::TempDir;
use test_support::{
    BIND_FAILURE_EXIT_CODE, ReapOnDrop, daemon_listen_port, oc_rsync_bin, spawn_daemon_on_free_port,
};

/// Starts a daemon bound only to the IPv4 loopback, so losing that one address
/// is a total bind failure rather than a fall-back to IPv6.
fn spawn_loopback_daemon(config: &Path, port: u16) -> std::io::Result<Child> {
    Command::new(oc_rsync_bin())
        .args([
            "--daemon",
            "--no-detach",
            "--address",
            "127.0.0.1",
            "--port",
        ])
        .arg(port.to_string())
        .arg("--config")
        .arg(config)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
}

fn write_config(dir: &TempDir) -> std::path::PathBuf {
    let config = dir.path().join("rsyncd.conf");
    fs::write(&config, "use chroot = false\n").expect("write config");
    config
}

/// Waits up to 20 s for `child` to exit on its own; `Drop` reaps it otherwise.
fn wait_bounded(child: &mut ReapOnDrop) -> ExitStatus {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(status) = child.try_wait().expect("try_wait") {
            return status;
        }
        assert!(Instant::now() < deadline, "daemon did not exit within 20 s");
        thread::sleep(Duration::from_millis(10));
    }
}

/// The classifier's premise: a daemon that cannot bind exits with the bind
/// failure code, not with a generic one.
#[test]
fn a_daemon_on_a_held_port_exits_with_the_bind_failure_code() {
    let dir = test_support::create_tempdir();
    let held = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("hold a port");
    let port = held.local_addr().expect("held addr").port();

    let child = spawn_loopback_daemon(&write_config(&dir), port).expect("spawn daemon");
    let mut child = ReapOnDrop::new(child);
    let status = wait_bounded(&mut child);
    assert_eq!(status.code(), Some(BIND_FAILURE_EXIT_CODE), "{status}");
    drop(held);
}

#[test]
fn a_lost_bind_race_is_retried_on_a_fresh_port() {
    let dir = test_support::create_tempdir();
    let config = write_config(&dir);
    let attempts = Cell::new(0u32);
    let mut held = Vec::new();

    let (child, port) = spawn_daemon_on_free_port(|port| {
        attempts.set(attempts.get() + 1);
        if attempts.get() == 1 {
            // Another process wins the race for the first candidate port.
            held.push(TcpListener::bind((Ipv4Addr::LOCALHOST, port)).expect("take the port"));
        }
        spawn_loopback_daemon(&config, port)
    })
    .expect("the retry must recover from a lost bind race");
    let child = ReapOnDrop::new(child);

    assert_eq!(attempts.get(), 2, "exactly one retry after one lost race");
    let lost = held[0].local_addr().expect("held addr").port();
    assert_ne!(port, lost, "the retry must use a fresh port");
    assert_eq!(daemon_listen_port(child.id()), Some(port));
}

#[test]
fn a_bad_config_fails_after_exactly_one_attempt() {
    let dir = test_support::create_tempdir();
    let missing = dir.path().join("absent.conf");
    let attempts = Cell::new(0u32);

    let error = spawn_daemon_on_free_port(|port| {
        attempts.set(attempts.get() + 1);
        spawn_loopback_daemon(&missing, port)
    })
    .expect_err("an unreadable config cannot become ready");

    assert_eq!(
        attempts.get(),
        1,
        "a non-bind failure must not be retried: {error}"
    );
    assert!(
        error.to_string().contains("exited before binding"),
        "unexpected error: {error}"
    );
}
