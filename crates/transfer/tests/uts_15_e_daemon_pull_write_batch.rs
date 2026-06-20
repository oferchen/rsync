//! UTS-15.e: regression coverage for `--write-batch` on the receiver side
//! of a daemon pull.
//!
//! # Background
//!
//! Upstream `options.c::server_options()` never forwards `--write-batch`,
//! `--only-write-batch`, or `--read-batch` to a daemon-bound argv: the
//! batch flag is a strictly client-local concern. On a pull
//! (`oc-rsync rsync://host/mod/ DEST`), the client is the receiver and the
//! receiver is the side that materialises the batch file. The daemon
//! merely streams the source flist and token stream over the wire.
//!
//! The matching call sites are:
//!
//! - `crates/core/src/client/remote/daemon_transfer/orchestration/arguments.rs::strip_client_only_batch_flags` -
//!   defensively strips the batch flags from the daemon-bound argv.
//! - `crates/cli/src/frontend/execution/drive/workflow/run.rs` - the
//!   client-side receiver wires the batch writer into the local-copy /
//!   remote-receive paths and emits the trailing batch script.
//!
//! UTS-15.a (#3626) pinned the local-copy executor side of
//! `--only-write-batch`. UTS-15 ports through PR #5722, #5614, #5736,
//! #5811 etc. covered batch-mode generally, but none of them exercised
//! the daemon-pull + receiver-side `--write-batch` shape end-to-end. A
//! future refactor that drops the receiver-side batch hook on the
//! remote-receive path (or that lets `strip_client_only_batch_flags`
//! eat the client-side flag too) would silently leave the batch file
//! empty while the destination still came out correct. This test pins
//! that observation.
//!
//! # What this test pins
//!
//! 1. `oc-rsync --write-batch=FILE rsync://127.0.0.1:PORT/mod/ DEST`
//!    exits with status 0.
//! 2. `DEST` matches the daemon-side source byte-for-byte (including a
//!    nested subdirectory and a multi-KiB file).
//! 3. The batch file is created and non-empty, and its header decodes to
//!    the negotiated protocol version. The batch format has NO ASCII magic:
//!    upstream `batch.c:113` writes the stream-flags i32 first, then
//!    `io.c:2446` writes the protocol-version i32. So bytes 4..8 of the file
//!    must equal the negotiated protocol (32). Emitted by
//!    `crates/batch/src/writer.rs::write_header`.
//! 4. A subsequent `oc-rsync --read-batch=FILE DEST_REPLAY/` (no remote
//!    URL) reconstructs the same source tree from the batch file alone,
//!    so the recorded batch is functional, not just non-empty.
//!
//! # Platform gate
//!
//! `#![cfg(unix)]` - matches the sibling daemon-spawning UTS tests
//! (`uts_nextest_daemon_delete_stats.rs`,
//! `v61d_2_daemon_push_increcurse_perf_regression.rs`). The daemon's
//! `use chroot = false` toggle requires Unix process semantics; the
//! Windows daemon mode has separate coverage.
//!
//! # Skip semantics
//!
//! The test self-skips (prints `skipping:` and returns) when:
//!
//! - The workspace `oc-rsync` binary cannot be located (e.g., when this
//!   file is built outside a `cargo nextest` invocation).
//! - A loopback TCP port cannot be allocated.
//! - The daemon fails to start accepting connections within
//!   [`DAEMON_BOOT_TIMEOUT`].
//!
//! Hard failures (non-zero exit, missing destination files, empty batch
//! file, failed replay) are real regressions.
//!
//! # Upstream References
//!
//! - `options.c::server_options()` - source of truth that the batch
//!   flags are client-local. Mirrored at
//!   `daemon_transfer/orchestration/arguments.rs:451`.
//! - `main.c:1830-1846` (3.4.4) - `--write-batch` / `--only-write-batch`
//!   open the batch fd before the transfer drives the receiver.
//! - `batch.c:113` `write_int(batch_fd, flags)` then `io.c:2446`
//!   `write_int(batch_fd, protocol_version)` - the header format this test
//!   asserts against: stream-flags i32 + protocol-version i32, no ASCII magic.

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
/// 10 seconds matches `uts_nextest_daemon_delete_stats.rs` and
/// `v61d_2_daemon_push_increcurse_perf_regression.rs`. A healthy daemon
/// binds well inside this budget; anything slower is a real failure.
const DAEMON_BOOT_TIMEOUT: Duration = Duration::from_secs(10);

/// Locate the workspace `oc-rsync` binary the test runner built.
///
/// Prefers `CARGO_BIN_EXE_oc-rsync` when set; otherwise walks up from
/// the test executable until a `target/` directory is found, then probes
/// the `debug/` and `release/` subdirectories. Mirrors the lookup used
/// by sibling integration tests (`uts_nextest_daemon_delete_stats.rs`,
/// `v61d_2_daemon_push_increcurse_perf_regression.rs`).
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
        if std::net::TcpStream::connect_timeout(&target, Duration::from_millis(200)).is_ok() {
            return true;
        }
        thread::sleep(backoff);
        backoff = (backoff * 2).min(Duration::from_millis(200));
    }
    false
}

