//! Daemon end-to-end guard for the `--stats` "Total transferred file size" line.
//!
//! # Background
//!
//! Upstream accumulates `stats.total_transferred_size` by adding each
//! transferred file's `F_LENGTH` at the exact point it bumps `xferred_files`:
//!
//! - `sender.c:343`   - `stats.total_transferred_size += F_LENGTH(file);`
//! - `receiver.c:784` - `stats.total_transferred_size += F_LENGTH(file);`
//!
//! The value never crosses the wire: `main.c:handle_stats()` (3.4.4:325-385)
//! exchanges only `total_read`, `total_written`, `total_size` and the two
//! flist timing counters. Each side computes `total_transferred_size` locally,
//! and for every direction the process that runs the transfer loop is the
//! client that prints the summary - on a push the client is the sender
//! (`sender.c`), on a pull the client is the receiver (`receiver.c`). So a
//! correct implementation reports the real summed length in BOTH directions
//! purely from its own accumulation. `main.c:439` / `log.c:output_summary()`
//! renders the `Total transferred file size: %s bytes` line from it.
//!
//! # What this test pins
//!
//! oc-rsync previously accumulated this total only in the local-copy executor,
//! so every remote (daemon / ssh) push AND pull printed
//! `Total transferred file size: 0 bytes` even though real file data moved.
//! This is the twin of the `bytes_copied` / `Literal data` gap fixed in #477.
//!
//! For both upload (client pushes into a daemon module) and download (client
//! pulls from a daemon module) into a FRESH empty destination - so every
//! regular file is transferred and the summed length is deterministic:
//!
//! 1. The transfer exits cleanly (status 0).
//! 2. The client's `--stats` summary reports the real non-zero
//!    `Total transferred file size: N bytes`, where `N` is the sum of the
//!    transferred regular files' lengths. A regression that drops the
//!    accumulation (push) or fails to adopt it into the client summary (pull)
//!    prints `0` here.
//!
//! # Platform gate
//!
//! `#![cfg(unix)]` - matches the sibling daemon-stats nextest guards
//! (`uts_nextest_daemon_delete_stats.rs`). Windows daemon mode has separate
//! coverage.
//!
//! # Upstream References
//!
//! - `sender.c:343` / `receiver.c:784` - `total_transferred_size += F_LENGTH`.
//! - `main.c:325-385` - `handle_stats()`: the stat is NOT sent on the wire.
//! - `main.c:439` / `log.c:output_summary()` - the summary line.

#![cfg(unix)]

use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use tempfile::{TempDir, tempdir};

/// Byte lengths of the three regular source files. Kept small (sum < 1000) so
/// the rendered figure carries no thousands separator and the assertion can
/// match the exact `Total transferred file size: N bytes` text. Distinct sizes
/// also guard against a stub that reported a file count instead of a length.
const FILE_SIZES: [usize; 3] = [300, 150, 70];

/// Sum of every transferred regular file's length - the expected value of the
/// `Total transferred file size` line for a fresh (all-new) destination.
fn expected_transferred_size() -> usize {
    FILE_SIZES.iter().sum()
}

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

/// Write an `rsyncd.conf` exposing a single module. `use chroot = false` lets
/// the unprivileged test process drive the daemon; `read only` is parameterised
/// so the same writer covers push (RW) and pull (RO) shapes.
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
         comment = total-transferred-size stats guard\n\
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
struct DaemonGuard {
    child: Child,
}

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Spawn `oc-rsync --daemon` on a free port and wait until it accepts.
fn spawn_oc_daemon(oc_bin: &Path, config_path: &Path) -> io::Result<(DaemonGuard, u16)> {
    let (child, port) = test_support::spawn_daemon_on_free_port(|port| {
        Command::new(oc_bin)
            .arg("--daemon")
            .arg("--no-detach")
            .arg("--port")
            .arg(port.to_string())
            .arg("--config")
            .arg(config_path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
    })?;
    Ok((DaemonGuard { child }, port))
}

/// Seed a source tree of three regular files with the fixed `FILE_SIZES`
/// lengths. Content bytes are position-varied so a size mismatch is obvious.
fn seed_source(dir: &Path) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    for (idx, &size) in FILE_SIZES.iter().enumerate() {
        let name = format!("file_{idx}.dat");
        fs::write(dir.join(name), vec![b'a' + idx as u8; size])?;
    }
    Ok(())
}

