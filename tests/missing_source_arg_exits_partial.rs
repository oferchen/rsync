//! Regression: a missing source argument must exit 23, driven by `MSG_ERROR_XFER`.
//!
//! Upstream's sender treats a source path that never existed as a transfer
//! error but deliberately keeps `io_error` clear for it (`flist.c:2668-2676`):
//!
//! ```c
//! if (errno != ENOENT || missing_args == 0) {
//!         /* This is a transfer error, but inhibit deletion
//!          * only if we might be omitting an existing file. */
//!         if (errno != ENOENT)
//!                 io_error |= IOERR_GENERAL;
//!         rsyserr(FERROR_XFER, errno, "link_stat %s failed", full_fname(fbuf));
//!         continue;
//! }
//! ```
//!
//! `io_error` is a wire field the receiver reads to decide whether to inhibit
//! its deletions, so upstream refuses to raise it here. That leaves the framed
//! `FERROR_XFER` as the *only* carrier of the failure: the peer's `read_a_msg`
//! routes `MSG_ERROR_XFER` to `rwrite` (`io.c:1686`), which sets
//! `got_xfer_error` (`log.c:310-311`), and `cleanup.c:217-218` turns that into
//! `RERR_PARTIAL`.
//!
//! Two coupled defects hid in that gap and had to be fixed together:
//!
//! 1. oc had no `got_xfer_error`, so a received `MSG_ERROR_XFER` changed
//!    nothing. Against a real upstream daemon - whose `io_error` is provably
//!    clear here - our client exited 0.
//! 2. oc's sender raised `IOERR_GENERAL` for `ENOENT` anyway, which is what
//!    made an oc-to-oc pull *look* correct. Removing it alone would have turned
//!    that pair from accidentally right into plainly wrong.
//!
//! The upstream-sender legs below are what pin the mechanism rather than the
//! number: upstream sends no `io_error` bit for this case, so a client that
//! exits 23 against it can only have got there through the frame.
//!
//! SSH masks the divergence - the child process's own exit status rescues the
//! client - so every leg here runs over a daemon.
// Windows refuses --daemon (cli frontend/server/daemon.rs:181-219), and every leg
// here runs over a daemon.
#![cfg(unix)]

use std::fs;
use std::net::{Ipv4Addr, TcpListener};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use tempfile::TempDir;
use test_support::inetd_server::{InetdCommand, InetdServer};

/// Upstream `RERR_PARTIAL`: some files were not transferred (`errcode.h`).
const RERR_PARTIAL: i32 = 23;

/// Path to the binary under test, resolved by Cargo at compile time so a stale
/// build in `target/` can never be picked up by a directory scan.
fn oc_rsync_binary() -> &'static str {
    env!("CARGO_BIN_EXE_oc-rsync")
}

/// Locates an upstream rsync 3.x binary, if one is installed.
///
/// Candidates, in order: an explicit `OC_RSYNC_UPSTREAM_RSYNC` override, the
/// binaries the interop harness installs under `target/interop`, then whatever
/// `rsync` is on `PATH`. Each must self-identify as `rsync version 3.x`, which
/// rejects the `openrsync` shim macOS ships as `/usr/bin/rsync`.
///
/// Returns `None` when nothing suitable exists; the caller skips those legs
/// rather than failing, since an upstream binary is an external resource.
fn upstream_rsync() -> Option<PathBuf> {
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
    fn spawn(binary: &Path, config: &Path) -> Self {
        let stderr = fs::OpenOptions::new()
            .write(true)
            .open("/dev/null")
            .expect("open /dev/null");
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind daemon listener");
        let command = InetdCommand::new(binary, stderr)
            .arg("--config")
            .arg(config);
        Self(InetdServer::start(listener, command).expect("start daemon"))
    }

    fn url(&self, path: &str) -> String {
        format!("rsync://127.0.0.1:{}/mod/{path}", self.0.port())
    }
}

/// Writes a writable single-module daemon config serving `module_root`, so the
/// same daemon serves both the pull and the push leg.
fn write_config(dir: &Path, module_root: &Path) -> PathBuf {
    let config = dir.join("rsyncd.conf");
    fs::write(
        &config,
        format!(
            "use chroot = no\n[mod]\n    path = {}\n    read only = no\n",
            module_root.display()
        ),
    )
    .expect("write daemon config");
    config
}

