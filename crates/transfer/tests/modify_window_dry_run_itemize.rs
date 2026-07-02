//! Regression coverage for `--modify-window` in a dry-run (`-n`) itemize.
//!
//! Upstream rsync applies its `same_time()` window tolerance
//! (`util1.c:1478`) inside the generator's quick-check
//! (`generator.c:645 mtime_differs()`) and itemize
//! (`generator.c:526`) decisions, and that path runs for a dry-run too
//! (only `dry_run > 1` at `generator.c:1290` short-circuits it, and that
//! only happens when the destination parent directory is missing).
//!
//! The receiver's wire path previously short-circuited every dry-run
//! candidate to a bare `ITEM_TRANSFER`, so a destination whose mtime was
//! within `--modify-window` seconds of the source was wrongly itemized as
//! `>f...` instead of being treated as up-to-date. This test drives the
//! actual `oc-rsync` binary against an `oc-rsync --daemon` over a loopback
//! socket - the only transport that exercises the wire generator - and
//! pins the itemize decision at the whole-second window boundary:
//!
//! - a 1s destination drift under `--modify-window=2` is absorbed (no row);
//! - a 3s drift exceeds the window and transfers (`>f`);
//! - `--modify-window=0` keeps exact whole-second semantics (1s => `>f`).
//!
//! Verified byte-for-byte against upstream rsync 3.4.4 over the same
//! loopback daemon during development.

#![cfg(unix)]

use std::env;
use std::fs;
use std::io;
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use filetime::{FileTime, set_file_mtime};
use tempfile::{TempDir, tempdir};

/// Maximum time the daemon has to start accepting connections.
const DAEMON_BOOT_TIMEOUT: Duration = Duration::from_secs(10);

/// A fixed base mtime for the source file, well clear of "now" so the
/// quick-check never races the wall clock.
const BASE_MTIME_SECS: i64 = 1_735_689_600; // 2025-01-01T00:00:00Z

/// Locate the workspace `oc-rsync` binary the test runner built.
fn locate_oc_rsync() -> Option<PathBuf> {
    if let Some(p) = env::var_os("CARGO_BIN_EXE_oc-rsync") {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Some(p);
        }
    }
    let exe = env::current_exe().ok()?;
    let mut dir = exe.parent()?;
    let name = format!("oc-rsync{}", env::consts::EXE_SUFFIX);
    while !dir.ends_with("target") {
        let candidate = dir.join(&name);
        if candidate.is_file() {
            return Some(candidate);
        }
        dir = dir.parent()?;
    }
    for sub in ["debug", "release"] {
        let candidate = dir.join(sub).join(&name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Allocate a free TCP port by binding to ephemeral port 0.
fn allocate_test_port() -> Option<u16> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0u16)).ok()?;
    let port = listener.local_addr().ok()?.port();
    drop(listener);
    Some(port)
}

/// Wait until the daemon accepts a TCP connection on `port`.
fn wait_for_daemon(port: u16) -> bool {
    let target = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let deadline = Instant::now() + DAEMON_BOOT_TIMEOUT;
    let mut backoff = Duration::from_millis(20);
    while Instant::now() < deadline {
        if TcpStream::connect_timeout(&target, Duration::from_millis(200)).is_ok() {
            return true;
        }
        thread::sleep(backoff);
        backoff = (backoff * 2).min(Duration::from_millis(200));
    }
    false
}

/// Guard that kills the daemon child on drop.
struct DaemonGuard {
    child: Child,
}

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Write a minimal read-only `rsyncd.conf` exposing one module.
fn write_daemon_config(
    config_path: &Path,
    pid_path: &Path,
    log_path: &Path,
    module_root: &Path,
) -> io::Result<()> {
    let body = format!(
        "pid file = {pid}\n\
         log file = {log}\n\
         use chroot = false\n\
         max connections = 4\n\
         \n\
         [mod]\n\
         path = {root}\n\
         read only = true\n\
         list = true\n",
        pid = pid_path.display(),
        log = log_path.display(),
        root = module_root.display(),
    );
    fs::write(config_path, body)
}

/// Spawn `oc-rsync --daemon` on `port` and wait until it binds.
fn spawn_oc_daemon(oc_bin: &Path, config_path: &Path, port: u16) -> io::Result<DaemonGuard> {
    let child = Command::new(oc_bin)
        .arg("--daemon")
        .arg("--no-detach")
        .arg("--port")
        .arg(port.to_string())
        .arg("--config")
        .arg(config_path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    if !wait_for_daemon(port) {
        let mut guard = DaemonGuard { child };
        let _ = guard.child.kill();
        return Err(io::Error::other(format!(
            "oc-rsync --daemon did not accept connections on port {port} within {DAEMON_BOOT_TIMEOUT:?}",
        )));
    }
    Ok(DaemonGuard { child })
}

/// Set an absolute whole-second mtime (nanoseconds zeroed) on `path`.
fn set_mtime_secs(path: &Path, secs: i64) {
    set_file_mtime(path, FileTime::from_unix_time(secs, 0)).expect("set mtime");
}

/// Run `oc-rsync -ain --modify-window=<window>` pulling the single module
/// file into `dest_dir` and return the file's itemize row, or `None` if
/// the file was treated as up-to-date (no row emitted).
fn dry_run_itemize_row(
    oc_bin: &Path,
    port: u16,
    window: u64,
    dest_dir: &Path,
) -> (Option<String>, String) {
    let window_arg = format!("--modify-window={window}");
    let source = format!("rsync://localhost:{port}/mod/file.txt");
    let output = Command::new(oc_bin)
        .arg("-ain")
        .arg(&window_arg)
        .arg(&source)
        .arg(format!("{}/", dest_dir.display()))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run oc-rsync dry-run");

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        output.status.success(),
        "dry-run exited non-zero: {:?}\nstdout:\n{stdout}\nstderr:\n{stderr}",
        output.status,
    );
    let row = stdout
        .lines()
        .find(|line| line.contains("file.txt"))
        .map(|line| line.to_string());
    (row, format!("stdout:\n{stdout}\nstderr:\n{stderr}"))
}