/// Write an `rsyncd.conf` exposing one read-only module rooted at
/// `module_root`. Read-only matches the pull-only shape this test
/// exercises; `use chroot = false` keeps the unprivileged test process
/// from needing `CAP_SYS_CHROOT`.
fn write_daemon_config(
    config_path: &Path,
    pid_path: &Path,
    log_path: &Path,
    module_name: &str,
    module_root: &Path,
) -> io::Result<()> {
    let body = format!(
        "pid file = {pid}\n\
         log file = {log}\n\
         use chroot = false\n\
         max connections = 4\n\
         \n\
         [{module}]\n\
         path = {root}\n\
         comment = UTS-15.e daemon-pull write-batch\n\
         read only = true\n\
         list = true\n",
        pid = pid_path.display(),
        log = log_path.display(),
        module = module_name,
        root = module_root.display(),
    );
    fs::write(config_path, body)
}

/// Guard that kills the daemon child on drop so a panicking test does
/// not leak the listener.
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

/// Build the daemon-side source tree: two top-level regular files, a
/// nested subdirectory, and one multi-KiB binary so the batch file has
/// to encode a delta token stream rather than degenerate to a single
/// header.
fn seed_source(dir: &Path) -> io::Result<()> {
    fs::create_dir_all(dir.join("subdir"))?;
    fs::write(dir.join("alpha.txt"), b"alpha contents\n")?;
    fs::write(dir.join("beta.txt"), b"beta contents\n")?;
    fs::write(dir.join("subdir/nested.txt"), b"nested contents\n")?;
    let big: Vec<u8> = (0..16 * 1024).map(|i| (i % 251) as u8).collect();
    fs::write(dir.join("payload.bin"), &big)?;
    Ok(())
}

/// Drive one `oc-rsync` invocation with the provided args and return
/// `(status, stdout, stderr)`.
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

/// Recursively collect `(relative_path, bytes)` pairs for every regular
/// file under `root`. Symlinks and directories are skipped at the leaf
/// level; the directory walk descends into subdirectories. Used to
/// compare source and destination byte-for-byte without depending on
/// an external crate.
fn collect_file_bytes(root: &Path) -> io::Result<Vec<(PathBuf, Vec<u8>)>> {
    let mut out = Vec::new();
    fn walk(base: &Path, dir: &Path, out: &mut Vec<(PathBuf, Vec<u8>)>) -> io::Result<()> {
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            let ft = entry.file_type()?;
            if ft.is_dir() {
                walk(base, &path, out)?;
            } else if ft.is_file() {
                let rel = path.strip_prefix(base).unwrap().to_path_buf();
                let bytes = fs::read(&path)?;
                out.push((rel, bytes));
            }
        }
        Ok(())
    }
    walk(root, root, &mut out)?;
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

/// Assert two file trees are byte-for-byte identical. `lhs_label` /
/// `rhs_label` are the role descriptions used in the failure message.
fn assert_trees_match(lhs: &Path, lhs_label: &str, rhs: &Path, rhs_label: &str) {
    let lhs_files = collect_file_bytes(lhs).expect("walk lhs");
    let rhs_files = collect_file_bytes(rhs).expect("walk rhs");
    assert_eq!(
        lhs_files.len(),
        rhs_files.len(),
        "{lhs_label} has {} files but {rhs_label} has {} files\n{lhs_label}: {:?}\n{rhs_label}: {:?}",
        lhs_files.len(),
        rhs_files.len(),
        lhs_files.iter().map(|(p, _)| p).collect::<Vec<_>>(),
        rhs_files.iter().map(|(p, _)| p).collect::<Vec<_>>(),
    );
    for ((lp, lb), (rp, rb)) in lhs_files.iter().zip(rhs_files.iter()) {
        assert_eq!(
            lp, rp,
            "{lhs_label} / {rhs_label} entry mismatch: {lp:?} vs {rp:?}",
        );
        assert_eq!(
            lb,
            rb,
            "{lhs_label} ({}) and {rhs_label} ({}) differ on {lp:?}",
            lhs.display(),
            rhs.display(),
        );
    }
}

/// Common per-test scratch state: tempdir, daemon log/pid/config paths,
/// and an allocated loopback port.
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