/// Drive one `oc-rsync` invocation and capture `(status, stdout, stderr)`.
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

/// Assert the `--stats` summary reports the real summed transferred-file
/// length, not the pre-fix zero.
fn assert_transferred_size(stdout: &str, stderr: &str, expected: usize) {
    let needle = format!("Total transferred file size: {expected} bytes");
    assert!(
        stdout.contains(&needle),
        "missing `{needle}` line in --stats output.\n\
         A remote transfer that failed to accumulate total_transferred_size \
         (push: sender.c:343) or to adopt it into the client summary (pull: \
         receiver.c:784) prints `Total transferred file size: 0 bytes` here.\n\
         stdout:\n{stdout}\nstderr:\n{stderr}",
    );
    assert!(
        !stdout.contains("Total transferred file size: 0 bytes"),
        "reported zero transferred file size despite real data moving.\n\
         stdout:\n{stdout}\nstderr:\n{stderr}",
    );
}

/// Shared scratch: tempdir + daemon log/pid/config paths.
struct DaemonScratch {
    _tmp: TempDir,
    root: PathBuf,
    config: PathBuf,
    log: PathBuf,
    pid: PathBuf,
}

impl DaemonScratch {
    fn new() -> Option<Self> {
        let tmp = tempdir().ok()?;
        let root = tmp.path().to_path_buf();
        let config = root.join("rsyncd.conf");
        let log = root.join("rsyncd.log");
        let pid = root.join("rsyncd.pid");
        Some(Self {
            _tmp: tmp,
            root,
            config,
            log,
            pid,
        })
    }
}

/// Push direction: client is the sender, so it accumulates
/// `total_transferred_size` itself (`sender.c:343`). Destination starts empty
/// so all three files transfer and the summed length is deterministic.
#[test]
fn daemon_push_reports_total_transferred_file_size() {
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
    fs::create_dir_all(&module_root).expect("create empty module root");

    write_daemon_config(
        &scratch.config,
        &scratch.pid,
        &scratch.log,
        "pushmod",
        &module_root,
        false,
    )
    .expect("write daemon config");

    let (_daemon, port) = match spawn_oc_daemon(&oc_bin, &scratch.config) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("skipping: could not start oc-rsync --daemon: {e}");
            return;
        }
    };

    let mut source_arg = source_dir.clone().into_os_string();
    source_arg.push("/");
    let dst_url = std::ffi::OsString::from(format!("rsync://127.0.0.1:{port}/pushmod/"));

    let args: &[&std::ffi::OsStr] = &[
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
    assert_transferred_size(&stdout, &stderr, expected_transferred_size());
}

/// Pull direction: client is the receiver, so it accumulates
/// `total_transferred_size` itself (`receiver.c:784`) and must adopt it into
/// the client summary. Regression guard for the opposite direction.
#[test]
fn daemon_pull_reports_total_transferred_file_size() {
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
    fs::create_dir_all(&dest_dir).expect("create empty local destination");

    // Read-only module: the daemon only serves; the client runs the receiver.
    write_daemon_config(
        &scratch.config,
        &scratch.pid,
        &scratch.log,
        "pullmod",
        &module_root,
        true,
    )
    .expect("write daemon config");

    let (_daemon, port) = match spawn_oc_daemon(&oc_bin, &scratch.config) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("skipping: could not start oc-rsync --daemon: {e}");
            return;
        }
    };

    let src_url = std::ffi::OsString::from(format!("rsync://127.0.0.1:{port}/pullmod/"));
    let mut dest_arg = dest_dir.clone().into_os_string();
    dest_arg.push("/");

    let args: &[&std::ffi::OsStr] = &[
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
    assert_transferred_size(&stdout, &stderr, expected_transferred_size());
}
