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
//!    out empty. Upstream bails out first (`main.c:968-974`), because a peer
//!    that received an empty list skips `do_recv()` entirely
//!    (`main.c:1379-1391`): it never writes an ndx, so the sender's next
//!    `read_ndx()` blocked until the peer's I/O timeout.
//!
//! Neither defect is reachable from a unit test: both need a real daemon
//! process whose sessions run on worker threads while `main` is elsewhere. So
//! these tests drive the shipped `oc-rsync` binary as both the daemon and the
//! client, over a real socket, and assert that each trigger finishes inside a
//! bounded deadline with exit code 23.
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
use std::io::{BufRead, BufReader, Read};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use tempfile::TempDir;

/// Upstream `RERR_PARTIAL`: some files were not transferred (`errcode.h`).
const RERR_PARTIAL: i32 = 23;

/// Wall-clock budget for one client run. The transfers here move a handful of
/// tiny files, so anything approaching this bound is a hang, not slowness. It
/// is deliberately well under upstream's own 60 s `noop_io_until_death()`
/// fallback timeout, so a wedged daemon fails the assertion instead of being
/// rescued by the peer's timeout.
const CLIENT_DEADLINE: Duration = Duration::from_secs(45);

/// How long to wait for the spawned daemon to bind its listening socket.
const DAEMON_READY_TIMEOUT: Duration = Duration::from_secs(30);

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

/// Reserves an ephemeral port by binding it and immediately releasing it.
fn allocate_port() -> u16 {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0u16)).expect("bind ephemeral port");
    let port = listener.local_addr().expect("local addr").port();
    drop(listener);
    port
}

/// One end of a child's stdio, boxed so both pipes share a drain loop.
enum Pipe {
    Out(std::process::ChildStdout),
    Err(std::process::ChildStderr),
}

impl Pipe {
    fn into_reader(self) -> Box<dyn Read> {
        match self {
            Self::Out(out) => Box::new(out),
            Self::Err(err) => Box::new(err),
        }
    }
}

/// A spawned `oc-rsync --daemon` process whose stdio is drained continuously.
///
/// Draining is not optional: an undrained pipe wedges the daemon once the OS
/// buffer fills, which would masquerade as the very hang under test.
struct Daemon {
    child: Child,
    port: u16,
}

impl Daemon {
    fn spawn(config: &Path) -> Self {
        let port = allocate_port();
        let mut child = Command::new(oc_rsync_binary())
            .arg("--daemon")
            .arg("--no-detach")
            .arg("--config")
            .arg(config)
            .arg("--port")
            .arg(port.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn oc-rsync daemon");

        for pipe in [
            child.stdout.take().map(Pipe::Out),
            child.stderr.take().map(Pipe::Err),
        ]
        .into_iter()
        .flatten()
        {
            thread::spawn(move || {
                let mut sink = Vec::new();
                let _ = pipe.into_reader().read_to_end(&mut sink);
            });
        }

        let daemon = Self { child, port };
        daemon.wait_until_listening();
        daemon
    }

    /// Polls the listening socket until the daemon accepts connections.
    fn wait_until_listening(&self) {
        let target = SocketAddr::from((Ipv4Addr::LOCALHOST, self.port));
        let deadline = Instant::now() + DAEMON_READY_TIMEOUT;
        while Instant::now() < deadline {
            if TcpStream::connect_timeout(&target, Duration::from_millis(250)).is_ok() {
                return;
            }
            thread::sleep(Duration::from_millis(50));
        }
        panic!("daemon did not start listening on port {}", self.port);
    }

    fn url(&self, module: &str, path: &str) -> String {
        format!("rsync://127.0.0.1:{}/{module}/{path}", self.port)
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
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

/// Runs one client pull and returns its exit code, failing the test if it does
/// not finish inside [`CLIENT_DEADLINE`].
///
/// Both child pipes are drained on their own threads while we wait. Polling
/// `try_wait()` without draining deadlocks the moment the child fills a pipe
/// buffer, which would hang this harness for a reason unrelated to the bug
/// under test.
fn run_client_bounded(
    client: &Path,
    fixture: &Fixture,
    module: &str,
    path: &str,
    label: &str,
) -> i32 {
    let mut child = Command::new(client)
        .arg("-a")
        .arg(fixture.daemon.url(module, path))
        .arg(&fixture.dest)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn oc-rsync client");

    let (tx, rx) = mpsc::channel::<(&'static str, String)>();
    for (name, pipe) in [
        ("stdout", child.stdout.take().map(Pipe::Out)),
        ("stderr", child.stderr.take().map(Pipe::Err)),
    ] {
        let Some(pipe) = pipe else { continue };
        let tx = tx.clone();
        thread::spawn(move || {
            let mut text = String::new();
            for line in BufReader::new(pipe.into_reader())
                .lines()
                .map_while(Result::ok)
            {
                text.push_str(&line);
                text.push('\n');
            }
            let _ = tx.send((name, text));
        });
    }
    drop(tx);

    let deadline = Instant::now() + CLIENT_DEADLINE;
    let status = loop {
        match child.try_wait().expect("poll client") {
            Some(status) => break status,
            None => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!(
                        "{label}: daemon sender hung - client did not finish within {CLIENT_DEADLINE:?}"
                    );
                }
                thread::sleep(Duration::from_millis(25));
            }
        }
    };

    for (name, text) in rx {
        if !text.is_empty() {
            eprintln!("{label} client {name}:\n{text}");
        }
    }

    status
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
        let code = run_client_bounded(&client, fixture, module, path, &leg);
        assert_eq!(
            code, RERR_PARTIAL,
            "{leg}: expected upstream RERR_PARTIAL after a per-file error, got {code}"
        );
    }
}

#[test]
fn unreadable_source_file_reports_partial_transfer_without_hanging() {
    // upstream: sender.c:383-400 reports the open failure, sends MSG_NO_SEND,
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
    // and the peer never enters its receive loop. upstream main.c:968-974
    // returns from the server sender instead of calling send_files().
    let root = TempDir::new().expect("tempdir");
    let module_root = root.path().join("src");
    fs::create_dir_all(&module_root).expect("create module root");
    fs::write(module_root.join("good.txt"), b"ok\n").expect("write good.txt");

    let fixture = Fixture::new(root, "mod", &module_root);
    assert_bounded_partial_transfer(&fixture, "mod", "no-such-entry", "missing-path");
}