/// Outcome of one client run.
struct ClientRun {
    code: i32,
    output: String,
}

/// Runs one client to completion and returns its exit code and combined output.
///
/// The nextest per-test timeout is the only bound: a hang under test fails
/// as a TIMEOUT naming the test.
fn run_client(binary: &Path, args: &[String], label: &str) -> ClientRun {
    let out = Command::new(binary)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("run rsync client");
    let output = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    eprintln!("{label} client output:\n{output}");

    let code = out
        .status
        .code()
        .unwrap_or_else(|| panic!("{label}: client terminated by signal instead of exiting"));
    ClientRun { code, output }
}

/// Every peer role the matrix is run for: always our own binary, plus a real
/// upstream rsync when one is installed. Both matter - an oc-to-oc pair shares
/// any divergence between the two halves of this fix and so cannot detect it.
fn peers() -> Vec<(String, PathBuf)> {
    let mut peers = vec![("oc-rsync".to_owned(), PathBuf::from(oc_rsync_binary()))];
    match upstream_rsync() {
        Some(upstream) => peers.push(("upstream-rsync".to_owned(), upstream)),
        None => eprintln!("skip upstream legs: no rsync 3.x binary available"),
    }
    peers
}

/// Creates a populated module root plus an empty destination.
fn fixture() -> (TempDir, PathBuf, PathBuf) {
    let root = TempDir::new().expect("tempdir");
    let module_root = root.path().join("src");
    let dest = root.path().join("dest");
    fs::create_dir_all(&module_root).expect("create module root");
    fs::create_dir_all(&dest).expect("create dest");
    fs::write(module_root.join("present.txt"), b"ok\n").expect("write present.txt");
    (root, module_root, dest)
}

/// Asserts the one thing every leg must show: the upstream `link_stat` line,
/// reported exactly once, and `RERR_PARTIAL`.
fn assert_partial_transfer(run: &ClientRun, label: &str) {
    assert_eq!(
        run.code, RERR_PARTIAL,
        "{label}: a missing source argument must exit {RERR_PARTIAL} \
         (upstream cleanup.c:217-218 via got_xfer_error), got {}",
        run.code
    );
    let reports = run.output.matches("link_stat").count();
    assert_eq!(
        reports, 1,
        "{label}: upstream rsyserr()s the failure once (flist.c:2673); \
         reporting it through both a MSG_ERROR_XFER frame and an io_error bit \
         would surface it twice"
    );
}

/// Pull leg: the daemon is the sender, so the `link_stat` failure happens on
/// the far side and only `MSG_ERROR_XFER` can carry it back. This is the cell
/// that exited 0 before `got_xfer_error` existed.
#[test]
fn daemon_pull_of_a_missing_path_exits_partial() {
    let (root, module_root, dest) = fixture();
    let config = write_config(root.path(), &module_root);

    for (sender_name, sender) in peers() {
        let daemon = Daemon::spawn(&sender, &config);
        for (client_name, client) in peers() {
            let label = format!("pull: {client_name} client -> {sender_name} daemon");
            let args = vec![
                "-a".to_owned(),
                daemon.url("no-such-entry"),
                format!("{}/", dest.display()),
            ];
            let run = run_client(&client, &args, &label);
            assert_partial_transfer(&run, &label);
        }
    }
}

/// Push leg: the local sender raises the error itself. Upstream's `rwrite()`
/// sets `got_xfer_error` on the emitting side too (`log.c:310-311` runs before
/// the `am_server` frame-and-return), so a client that only *reports* the
/// failure still exits 23 - without the `io_error` bit that used to stand in
/// for it here.
#[test]
fn daemon_push_of_a_missing_path_exits_partial() {
    let (root, module_root, _dest) = fixture();
    let config = write_config(root.path(), &module_root);
    let missing = root.path().join("no-such-source");

    for (receiver_name, receiver) in peers() {
        let daemon = Daemon::spawn(&receiver, &config);
        for (client_name, client) in peers() {
            let label = format!("push: {client_name} client -> {receiver_name} daemon");
            let args = vec![
                "-a".to_owned(),
                missing.display().to_string(),
                daemon.url(""),
            ];
            let run = run_client(&client, &args, &label);
            assert_partial_transfer(&run, &label);
        }
    }
}