/// Build a source module holding a single `file.txt` at [`BASE_MTIME_SECS`]
/// plus a destination whose `file.txt` has identical bytes but an mtime
/// offset by `dest_offset_secs` seconds. Returns the temp dir plus the
/// module root so the daemon can serve it.
fn seed(dest_offset_secs: i64) -> (TempDir, PathBuf, PathBuf) {
    let temp = tempdir().expect("tempdir");
    let module_root = temp.path().join("src");
    let dest = temp.path().join("dst");
    fs::create_dir_all(&module_root).expect("create src");
    fs::create_dir_all(&dest).expect("create dst");

    let payload = b"identical content payload";
    fs::write(module_root.join("file.txt"), payload).expect("write src file");
    fs::write(dest.join("file.txt"), payload).expect("write dst file");

    set_mtime_secs(&module_root.join("file.txt"), BASE_MTIME_SECS);
    set_mtime_secs(&dest.join("file.txt"), BASE_MTIME_SECS + dest_offset_secs);

    (temp, module_root, dest)
}

/// End-to-end helper: spawn a daemon serving a freshly seeded module and
/// return the itemize row (or `None`) for the dry-run pull.
fn run_scenario(dest_offset_secs: i64, window: u64) -> (Option<String>, String) {
    let Some(oc_bin) = locate_oc_rsync() else {
        panic!("oc-rsync binary not found; build it before running this test");
    };
    let (temp, module_root, dest) = seed(dest_offset_secs);
    let config_path = temp.path().join("rsyncd.conf");
    let pid_path = temp.path().join("daemon.pid");
    let log_path = temp.path().join("daemon.log");
    write_daemon_config(&config_path, &pid_path, &log_path, &module_root)
        .expect("write daemon config");
    let port = allocate_test_port().expect("allocate port");
    let _daemon = spawn_oc_daemon(&oc_bin, &config_path, port).expect("spawn daemon");

    dry_run_itemize_row(&oc_bin, port, window, &dest)
}

/// A 1-second destination drift is inside `--modify-window=2`, so upstream
/// `same_time()` reports the mtimes as equal: quick-check treats the file as
/// up-to-date and itemize omits the time bit. The dry-run must emit NO row
/// (no `>f`) rather than the pre-fix bare `ITEM_TRANSFER`.
///
/// Guards: `generator.c:645 mtime_differs()` and `generator.c:526`.
#[test]
fn within_modify_window_is_unchanged_in_dry_run() {
    if SystemTime::UNIX_EPOCH.elapsed().is_err() {
        return; // pre-epoch clock; skip rather than misfire
    }
    let (row, ctx) = run_scenario(1, 2);
    assert!(
        row.is_none(),
        "1s drift within --modify-window=2 must be itemized as unchanged \
         (no `>f` row); got {row:?}\n{ctx}",
    );
}

/// A 3-second drift exceeds `--modify-window=2`, so the file must transfer:
/// the dry-run itemizes it with the `>f` update indicator.
#[test]
fn beyond_modify_window_transfers_in_dry_run() {
    let (row, ctx) = run_scenario(3, 2);
    let row = row.unwrap_or_else(|| panic!("expected a `>f` transfer row for a 3s drift\n{ctx}"));
    assert!(
        row.starts_with(">f"),
        "3s drift beyond --modify-window=2 must transfer (`>f`); got `{row}`\n{ctx}",
    );
}

/// `--modify-window=0` keeps exact whole-second semantics, so even a 1s
/// drift transfers. This pins the `window == 0` branch of the shared
/// `same_time()` helper (exact equality, no tolerance).
#[test]
fn zero_window_preserves_exact_mtime_in_dry_run() {
    let (row, ctx) = run_scenario(1, 0);
    let row = row.unwrap_or_else(|| {
        panic!("expected a `>f` transfer row for a 1s drift at window 0\n{ctx}")
    });
    assert!(
        row.starts_with(">f"),
        "1s drift at --modify-window=0 must transfer (`>f`); got `{row}`\n{ctx}",
    );
}

/// Identical mtimes at `--modify-window=0` remain up-to-date: no row.
/// Guards against an over-eager fix that would itemize matching files.
#[test]
fn identical_mtime_stays_unchanged_at_zero_window() {
    let (row, ctx) = run_scenario(0, 0);
    assert!(
        row.is_none(),
        "identical mtimes must stay unchanged (no row); got {row:?}\n{ctx}",
    );
}
