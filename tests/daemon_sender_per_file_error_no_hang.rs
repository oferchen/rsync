//! Regression: a daemon sender must never hang on a per-file/per-path error.
//!
//! Upstream reports the failure, keeps going, and finishes with
//! `RERR_PARTIAL` (23). Two independent daemon-only defects used to turn that
//! into an unbounded hang, and each one hid behind the other:
//!
//! 1. `main()` held the process-wide `StdoutLock`/`StderrLock` for the whole
//!    program. In TCP-daemon mode `main` then parked in the accept loop still
//!    holding them while every session ran on a spawned worker thread, so the
//!    first `eprintln!` on a worker - for example the `opendir` failure raised
//!    from the flist walk - blocked forever on a lock that was never released.
//! 2. The server sender entered `send_files()` even when the file list came
//!    out empty. Upstream bails out first (`main.c:981-987`), because a peer
//!    that received an empty list skips `do_recv()` entirely
//!    (`main.c:1397-1409`): it never writes an ndx, so the sender's next
//!    `read_ndx()` blocked until the peer's I/O timeout.
//!
//! Neither defect is reachable from a unit test: both need a real daemon
//! process whose sessions run on worker threads while `main` is elsewhere. So
//! these tests drive the shipped `oc-rsync` binary as both the daemon and the
//! client, over a real socket, and assert that each trigger finishes with exit
//! code 23. A hang fails as a nextest TIMEOUT naming the test.
//!
//! One test per trigger, so a regression in either defect is reported on its
//! own rather than being masked by whichever hang happens to fire first:
//! - unreadable source file - `chmod 000` file, fails inside `send_files`.
//! - unreadable source directory - `chmod 000` directory, fails inside the
//!   flist walk, which is where the stdio-lock deadlock fired.
//! - missing requested path - absent path, yields an empty file list, which is
//!   where the `send_files` deadlock fired.
//!
//! Each trigger is pulled twice: once by our own client, and once by an
//! upstream `rsync` client when a real 3.x binary is available. Both legs
//! matter. Our client keeps reading after an empty file list, so only the
//! upstream client - which skips its receive loop entirely and then waits in
//! `noop_io_until_death()` for our FIN - exposes the second deadlock. Driving
//! only our own binary would let a symmetric divergence hide the bug.
//!
//! Unix-only: the fault injection is POSIX DAC (`chmod 000`), which root
//! bypasses and Windows does not implement the same way. The two permission
//! triggers opt out under root; the missing-path trigger always runs.

#![cfg(unix)]

use std::fs;
use std::net::{Ipv4Addr, TcpListener};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use tempfile::TempDir;
use test_support::inetd_server::{InetdCommand, InetdServer};

/// Upstream `RERR_PARTIAL`: some files were not transferred (`errcode.h`).
const RERR_PARTIAL: i32 = 23;

/// Path to the binary under test, resolved by Cargo for integration tests.
fn oc_rsync_binary() -> &'static str {
    env!("CARGO_BIN_EXE_oc-rsync")
}

/// Locates an upstream rsync 3.x client to pull with, if one is installed.
///
/// Candidates, in order: an explicit `OC_RSYNC_UPSTREAM_RSYNC` override, the
/// binaries the interop harness installs under `target/interop`, then whatever
/// `rsync` is on `PATH`. Each candidate must self-identify as `rsync version
/// 3.x`, which rejects the `openrsync` shim macOS ships as `/usr/bin/rsync`
/// (protocol 29, different client semantics).
///
/// Returns `None` when nothing suitable exists, in which case the caller skips
/// that leg rather than failing - an upstream binary is an external resource.
fn upstream_rsync_client() -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(explicit) = std::env::var_os("OC_RSYNC_UPSTREAM_RSYNC") {
        candidates.push(PathBuf::from(explicit));
    }
    candidates.extend(
        ["3.4.4", "3.4.1", "3.1.3"]
            .into_iter()
            .filter_map(test_support::upstream_install_bin),
    );
    candidates.push(PathBuf::from("rsync"));

    candidates.into_iter().find(|candidate| {
        Command::new(candidate)
            .arg("--version")
            .output()
            .ok()
            .and_then(|out| String::from_utf8(out.stdout).ok())
            .is_some_and(|text| {
                text.lines()
                    .next()
                    .is_some_and(|line| line.starts_with("rsync") && line.contains(" version 3."))
            })
    })
}

/// A daemon served in inetd mode on a listener bound before any session
/// starts, so a client connect queues instead of racing the daemon's start.
/// Dropping it kills every session.
struct Daemon(InetdServer);

impl Daemon {
    fn spawn(config: &Path) -> Self {
        let stderr = fs::OpenOptions::new()
            .write(true)
            .open("/dev/null")
            .expect("open /dev/null");
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind daemon listener");
        let command = InetdCommand::new(oc_rsync_binary(), stderr)
            .arg("--config")
            .arg(config);
        Self(InetdServer::start(listener, command).expect("start daemon"))
    }

    fn url(&self, module: &str, path: &str) -> String {
        format!("rsync://127.0.0.1:{}/{module}/{path}", self.0.port())
    }
}

/// Restores `0o755` on the poisoned paths so `TempDir` cleanup can succeed
/// even when the test body panics.
struct PermissionRestorer(Vec<PathBuf>);

impl Drop for PermissionRestorer {
    fn drop(&mut self) {
        for path in &self.0 {
            let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o755));
        }
    }
}