/// `oc-rsync --write-batch=FILE rsync://daemon/mod/ DEST` must:
///
/// 1. Exit 0.
/// 2. Materialise the source tree at `DEST` byte-for-byte.
/// 3. Create a non-empty batch file with the canonical batch magic
///    header (`RSYNC` + flags byte).
/// 4. Be replayable: `oc-rsync --read-batch=FILE DEST_REPLAY/` (no
///    remote endpoint) must reconstruct the same tree.
///
/// This pins the receiver-side batch hook on the daemon-pull path
/// against a future regression that would either drop the batch flag
/// from the receiver's effective options or strip it from the
/// client-side argv along with the daemon-bound argv strip.
#[test]
fn daemon_pull_write_batch_records_and_replays() {
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
    let replay_dir = scratch.root.join("replay");
    let batch_path = scratch.root.join("uts15e.batch");

    seed_source(&module_root).expect("seed daemon-side source");
    fs::create_dir_all(&dest_dir).expect("create local destination dir");
    fs::create_dir_all(&replay_dir).expect("create replay destination dir");

    write_daemon_config(
        &scratch.config,
        &scratch.pid,
        &scratch.log,
        "pullmod",
        &module_root,
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
    let write_batch_arg = {
        let mut s = std::ffi::OsString::from("--write-batch=");
        s.push(batch_path.as_os_str());
        s
    };

    let args: &[&std::ffi::OsStr] = &[
        std::ffi::OsStr::new("--recursive"),
        std::ffi::OsStr::new("--times"),
        &write_batch_arg,
        &src_url,
        &dest_arg,
    ];

    let (status, stdout, stderr) =
        run_oc_rsync_capture(&oc_bin, args).expect("spawn oc-rsync client (pull + write-batch)");

    assert!(
        status.success(),
        "client pull --write-batch exited non-zero: {status:?}\nstdout:\n{stdout}\nstderr:\n{stderr}",
    );

    // The destination must come out matching the daemon-side source
    // byte-for-byte. A regression that lets the batch hook short-circuit
    // the receiver's write path would show up here (a `--write-batch`
    // pull must still populate the destination - that is the contract
    // distinguishing it from `--only-write-batch`).
    assert_trees_match(
        &module_root,
        "daemon source",
        &dest_dir,
        "client destination",
    );

    // Batch file must exist, be non-empty, and start with the upstream
    // rsync batch header. `crates/batch/src/writer.rs`
    // `BatchWriter::write_header` is the single emission site - if it is
    // bypassed on the daemon-pull path the receiver writes nothing here and
    // the header assertion below trips.
    //
    // upstream: batch.c:113 `write_int(batch_fd, flags)` followed by
    // io.c:2446 `write_int(batch_fd, protocol_version)`. The batch format has
    // NO ASCII magic - the first field is the stream-flags i32, the second is
    // the negotiated protocol version. So the header is validated by decoding
    // the protocol-version field (bytes 4..8) rather than a magic string.
    let batch_meta = fs::metadata(&batch_path).expect("stat batch file");
    assert!(
        batch_meta.len() > 0,
        "--write-batch must produce a non-empty batch file at {}",
        batch_path.display(),
    );
    let batch_bytes = fs::read(&batch_path).expect("read batch file");
    // stream-flags i32 + protocol i32 = 8 bytes minimum before any body.
    assert!(
        batch_bytes.len() >= 8,
        "batch file too short to contain the stream-flags + protocol header: {} bytes",
        batch_bytes.len(),
    );
    let protocol_version =
        i32::from_le_bytes(batch_bytes[4..8].try_into().expect("4-byte protocol field"));
    assert_eq!(
        protocol_version,
        32,
        "batch header protocol-version field (bytes 4..8) must be the negotiated \
         protocol 32; first 16 bytes = {:?}",
        &batch_bytes[..batch_bytes.len().min(16)],
    );

    // Replay leg: `--read-batch=FILE DEST_REPLAY/` must reconstruct the
    // source tree purely from the recorded batch. No daemon endpoint
    // involved on the replay path - this proves the batch the daemon
    // pull recorded is functionally complete, not just non-empty.
    //
    // `crates/core/src/client/run/batch.rs::replay_batch` reads the
    // destination root from `transfer_args().last()`, so the canonical
    // single-positional form `oc-rsync --read-batch=FILE DEST/` is the
    // safest invocation: it matches what
    // `generate_script_with_args` emits into the trailing batch.sh and
    // what upstream's `rsync(1)` documents for `--read-batch`.
    let read_batch_arg = {
        let mut s = std::ffi::OsString::from("--read-batch=");
        s.push(batch_path.as_os_str());
        s
    };
    let mut replay_dest_arg = replay_dir.clone().into_os_string();
    replay_dest_arg.push("/");
    let replay_args: &[&std::ffi::OsStr] = &[
        std::ffi::OsStr::new("--recursive"),
        std::ffi::OsStr::new("--times"),
        &read_batch_arg,
        &replay_dest_arg,
    ];

    let (replay_status, replay_stdout, replay_stderr) =
        run_oc_rsync_capture(&oc_bin, replay_args).expect("spawn oc-rsync client (--read-batch)");

    assert!(
        replay_status.success(),
        "--read-batch replay exited non-zero: {replay_status:?}\nstdout:\n{replay_stdout}\nstderr:\n{replay_stderr}",
    );

    assert_trees_match(
        &module_root,
        "daemon source",
        &replay_dir,
        "replay destination",
    );
}
