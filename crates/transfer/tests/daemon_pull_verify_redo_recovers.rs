//! A forced phase-2 whole-file verification failure must RECOVER over a daemon
//! transport, matching upstream rsync 3.4.4.
//!
//! # Background
//!
//! When a transferred file fails its whole-file checksum, the receiver queues
//! the file for a phase-2 redo: it re-requests the file with a full-length
//! strong checksum, the sender re-sends it, and the receiver re-verifies
//! (`receiver.c:1093-1096` `send_msg_int(MSG_REDO, ndx)` ->
//! `generator.c:2175-2216` `check_for_finished_files()` redo ->
//! `sender.c:325-341` full-content resend). Upstream recovers such a failure
//! with exit 0 and a byte-correct destination.
//!
//! # What forces the failure deterministically
//!
//! `--append-verify` (append_mode 2, `receiver.c:361-375`) checksums the FULL
//! reconstructed file - the retained destination prefix plus the appended tail -
//! against the sender's whole-file checksum over the authoritative source. A
//! destination pre-seeded with a WRONG (and shorter) prefix therefore fails the
//! whole-file verify with certainty, and no colliding-block construction is
//! needed. The redo pass negates append_mode (`receiver.c:761-773`) and re-sends
//! the file in full, so the destination ends byte-for-byte equal to the source.
//!
//! Plain `--append` (append_mode 1) checksums only the appended tail on both
//! ends, so a wrong prefix is not detected and would not exercise the redo; the
//! verify variant is required.
//!
//! # The regression this pins
//!
//! The redo re-request is a positive file index written through the
//! connection-wide NDX diff-state codec (`io.c::write_ndx`,
//! `prev_positive`/`prev_negative`). The receiver previously created a FRESH
//! codec for the redo pass, resetting `prev_positive` to -1, so the
//! daemon-sender decoded the redo NDX against its running phase-1 state and
//! mis-read the file index, truncating the multiplexed stream. The client died
//! with `multiplexed frame truncated ... (code 23)` and the destination was
//! never corrected. This test fails (exit 23, diverged destination) before the
//! codec is threaded through both passes and passes after.
//!
//! # Platform gate
//!
//! `#![cfg(unix)]` - matches the sibling daemon-spawning tests
//! (`nxt_4_reverse_daemon_delta.rs`, `uts_15_e_daemon_pull_write_batch.rs`); the
//! module's `use chroot = false` toggle needs Unix process semantics.
//!
//! # Skip semantics
//!
//! Self-skips (prints `skipping:` and returns) when a tempdir or loopback port
//! cannot be allocated or the daemon does not start. A missing workspace
//! `oc-rsync` binary fails loudly. A non-zero client exit or a diverged
//! destination is a real regression.

#![cfg(unix)]

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use tempfile::{TempDir, tempdir};

/// Authoritative source length. Spans many signature blocks so the redo resend
/// is a real multi-block delta round-trip, not a single-token special case.
const SOURCE_LEN: usize = 200 * 1024;
/// Length of the deliberately-wrong destination prefix. Shorter than the source
/// so `--append` has a tail to append, and byte-mismatched so the whole-file
/// verify fails.
const BAD_PREFIX_LEN: usize = 100 * 1024;

/// Deterministic authoritative payload bytes.
fn authoritative_payload() -> Vec<u8> {
    (0..SOURCE_LEN).map(|i| (i % 251) as u8).collect()
}

/// Write an `rsyncd.conf` exposing one read-only module rooted at `module_root`.
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
         comment = phase-2 verify redo recovery\n\
         read only = true\n\
         list = true\n",
        pid = pid_path.display(),
        log = log_path.display(),
        module = module_name,
        root = module_root.display(),
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

/// Per-test scratch tree holding the daemon config, log, and pid paths.
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
        Some(Self {
            config: root.join("rsyncd.conf"),
            log: root.join("rsyncd.log"),
            pid: root.join("rsyncd.pid"),
            root,
            _tmp: tmp,
        })
    }
}

/// Drive one `oc-rsync` invocation and return `(status, stdout, stderr)`.
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

/// A daemon pull that forces a whole-file verification failure via
/// `--append-verify` against a wrong-prefix basis must recover through the
/// phase-2 redo: exit 0 and a byte-correct destination.
#[test]
fn daemon_pull_forced_verification_failure_recovers_via_redo() {
    let oc_bin = test_support::oc_rsync_bin();
    let Some(scratch) = DaemonScratch::new() else {
        eprintln!("skipping: tempdir allocation failed");
        return;
    };

    let module_root = scratch.root.join("source");
    let dest_dir = scratch.root.join("dest");
    fs::create_dir_all(&module_root).expect("create module root");
    fs::create_dir_all(&dest_dir).expect("create dest dir");

    let payload = authoritative_payload();
    fs::write(module_root.join("payload.bin"), &payload).expect("seed authoritative source");

    // Destination basis: a wrong, shorter prefix. `--append-verify` retains it,
    // appends the source tail, then fails the whole-file checksum -> redo.
    fs::write(dest_dir.join("payload.bin"), vec![0u8; BAD_PREFIX_LEN])
        .expect("seed wrong-prefix basis");

    write_daemon_config(
        &scratch.config,
        &scratch.pid,
        &scratch.log,
        "data",
        &module_root,
    )
    .expect("write daemon config");

    let (_daemon, port) = match spawn_oc_daemon(&oc_bin, &scratch.config) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("skipping: could not start oc-rsync --daemon: {e}");
            return;
        }
    };

    let src_url = std::ffi::OsString::from(format!("rsync://127.0.0.1:{port}/data/payload.bin"));
    let dest_arg = dest_dir.join("payload.bin").into_os_string();

    // `--append-verify` forces the full-file checksum comparison that fails on
    // the wrong prefix; `--ignore-times` defeats any quick-check skip.
    let args: &[&std::ffi::OsStr] = &[
        std::ffi::OsStr::new("--append-verify"),
        std::ffi::OsStr::new("--ignore-times"),
        &src_url,
        &dest_arg,
    ];

    let (status, stdout, stderr) =
        run_oc_rsync_capture(&oc_bin, args).expect("spawn oc-rsync client (daemon pull redo)");

    // Pre-fix, the redo re-request desynced the sender's NDX decode and the
    // client exited 23 with "multiplexed frame truncated"; the destination was
    // never corrected. The redo must recover the transfer with exit 0.
    assert!(
        status.success(),
        "daemon-pull forced-verification redo exited non-zero: {status:?}\n\
         stdout:\n{stdout}\nstderr:\n{stderr}",
    );

    let got = fs::read(dest_dir.join("payload.bin")).expect("read reconstructed destination");
    assert_eq!(
        got, payload,
        "destination not byte-correct after phase-2 redo recovery",
    );
}