/// True when the effective uid is 0, which bypasses the DAC checks the two
/// permission triggers rely on.
fn running_as_root() -> bool {
    // `id -u` keeps this test-only probe free of a libc dependency.
    Command::new("id")
        .arg("-u")
        .output()
        .ok()
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .is_some_and(|s| s.trim() == "0")
}

/// A read-only single-module daemon serving `module_root`.
struct Fixture {
    _root: TempDir,
    daemon: Daemon,
    dest: PathBuf,
}

impl Fixture {
    fn new(root: TempDir, module: &str, module_root: &Path) -> Self {
        let config = root.path().join("oc-rsyncd.conf");
        fs::write(
            &config,
            format!(
                "use chroot = no\n[{module}]\n    path = {}\n    read only = yes\n",
                module_root.display()
            ),
        )
        .expect("write daemon config");

        let dest = root.path().join("dest");
        fs::create_dir_all(&dest).expect("create dest");

        Self {
            daemon: Daemon::spawn(&config),
            dest,
            _root: root,
        }
    }
}

/// Runs one client pull and returns its exit code.
///
/// The nextest per-test timeout is the only bound: a hang under test fails
/// as a TIMEOUT naming the test.
fn run_client(client: &Path, fixture: &Fixture, module: &str, path: &str, label: &str) -> i32 {
    let out = Command::new(client)
        .arg("-a")
        .arg(fixture.daemon.url(module, path))
        .arg(&fixture.dest)
        .stdin(Stdio::null())
        .output()
        .expect("run oc-rsync client");
    for (name, text) in [("stdout", &out.stdout), ("stderr", &out.stderr)] {
        if !text.is_empty() {
            eprintln!("{label} client {name}:\n{}", String::from_utf8_lossy(text));
        }
    }

    out.status
        .code()
        .unwrap_or_else(|| panic!("{label}: client terminated by signal instead of exiting"))
}

/// Pulls `module/path` with our client and, when available, with an upstream
/// rsync client, asserting each run ends bounded with `RERR_PARTIAL`.
fn assert_bounded_partial_transfer(fixture: &Fixture, module: &str, path: &str, label: &str) {
    let mut clients: Vec<(String, PathBuf)> =
        vec![("oc-rsync".to_owned(), PathBuf::from(oc_rsync_binary()))];
    match upstream_rsync_client() {
        Some(upstream) => clients.push(("upstream-rsync".to_owned(), upstream)),
        None => eprintln!("skip upstream leg for {label}: no rsync 3.x client available"),
    }

    for (name, client) in clients {
        let leg = format!("{label} via {name}");
        let _ = fs::remove_dir_all(&fixture.dest);
        fs::create_dir_all(&fixture.dest).expect("create dest");
        let code = run_client(&client, fixture, module, path, &leg);
        assert_eq!(
            code, RERR_PARTIAL,
            "{leg}: expected upstream RERR_PARTIAL after a per-file error, got {code}"
        );
    }
}

#[test]
fn unreadable_source_file_reports_partial_transfer_without_hanging() {
    // upstream: sender.c:384-401 reports the open failure, sends MSG_NO_SEND,
    // and continues with the next ndx.
    if running_as_root() {
        eprintln!("skip: root bypasses the chmod 000 read denial");
        return;
    }

    let root = TempDir::new().expect("tempdir");
    let module_root = root.path().join("src");
    fs::create_dir_all(&module_root).expect("create module root");
    fs::write(module_root.join("good.txt"), b"ok\n").expect("write good.txt");
    let bad = module_root.join("bad.txt");
    fs::write(&bad, b"bad\n").expect("write bad.txt");
    let _restore = PermissionRestorer(vec![bad.clone()]);
    fs::set_permissions(&bad, fs::Permissions::from_mode(0o000)).expect("chmod 000 bad.txt");

    let fixture = Fixture::new(root, "mod", &module_root);
    assert_bounded_partial_transfer(&fixture, "mod", "", "unreadable-file");
}

#[test]
fn unreadable_source_directory_reports_partial_transfer_without_hanging() {
    // Deadlock 1: the flist walk's `eprintln!` for the failed directory ran on
    // a daemon worker thread and blocked on the `StderrLock` that `main` held
    // in the accept loop.
    if running_as_root() {
        eprintln!("skip: root bypasses the chmod 000 traversal denial");
        return;
    }

    let root = TempDir::new().expect("tempdir");
    let module_root = root.path().join("src");
    let locked = module_root.join("locked");
    fs::create_dir_all(&locked).expect("create module root");
    fs::write(module_root.join("good.txt"), b"ok\n").expect("write good.txt");
    fs::write(locked.join("inner.txt"), b"inner\n").expect("write inner.txt");
    let _restore = PermissionRestorer(vec![locked.clone()]);
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).expect("chmod 000 locked");

    let fixture = Fixture::new(root, "mod", &module_root);
    assert_bounded_partial_transfer(&fixture, "mod", "", "unreadable-dir");
}

#[test]
fn missing_requested_path_reports_partial_transfer_without_hanging() {
    // Deadlock 2: the requested path does not exist, so the file list is empty
    // and the peer never enters its receive loop. upstream main.c:981-987
    // returns from the server sender instead of calling send_files().
    let root = TempDir::new().expect("tempdir");
    let module_root = root.path().join("src");
    fs::create_dir_all(&module_root).expect("create module root");
    fs::write(module_root.join("good.txt"), b"ok\n").expect("write good.txt");

    let fixture = Fixture::new(root, "mod", &module_root);
    assert_bounded_partial_transfer(&fixture, "mod", "no-such-entry", "missing-path");
}
