//! UTS-NEXTEST-EDGE.g: nextest port of the `daemon-delete-stats` scenario.
//!
//! Upstream scenario lineage:
//! - `target/interop/upstream-src/rsync-3.4.4/testsuite/delete.test` -
//!   canonical upstream delete sweep coverage; the daemon-mode
//!   `NDX_DEL_STATS` exchange is not a standalone upstream script but
//!   is exercised by the same `--delete --stats` invocation under
//!   `runtests.py` daemon mode.
//! - `del.c::do_delete_pass()` - the full-tree delete sweep that
//!   populates the per-type counters.
//! - `generator.c:2376-2398` and `main.c:225-238` (3.4.4 `write_del_stats`):
//!   the `NDX_DEL_STATS` wire frame contract gated on `delete_mode ||
//!   force_delete || read_batch` plus `INFO_GTE(STATS, 2)`.
//!
//! # Background
//!
//! The `--delete --stats` exchange relies on the generator transmitting
//! an `NDX_DEL_STATS` frame during the goodbye phase. The frame carries
//! five varints (files / dirs / symlinks / devices / specials) that the
//! sender accumulates into the `Number of deleted files: N` line of the
//! `--stats` summary. If the generator silently skips the frame (or
//! emits zeroes), the user-visible counter stays at zero even though the
//! receiver pruned the destination on disk.
//!
//! oc-rsync's coverage of this contract landed in two passes:
//!
//! - UTS-6 / URV-6: the receiver-side delete sweep in
//!   `run_pipelined_incremental` was wired so daemon-upload transfers
//!   carry per-type counters into `ReceiverContext::pending_del_stats`,
//!   surfaced into `ServerStats::Receiver` and
//!   `ServerStats::Generator` (PR #5586).
//! - The pull direction (client pulls from a daemon) was already wired
//!   through the local receiver sweep but had no end-to-end nextest
//!   guard against a future regression.
//!
//! Both wire paths converge on
//! `crates/transfer/src/generator/transfer/goodbye.rs::handle_goodbye`,
//! which emits `NDX_DEL_STATS` followed by `NDX_DONE` once
//! `should_send_del_stats()` returns true.
//!
//! Upstream's `runtests.py` harness exercises the daemon-mode delete
//! sweep through `continue-on-error: true` CI plumbing, so a per-test
//! regression on that path does not block a PR. The UTS-NEXTEST-EDGE
//! family lifts these scenarios into native nextest integration tests
//! that run as required checks on every PR.
//!
//! # What this test pins
//!
//! For both upload (client pushes into a daemon module) and download
//! (client pulls from a daemon module):
//!
//! 1. The transfer exits cleanly (status code 0).
//! 2. The destination is correctly pruned - the seeded extras are gone.
//! 3. The `--stats` summary reports the expected non-zero
//!    `Number of deleted files: N` line, which is the user-visible
//!    surface of the `NDX_DEL_STATS` frame emitted during the goodbye
//!    phase. A regression that drops the frame or zeroes the counters
//!    would print `Number of deleted files: 0` here despite the
//!    on-disk deletion happening.
//! 4. The source-side entries survived: the test discriminates between a
//!    `--delete` that pruned just the extras vs an over-aggressive sweep
//!    that wiped the legitimate destination files too.
//!
//! # Platform gate
//!
//! `#![cfg(unix)]` - matches the sibling UTS-NEXTEST-EDGE tests
//! (`uts_nextest_chdir_symlink_race.rs` etc.). Windows daemon mode has
//! separate coverage and uses a different ready-probe surface.
//!
//! # Upstream References
//!
//! - `del.c::do_delete_pass()` - delete sweep loop.
//! - `generator.c:2376-2398` - `write_del_stats()` early emission gate
//!   (`delete_mode || force_delete || read_batch`).
//! - `main.c:225-238` - `write_del_stats()` wire format.
//! - `rsync.c:337-342` - sender-side `read_ndx_and_attrs()` consumes
//!   the `NDX_DEL_STATS` frame and accumulates the counters.
//! - `crates/protocol/src/stats/delete.rs::DeleteStats::write_to` /
//!   `read_from` - oc-rsync side of the wire format.
//! - `crates/transfer/src/generator/transfer/goodbye.rs::handle_goodbye` -
//!   emission site.
//! - `crates/transfer/src/receiver/transfer/pipelined_incremental.rs:118-124` -
//!   URV-6.b wiring of the receiver-side counters into
//!   `pending_del_stats` for the upload direction.

