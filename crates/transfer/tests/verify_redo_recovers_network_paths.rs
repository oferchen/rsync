//! A forced phase-2 whole-file verification failure must RECOVER on EVERY
//! network transport - daemon pull, daemon push, remote-shell pull and
//! remote-shell push - matching upstream rsync 3.4.4.
//!
//! # Background
//!
//! When a transferred file fails its whole-file checksum, the receiver queues
//! the file for a phase-2 redo: it re-requests the file with a full-length
//! strong checksum, the sender re-sends it, and the receiver re-verifies
//! (`receiver.c:1093-1096` `send_msg_int(MSG_REDO, ndx)` ->
//! `generator.c:2175-2216` `check_for_finished_files()` redo ->
//! `sender.c:325-341` full-content resend).
//!
//! # The upstream oracle
//!
//! Measured with the real `rsync 3.4.4 protocol version 32` binary on all four
//! cells plus a local control, using exactly the fixture below: every cell
//! exits 0, leaves the destination byte-identical to the source, reports
//! `Number of regular files transferred: 2`, and prints
//! `WARNING: <name> failed verification -- update retained (will try again).`
//! to **stderr** - on a pull from the client's own receiver, on a push from the
//! remote receiver's `MSG_WARNING` frame. These assertions encode that oracle,
//! not oc's prior behaviour.
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
//! # The regressions this pins
//!
//! 1. The redo re-request is a positive file index written through the
//!    connection-wide NDX diff-state codec (`io.c::write_ndx`,
//!    `prev_positive`/`prev_negative`). The receiver once created a FRESH codec
//!    for the redo pass, resetting `prev_positive` to -1, so the sender decoded
//!    the redo NDX against its running phase-1 state, mis-read the file index
//!    and truncated the multiplexed stream (exit 23, destination uncorrected).
//!
//! 2. The receiver queued the `failed verification` warning and pushed it to the
//!    peer as an `MSG_INFO` frame unconditionally, including in CLIENT mode.
//!    Upstream `log.c:330-346` frames a diagnostic only when `am_server`; a
//!    client writes it to its own stderr. Framing it from a client receiver put
//!    the text in front of a `--server --sender` whose stdout IS the wire: the
//!    peer rendered the payload raw into the multiplexed stream, the client's
//!    demux desynced, its receive loop returned before consuming the redo
//!    resend, and the run DEADLOCKED - the client blocked in `wait()` on a child
//!    blocked writing a resend nobody was reading. Over a daemon transport the
//!    same frame was merely swallowed, which is why the daemon cells recovered
//!    while the remote-shell pull hung forever.
//!
//! Both regressions are transport-shaped: each reproduced on some cells and not
//! others, so every cell is pinned here individually.
//!
//! # Platform gate
//!
//! `#![cfg(unix)]` - matches the sibling daemon-spawning tests
//! (`nxt_4_reverse_daemon_delta.rs`, `uts_15_e_daemon_pull_write_batch.rs`); the
//! module's `use chroot = false` toggle and the `lsh-stub` remote shell both
//! need Unix process semantics.
//!
//! # Skip semantics
//!
//! Self-skips (prints `skipping:` and returns) when a tempdir or loopback port
//! cannot be allocated or the daemon does not start. A missing workspace
//! `oc-rsync`/`lsh-stub` binary fails loudly. A non-zero client exit, a diverged
//! destination, a missing warning, or a client that outlives the deadline is a
//! real regression.

#![cfg(unix)]

use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use tempfile::{TempDir, tempdir};
use test_support::{LSH_STUB_BIN, LshRunnerStub, require_binaries};

/// Authoritative source length. Spans many signature blocks so the redo resend
/// is a real multi-block delta round-trip, not a single-token special case.
const SOURCE_LEN: usize = 200 * 1024;
/// Length of the deliberately-wrong destination prefix. Shorter than the source
/// so `--append` has a tail to append, and byte-mismatched so the whole-file
/// verify fails.
const BAD_PREFIX_LEN: usize = 100 * 1024;
/// Upper bound on a single client run. A recovering 200 KiB transfer over
/// loopback finishes in well under a second; the pre-fix remote-shell cells
/// deadlocked forever, so this deadline is what turns that hang into a failing
/// test instead of a stuck CI job.
const CLIENT_DEADLINE: Duration = Duration::from_secs(60);
/// The stderr text upstream 3.4.4 emits on the phase-1 verification failure.
const UPSTREAM_WARNING: &str = "failed verification -- update retained (will try again).";

/// Deterministic authoritative payload bytes.
fn authoritative_payload() -> Vec<u8> {
    (0..SOURCE_LEN).map(|i| (i % 251) as u8).collect()
}