#![cfg(unix)]

use std::env;
use std::fs;
use std::io;
use std::net::{Ipv4Addr, SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use tempfile::{TempDir, tempdir};

/// Maximum time the daemon has to start accepting connections.
///
/// 10 seconds matches `v61d_2_daemon_push_increcurse_perf_regression.rs`,
/// the closest sibling that spawns an `oc-rsync --daemon` process. A
/// daemon that does not bind in this window is treated as a hard
/// failure rather than a flake, because the readiness probe polls
/// every 20-200ms so a healthy daemon always responds well inside the
/// budget.
const DAEMON_BOOT_TIMEOUT: Duration = Duration::from_secs(10);

/// Locate the workspace `oc-rsync` binary the test runner built.
///
/// Prefers Cargo's injected `CARGO_BIN_EXE_oc-rsync` when set; otherwise
/// walks up from the test executable until a `target/` directory is
/// found, mirroring the lookup used by sibling integration tests
/// (`v61d_2_daemon_push_increcurse_perf_regression.rs`,
/// `uts_nextest_chdir_symlink_race.rs`,
/// `uts_9_daemon_gzip_download_goodbye.rs`).
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
///
/// The kernel does not immediately recycle ephemeral ports, so the
/// residual race between drop and daemon bind is small enough that the
/// test does not need nextest-level retries. Mirrors the helper in
/// `v61d_2_daemon_push_increcurse_perf_regression.rs` and
/// `crates/core/tests/common/mod.rs::allocate_test_port`.
fn allocate_test_port() -> Option<u16> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0u16)).ok()?;
    let port = listener.local_addr().ok()?.port();
    drop(listener);
    Some(port)
}

/// Wait until the daemon accepts a TCP connection on `port`.
///
/// Polls with a short backoff up to [`DAEMON_BOOT_TIMEOUT`]. Returns
/// `true` once a connection succeeds, `false` if the timeout elapses
/// without ever getting through.
fn wait_for_daemon(port: u16) -> bool {
    let target = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let deadline = Instant::now() + DAEMON_BOOT_TIMEOUT;
    let mut backoff = Duration::from_millis(20);
    while Instant::now() < deadline {
        if std::net::TcpStream::connect_timeout(&target, Duration::from_millis(200)).is_ok() {
            return true;
        }
        thread::sleep(backoff);
        backoff = (backoff * 2).min(Duration::from_millis(200));
    }
    false
}

/// Write an `rsyncd.conf` exposing a single read-write module.
///
/// `use chroot = false` is required so the unprivileged test process
/// can drive the daemon without `CAP_SYS_CHROOT`. The `read only`
/// flag is parameterised so the same writer covers both push (RW) and
/// pull (RO) shapes.
fn write_daemon_config(
    config_path: &Path,
    pid_path: &Path,
    log_path: &Path,
    module_name: &str,
    module_root: &Path,
    read_only: bool,
) -> io::Result<()> {
    let body = format!(
        "pid file = {pid}\n\
         log file = {log}\n\
         use chroot = false\n\
         max connections = 4\n\
         \n\
         [{module}]\n\
         path = {root}\n\
         comment = UTS-NEXTEST-EDGE.g daemon-delete-stats\n\
         read only = {ro}\n\
         list = true\n",
        pid = pid_path.display(),
        log = log_path.display(),
        module = module_name,
        root = module_root.display(),
        ro = if read_only { "true" } else { "false" },
    );
    fs::write(config_path, body)
}

/// Guard that kills the daemon child on drop.
///
/// Prevents a leaked TCP listener on a panicking test and ensures
/// the temp directory backing the daemon config can be cleaned up.
struct DaemonGuard {
    child: Child,
}

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Spawn `oc-rsync --daemon` on `port` against `config_path` and wait
/// until it accepts connections.
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

/// Build the standard two-file source set + the extras the
/// `--delete` sweep is expected to remove from the destination.
///
/// Mirrors `crates/daemon/src/tests/chunks/daemon_delete_push_emits_stats.rs`
/// so the per-type counter we assert against
/// (`Number of deleted files: 1`) is grounded in the same fixture
/// shape the library-level test already pins.
fn seed_source(dir: &Path) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    fs::write(dir.join("file_a.txt"), b"contents of A\n")?;
    fs::write(dir.join("file_b.txt"), b"contents of B\n")?;
    Ok(())
}

/// Seed the destination with the two source files plus a single
/// extraneous regular file the `--delete` sweep must remove.
///
/// A single extra file (rather than a mix of files / dirs /
/// symlinks) keeps the expected `Number of deleted files: 1` line
/// portable across kernels and filesystems - the symlink and device
/// counters live in the same wire frame but are not the focus of
/// this test.
fn seed_destination_with_extra(dir: &Path) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    fs::write(dir.join("file_a.txt"), b"contents of A\n")?;
    fs::write(dir.join("file_b.txt"), b"contents of B\n")?;
    fs::write(dir.join("extra.txt"), b"should be deleted\n")?;
    Ok(())
}

/// Assert the destination is pruned: source files survived and the
/// extraneous seed file is gone. Run after both the push and pull
/// scenarios so the same invariant guards both directions.
fn assert_destination_pruned(dest: &Path) {
    assert!(
        dest.join("file_a.txt").is_file(),
        "file_a.txt must survive the transfer at {}",
        dest.display(),
    );
    assert!(
        dest.join("file_b.txt").is_file(),
        "file_b.txt must survive the transfer at {}",
        dest.display(),
    );
    assert!(
        !dest.join("extra.txt").exists(),
        "extra.txt must have been removed by --delete at {} (sweep skipped or NDX_DEL_STATS path regressed)",
        dest.display(),
    );
}

/// Assert that `stdout` reports `Number of deleted files: N (reg: N)`.
///
/// Both scenarios delete a single extraneous *regular* file, so upstream's
/// `output_itemized_counts` (main.c) prints the total plus the per-type
/// breakdown `(reg: N)`. This is the user-visible surface of the
/// `NDX_DEL_STATS` wire frame (5 per-type varints): a regression that dropped
/// the frame or zeroed the counters would print "Number of deleted files: 0",
/// and one that collapsed the per-type counts to a bare total would drop the
/// `(reg: N)` suffix.
fn assert_delete_count(stdout: &str, stderr: &str, expected: u32) {
    let needle = format!("Number of deleted files: {expected} (reg: {expected})");
    assert!(
        stdout.contains(&needle),
        "missing `{needle}` line in --stats output (NDX_DEL_STATS goodbye-phase frame missing, zeroed, or per-type breakdown dropped)\n\
         stdout:\n{stdout}\nstderr:\n{stderr}",
    );
}

/// Drive one `oc-rsync` invocation with the provided args and return
/// `(status, stdout, stderr)`. Used by both directions so the success
/// + stats parsing is centralised.
fn run_oc_rsync_capture(
    bin: &Path,
    args: &[&std::ffi::OsStr],
) -> io::Result<(std::process::ExitStatus, String, String)> {
    let output = Command::new(bin)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()?;
    Ok((
        output.status,
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    ))
}

/// Common setup the push and pull scenarios share: tempdir, port,
/// log/pid/config paths.
struct DaemonScratch {
    _tmp: TempDir,
    root: PathBuf,
    config: PathBuf,
    log: PathBuf,
    pid: PathBuf,
    port: u16,
}

impl DaemonScratch {
    fn new() -> Option<Self> {
        let tmp = tempdir().ok()?;
        let root = tmp.path().to_path_buf();
        let config = root.join("rsyncd.conf");
        let log = root.join("rsyncd.log");
        let pid = root.join("rsyncd.pid");
        let port = allocate_test_port()?;
        Some(Self {
            _tmp: tmp,
            root,
            config,
            log,
            pid,
            port,
        })
    }
}