/// Write an `rsyncd.conf` exposing one module rooted at `module_root`.
///
/// `read_only` distinguishes the pull cell (client reads the module) from the
/// push cell (client writes into it).
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
         comment = phase-2 verify redo recovery\n\
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

/// Outcome of one client run.
struct RunOutcome {
    status: std::process::ExitStatus,
    stdout: String,
    stderr: String,
}

/// Drive one `oc-rsync` invocation under a deadline.
///
/// The child is polled rather than waited on so a transport that deadlocks (the
/// pre-fix remote-shell pull) is reported as a failure instead of hanging the
/// test binary. Returns `Err` describing the timeout after killing the child.
fn run_oc_rsync_deadlined(bin: &Path, args: &[&OsStr]) -> io::Result<RunOutcome> {
    let mut child = Command::new(bin)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    let deadline = Instant::now() + CLIENT_DEADLINE;
    while child.try_wait()?.is_none() {
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("client did not finish within {CLIENT_DEADLINE:?}"),
            ));
        }
        std::thread::sleep(Duration::from_millis(20));
    }

    let output = child.wait_with_output()?;
    Ok(RunOutcome {
        status: output.status,
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

/// Seeds the authoritative source and the wrong-prefix destination basis,
/// returning the authoritative bytes.
fn seed_fixture(source_dir: &Path, dest_dir: &Path) -> Vec<u8> {
    fs::create_dir_all(source_dir).expect("create source dir");
    fs::create_dir_all(dest_dir).expect("create dest dir");
    let payload = authoritative_payload();
    fs::write(source_dir.join("payload.bin"), &payload).expect("seed authoritative source");
    // `--append-verify` retains this prefix, appends the source tail, then fails
    // the whole-file checksum -> redo.
    fs::write(dest_dir.join("payload.bin"), vec![0u8; BAD_PREFIX_LEN])
        .expect("seed wrong-prefix basis");
    payload
}

/// Asserts the oracle outcome: exit 0, byte-correct destination, and the
/// upstream phase-1 warning on stderr.
fn assert_recovered(cell: &str, outcome: &RunOutcome, dest_file: &Path, payload: &[u8]) {
    assert!(
        outcome.status.success(),
        "{cell}: forced-verification redo exited non-zero: {:?}\nstdout:\n{}\nstderr:\n{}",
        outcome.status,
        outcome.stdout,
        outcome.stderr,
    );

    let got = fs::read(dest_file).expect("read reconstructed destination");
    assert_eq!(
        got, payload,
        "{cell}: destination not byte-correct after phase-2 redo recovery",
    );

    // Upstream routes the phase-1 `FWARNING` to stderr on every cell: written
    // directly on a pull (log.c:313-315, `am_server == 0`) and rendered from the
    // remote receiver's `MSG_WARNING` frame on a push (log.c:330-346). A silent
    // recovery means the diagnostic was framed to the wrong peer or dropped.
    assert!(
        outcome.stderr.contains(UPSTREAM_WARNING),
        "{cell}: missing upstream verification warning on stderr\nstdout:\n{}\nstderr:\n{}",
        outcome.stdout,
        outcome.stderr,
    );
}

/// A daemon pull that forces a whole-file verification failure via
/// `--append-verify` against a wrong-prefix basis must recover through the
/// phase-2 redo.
#[test]
fn daemon_pull_forced_verification_failure_recovers_via_redo() {
    let oc_bin = test_support::oc_rsync_bin();
    let Some(scratch) = DaemonScratch::new() else {
        eprintln!("skipping: tempdir allocation failed");
        return;
    };

    let module_root = scratch.root.join("source");
    let dest_dir = scratch.root.join("dest");
    let payload = seed_fixture(&module_root, &dest_dir);

    write_daemon_config(
        &scratch.config,
        &scratch.pid,
        &scratch.log,
        "data",
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

    let src_url = OsString::from(format!("rsync://127.0.0.1:{port}/data/payload.bin"));
    let dest_arg = dest_dir.join("payload.bin").into_os_string();

    // `--append-verify` forces the full-file checksum comparison that fails on
    // the wrong prefix; `--ignore-times` defeats any quick-check skip.
    let args: &[&OsStr] = &[
        OsStr::new("--append-verify"),
        OsStr::new("--ignore-times"),
        &src_url,
        &dest_arg,
    ];

    let outcome = run_oc_rsync_deadlined(&oc_bin, args).expect("run daemon-pull redo client");
    assert_recovered(
        "daemon pull",
        &outcome,
        &dest_dir.join("payload.bin"),
        &payload,
    );
}

/// The same forced failure on a daemon PUSH, where the REMOTE (server) receiver
/// runs the redo and frames its warning back to the pushing client.
#[test]
fn daemon_push_forced_verification_failure_recovers_via_redo() {
    let oc_bin = test_support::oc_rsync_bin();
    let Some(scratch) = DaemonScratch::new() else {
        eprintln!("skipping: tempdir allocation failed");
        return;
    };

    let source_dir = scratch.root.join("source");
    let module_root = scratch.root.join("dest");
    let payload = seed_fixture(&source_dir, &module_root);

    write_daemon_config(
        &scratch.config,
        &scratch.pid,
        &scratch.log,
        "data",
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

    let src_arg = source_dir.join("payload.bin").into_os_string();
    let dest_url = OsString::from(format!("rsync://127.0.0.1:{port}/data/payload.bin"));

    let args: &[&OsStr] = &[
        OsStr::new("--append-verify"),
        OsStr::new("--ignore-times"),
        &src_arg,
        &dest_url,
    ];

    let outcome = run_oc_rsync_deadlined(&oc_bin, args).expect("run daemon-push redo client");
    assert_recovered(
        "daemon push",
        &outcome,
        &module_root.join("payload.bin"),
        &payload,
    );
}

/// The same forced failure over a REMOTE SHELL pull.
///
/// This is the cell the daemon-only fix did not cover: the local client is the
/// receiver and the peer is a `--server --sender` whose stdout is the wire, so a
/// client-framed diagnostic corrupts the very stream the redo resend arrives on.
#[test]
fn rsh_pull_forced_verification_failure_recovers_via_redo() {
    require_binaries!("oc-rsync", LSH_STUB_BIN);
    let oc_bin = test_support::oc_rsync_bin();
    let stub = LshRunnerStub::locate().expect("lsh-stub located");
    let Ok(tmp) = tempdir() else {
        eprintln!("skipping: tempdir allocation failed");
        return;
    };

    let source_dir = tmp.path().join("source");
    let dest_dir = tmp.path().join("dest");
    let payload = seed_fixture(&source_dir, &dest_dir);

    let rsh_arg = OsString::from(format!("--rsh={}", stub.path().display()));
    let rsync_path_arg = OsString::from(format!("--rsync-path={}", oc_bin.display()));
    let src_arg = OsString::from(format!(
        "localhost:{}",
        source_dir.join("payload.bin").display()
    ));
    let dest_arg = dest_dir.join("payload.bin").into_os_string();

    let args: &[&OsStr] = &[
        OsStr::new("--append-verify"),
        OsStr::new("--ignore-times"),
        &rsh_arg,
        &rsync_path_arg,
        &src_arg,
        &dest_arg,
    ];

    let outcome = run_oc_rsync_deadlined(&oc_bin, args).expect("run rsh-pull redo client");
    assert_recovered(
        "remote-shell pull",
        &outcome,
        &dest_dir.join("payload.bin"),
        &payload,
    );
}

/// The same forced failure over a REMOTE SHELL push, where the remote
/// `--server` receiver runs the redo and frames its warning back.
#[test]
fn rsh_push_forced_verification_failure_recovers_via_redo() {
    require_binaries!("oc-rsync", LSH_STUB_BIN);
    let oc_bin = test_support::oc_rsync_bin();
    let stub = LshRunnerStub::locate().expect("lsh-stub located");
    let Ok(tmp) = tempdir() else {
        eprintln!("skipping: tempdir allocation failed");
        return;
    };

    let source_dir = tmp.path().join("source");
    let dest_dir = tmp.path().join("dest");
    let payload = seed_fixture(&source_dir, &dest_dir);

    let rsh_arg = OsString::from(format!("--rsh={}", stub.path().display()));
    let rsync_path_arg = OsString::from(format!("--rsync-path={}", oc_bin.display()));
    let src_arg = source_dir.join("payload.bin").into_os_string();
    let dest_arg = OsString::from(format!(
        "localhost:{}",
        dest_dir.join("payload.bin").display()
    ));

    let args: &[&OsStr] = &[
        OsStr::new("--append-verify"),
        OsStr::new("--ignore-times"),
        &rsh_arg,
        &rsync_path_arg,
        &src_arg,
        &dest_arg,
    ];

    let outcome = run_oc_rsync_deadlined(&oc_bin, args).expect("run rsh-push redo client");
    assert_recovered(
        "remote-shell push",
        &outcome,
        &dest_dir.join("payload.bin"),
        &payload,
    );
}