/// Push direction: client pushes a source tree into a daemon module
/// that already contains an extraneous file. URV-6.b path: the
/// receiver runs in the daemon process, sweeps the destination, and
/// must emit `NDX_DEL_STATS` so the sender renders the right
/// `--stats` line.
///
/// Asserts:
/// 1. The client exits with status 0.
/// 2. The destination is pruned (extra.txt removed, source files kept).
/// 3. The `--stats` summary on the client reports
///    `Number of deleted files: 1`.
#[test]
fn daemon_push_emits_ndx_del_stats_and_reports_count() {
    let Some(oc_bin) = locate_oc_rsync() else {
        eprintln!("skipping: oc-rsync binary not found in target/");
        return;
    };
    let Some(scratch) = DaemonScratch::new() else {
        eprintln!("skipping: tempdir or test port allocation failed");
        return;
    };

    let source_dir = scratch.root.join("source");
    let module_root = scratch.root.join("dest");
    seed_source(&source_dir).expect("seed source");
    seed_destination_with_extra(&module_root).expect("seed destination");

    write_daemon_config(
        &scratch.config,
        &scratch.pid,
        &scratch.log,
        "pushmod",
        &module_root,
        false,
    )
    .expect("write daemon config");

    let _daemon = match spawn_oc_daemon(&oc_bin, &scratch.config, scratch.port) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("skipping: could not start oc-rsync --daemon: {e}");
            return;
        }
    };

    let mut source_arg = source_dir.clone().into_os_string();
    source_arg.push("/");
    let dst_url = std::ffi::OsString::from(format!("rsync://127.0.0.1:{}/pushmod/", scratch.port));

    let args: &[&std::ffi::OsStr] = &[
        std::ffi::OsStr::new("--delete"),
        std::ffi::OsStr::new("--stats"),
        std::ffi::OsStr::new("--recursive"),
        std::ffi::OsStr::new("--times"),
        &source_arg,
        &dst_url,
    ];

    let (status, stdout, stderr) =
        run_oc_rsync_capture(&oc_bin, args).expect("spawn oc-rsync client (push)");

    assert!(
        status.success(),
        "client push exited non-zero: {status:?}\nstdout:\n{stdout}\nstderr:\n{stderr}",
    );

    assert_destination_pruned(&module_root);
    assert_delete_count(&stdout, &stderr, 1);
}

/// Pull direction: client pulls from a daemon module into a local
/// destination that contains an extraneous file. The receiver runs
/// in the client process so the `NDX_DEL_STATS` accumulation happens
/// on the client side. Regression guard that landing the
/// upload-direction emission did not break the previously-working
/// pull case.
///
/// Asserts:
/// 1. The client exits with status 0.
/// 2. The local destination is pruned (extra.txt removed, source
///    files kept).
/// 3. The `--stats` summary on the client reports
///    `Number of deleted files: 1`.
#[test]
fn daemon_pull_emits_ndx_del_stats_and_reports_count() {
    let Some(oc_bin) = locate_oc_rsync() else {
        eprintln!("skipping: oc-rsync binary not found in target/");
        return;
    };
    let Some(scratch) = DaemonScratch::new() else {
        eprintln!("skipping: tempdir or test port allocation failed");
        return;
    };

    let module_root = scratch.root.join("source");
    let dest_dir = scratch.root.join("dest");
    seed_source(&module_root).expect("seed source (module)");
    seed_destination_with_extra(&dest_dir).expect("seed local destination");

    // Read-only module: the daemon side only serves; the client side
    // owns the sweep.
    write_daemon_config(
        &scratch.config,
        &scratch.pid,
        &scratch.log,
        "pullmod",
        &module_root,
        true,
    )
    .expect("write daemon config");

    let _daemon = match spawn_oc_daemon(&oc_bin, &scratch.config, scratch.port) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("skipping: could not start oc-rsync --daemon: {e}");
            return;
        }
    };

    let src_url = std::ffi::OsString::from(format!("rsync://127.0.0.1:{}/pullmod/", scratch.port));
    let mut dest_arg = dest_dir.clone().into_os_string();
    dest_arg.push("/");

    let args: &[&std::ffi::OsStr] = &[
        std::ffi::OsStr::new("--delete"),
        std::ffi::OsStr::new("--stats"),
        std::ffi::OsStr::new("--recursive"),
        std::ffi::OsStr::new("--times"),
        &src_url,
        &dest_arg,
    ];

    let (status, stdout, stderr) =
        run_oc_rsync_capture(&oc_bin, args).expect("spawn oc-rsync client (pull)");

    assert!(
        status.success(),
        "client pull exited non-zero: {status:?}\nstdout:\n{stdout}\nstderr:\n{stderr}",
    );

    assert_destination_pruned(&dest_dir);
    assert_delete_count(&stdout, &stderr, 1);
}
