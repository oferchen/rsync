//! A forced phase-2 whole-file verification failure must RECOVER on every
//! network transport that can reach it - daemon pull, daemon push and
//! remote-shell pull - matching upstream rsync 3.4.4.
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
//! exits 0, leaves the destination byte-identical to the source and reports
//! `Number of regular files transferred: 2`.
//!
//! The diagnostic is verbosity gated. Under `-v` (or `-i`) every cell prints
//! `WARNING: payload.bin failed verification -- update retained (will try
//! again).` to **stderr** and to nothing else - on a pull from the client's own
//! receiver, on a push from the remote receiver's `MSG_WARNING` frame. Under a
//! plain `-a` every cell recovers in complete silence, on both streams. These
//! assertions encode that oracle, not oc's prior behaviour.
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
//! 3. The queue was unconditional and named the file by its joined ABSOLUTE
//!    destination path, so oc reported `WARNING: /tmp/.../dst/payload.bin ...`
//!    on a plain `-a` run where upstream says nothing at all, and never
//!    reproduced upstream's bare `payload.bin` under `-v`. Measured against
//!    3.4.4 on daemon pull, daemon push, ssh pull and ssh push.
//!
//! The first two regressions are transport-shaped: each reproduced on some cells
//! and not others, so every reachable cell is pinned here individually.
//!
//! # The fourth cell is NOT covered, and why
//!
//! A remote-shell PUSH cannot reach the redo with this fixture. The client does
//! forward the flags - the server argv is
//! `--server -vIe.LsfxCIvu --append --append --stats .` (upstream sends the same
//! pair, `options.c` emits `--append` twice for append_mode 2) - but the
//! `--server` receiver transfers the file whole (`regular files transferred: 1`,
//! literal 204,800, matched 0) instead of appending, so nothing ever fails
//! verification. Upstream on the identical command reports 2 transfers, literal
//! 205,300 / matched 101,900 and the warning. That append-decode gap is a
//! distinct defect from the redo recovery this file pins - it reproduces
//! identically before and after the fix here - so the cell is reported rather
//! than pinned with a test that would assert a recovery that never runs. The
//! same gap makes the local (non-network) cell unreachable.
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
/// The stderr line upstream 3.4.4 emits on the phase-1 verification failure,
/// measured verbatim on every cell below.
///
/// The name is the *file list* name, not the destination path: upstream's
/// receiver has already `change_dir()`ed into the destination root
/// (`main.c:815`), so `f_name(file, ..)` renders relative. Pinning the whole
/// line - not a suffix - is what keeps an absolute path from creeping back in.
const UPSTREAM_WARNING: &str =
    "WARNING: payload.bin failed verification -- update retained (will try again).";

/// Substring identifying the diagnostic regardless of its wording, used to
/// assert it is *absent* from a stream or a run.
const WARNING_MARKER: &str = "failed verification";

/// `-v` is required on every cell: upstream gates the phase-1 `FWARNING` behind
/// `INFO_GTE(NAME, 1) || stdout_format_has_i` (`receiver.c:1072`), so a plain
/// `-a` run recovers silently and the warning assertions would have nothing to
/// match. The silence itself is pinned by
/// [`daemon_pull_default_verbosity_recovers_silently`].
const VERBOSE: &str = "-v";

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
        "{cell}: stderr must carry upstream's verbatim warning\nwant: {UPSTREAM_WARNING}\nstdout:\n{}\nstderr:\n{}",
        outcome.stdout,
        outcome.stderr,
    );

    // ... and only to stderr. `FWARNING` never reaches stdout, so a copy there
    // means the line was framed as `MSG_INFO` (which the peer renders as
    // `FINFO`) instead of `MSG_WARNING`, or written to the wrong local stream.
    assert!(
        !outcome.stdout.contains(WARNING_MARKER),
        "{cell}: the warning must not reach stdout\nstdout:\n{}\nstderr:\n{}",
        outcome.stdout,
        outcome.stderr,
    );

    // The redo must actually have run. Upstream counts the phase-1 append and
    // the phase-2 resend as two transfers of the one file
    // (`receiver.c:784 stats.num_transferred_files++` per pass), so a `1` here
    // means the fixture stopped forcing a verification failure and every other
    // assertion above passed vacuously.
    assert!(
        outcome
            .stdout
            .contains("Number of regular files transferred: 2"),
        "{cell}: phase-2 redo did not run - the fixture no longer forces a \
         verification failure\nstdout:\n{}\nstderr:\n{}",
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
        OsStr::new("--stats"),
        OsStr::new(VERBOSE),
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

/// The same daemon pull WITHOUT `-v` must recover just as silently as upstream.
///
/// This is the other half of the oracle and it is not cosmetic: the diagnostic
/// is queued by the receiver and then routed, so an ungated queue puts text on a
/// stream - or a wire - that upstream leaves untouched at this verbosity. The
/// redo itself is unconditional, so the recovery assertions still have to hold.
///
/// upstream: receiver.c:1072 - `INFO_GTE(NAME, 1) || stdout_format_has_i` gates
/// the `rprintf`; receiver.c:1093-1096 queues the redo outside that `if`.
#[test]
fn daemon_pull_default_verbosity_recovers_silently() {
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
    let args: &[&OsStr] = &[
        OsStr::new("--append-verify"),
        OsStr::new("--ignore-times"),
        OsStr::new("--stats"),
        &src_url,
        &dest_arg,
    ];

    let outcome =
        run_oc_rsync_deadlined(&oc_bin, args).expect("run silent daemon-pull redo client");

    assert!(
        outcome.status.success(),
        "silent daemon pull exited non-zero: {:?}\nstdout:\n{}\nstderr:\n{}",
        outcome.status,
        outcome.stdout,
        outcome.stderr,
    );
    assert_eq!(
        fs::read(dest_dir.join("payload.bin")).expect("read reconstructed destination"),
        payload,
        "silent daemon pull: destination not byte-correct after the redo",
    );
    // The redo ran - so the run had something it could have reported.
    assert!(
        outcome
            .stdout
            .contains("Number of regular files transferred: 2"),
        "silent daemon pull: the redo did not run, so the silence proves nothing\nstdout:\n{}",
        outcome.stdout,
    );
    assert!(
        !outcome.stderr.contains(WARNING_MARKER) && !outcome.stdout.contains(WARNING_MARKER),
        "silent daemon pull: -a alone must print nothing about the failure\nstdout:\n{}\nstderr:\n{}",
        outcome.stdout,
        outcome.stderr,
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
        OsStr::new("--stats"),
        OsStr::new(VERBOSE),
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
        OsStr::new("--stats"),
        OsStr::new(VERBOSE),
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

// The remote-shell PUSH cell has no test on purpose - see "The fourth cell is
// NOT covered, and why" above. Its `--server` receiver never enters append
// mode, so this fixture produces one clean whole-file transfer and no
// verification failure to recover from. A test here could only assert an
// outcome the redo never produced.

/// The warning upstream emits for a file NESTED under the transfer root.
///
/// The flat `payload.bin` cells above cannot tell three renderings apart: the
/// flist name, the basename, and a name relativised against the wrong root all
/// collapse to the same string when the file sits at the top level. Only a
/// nested fixture separates them.
///
/// Measured on rsync 3.4.4: identical on a local copy, a remote-shell push and
/// a remote-shell pull, and unchanged by an absolute destination operand.
const UPSTREAM_NESTED_WARNING: &str =
    "WARNING: sub/deep/payload.bin failed verification -- update retained (will try again).";

/// Seeds the same forced-failure fixture as [`seed_fixture`], nested two
/// directories below the transfer root.
fn seed_nested_fixture(source_root: &Path, dest_root: &Path) -> Vec<u8> {
    let source_dir = source_root.join("sub").join("deep");
    let dest_dir = dest_root.join("sub").join("deep");
    fs::create_dir_all(&source_dir).expect("create nested source dir");
    fs::create_dir_all(&dest_dir).expect("create nested dest dir");
    let payload = authoritative_payload();
    fs::write(source_dir.join("payload.bin"), &payload).expect("seed authoritative source");
    fs::write(dest_dir.join("payload.bin"), vec![0u8; BAD_PREFIX_LEN])
        .expect("seed wrong-prefix basis");
    payload
}

/// The warning must name the file the way upstream's receiver does: relative to
/// the transfer root, with its directory prefix, and never as the absolute
/// destination path - even when the destination OPERAND is absolute.
///
/// Upstream's receiver has already `change_dir()`ed into the destination root
/// (`main.c:815`) by the time it formats this line, so `fname` at
/// `receiver.c:1089` is the file-list name however the operand was written.
/// oc joins a destination root to reach the file on disk, so the absolute path
/// is the value most readily to hand - which is exactly why it needs pinning.
///
/// An absolute destination operand is the discriminator: a joined path leaks
/// there and nowhere else. The nesting is the second discriminator: a rendering
/// that dropped the directory prefix would still pass every flat-name cell in
/// this file.
///
/// upstream: receiver.c:1089 - `local_name ? f_name(file, NULL) : fname`.
#[test]
fn rsh_pull_verification_warning_carries_the_nested_flist_name() {
    require_binaries!("oc-rsync", LSH_STUB_BIN);
    let oc_bin = test_support::oc_rsync_bin();
    let stub = LshRunnerStub::locate().expect("lsh-stub located");
    let Ok(tmp) = tempdir() else {
        eprintln!("skipping: tempdir allocation failed");
        return;
    };

    let source_root = tmp.path().join("source");
    let dest_root = tmp.path().join("dest");
    let payload = seed_nested_fixture(&source_root, &dest_root);

    let rsh_arg = OsString::from(format!("--rsh={}", stub.path().display()));
    let rsync_path_arg = OsString::from(format!("--rsync-path={}", oc_bin.display()));
    let src_arg = OsString::from(format!("localhost:{}/", source_root.display()));
    // Absolute on purpose - see the doc comment.
    let dest_arg = dest_root.clone().into_os_string();

    let args: &[&OsStr] = &[
        OsStr::new("--append-verify"),
        OsStr::new("--ignore-times"),
        OsStr::new("--stats"),
        OsStr::new("-r"),
        OsStr::new(VERBOSE),
        &rsh_arg,
        &rsync_path_arg,
        &src_arg,
        &dest_arg,
    ];

    let outcome = run_oc_rsync_deadlined(&oc_bin, args).expect("run nested-name redo client");
    assert!(
        outcome.status.success(),
        "nested rsh pull exited non-zero: {:?}\nstdout:\n{}\nstderr:\n{}",
        outcome.status,
        outcome.stdout,
        outcome.stderr,
    );

    let reconstructed = fs::read(dest_root.join("sub").join("deep").join("payload.bin"))
        .expect("read reconstructed destination");
    assert_eq!(
        reconstructed, payload,
        "nested rsh pull: destination not byte-correct after the phase-2 redo",
    );

    assert!(
        outcome.stderr.contains(UPSTREAM_NESTED_WARNING),
        "the warning must name the file as upstream does, with its directory \
         prefix and no destination root\nwant: {UPSTREAM_NESTED_WARNING}\nstderr:\n{}",
        outcome.stderr,
    );

    // Stated separately from the line match so a leak reports as a leak rather
    // than as an opaque string mismatch.
    let dest_root_text = dest_root.display().to_string();
    assert!(
        !outcome.stderr.contains(&dest_root_text),
        "the destination root leaked into the diagnostic; upstream prints the \
         file-list name because it change_dir()ed into that root\nroot: \
         {dest_root_text}\nstderr:\n{}",
        outcome.stderr,
    );
}

/// The emission gate reads the resolved output FORMAT, not an `-i` boolean.
///
/// `options.c:2345-2358` feeds one variable, `stdout_format_has_i`, from two
/// sources: an explicit `--out-format`/`--log-format` carrying `%i`, and `-i`,
/// which REWRITES `stdout_format` to `"%i %n%L"`. So a bare `--out-format` with
/// `%i` and no `-i` and no `-v` still enables this warning. A gate implemented
/// as "did the user pass -i" is silent here, which is the divergence this cell
/// exists to catch - it is the one input that separates the two spellings.
///
/// upstream: receiver.c:1072 - `msgtype == FERROR_XFER || INFO_GTE(NAME, 1)
/// || stdout_format_has_i`.
#[test]
fn verification_warning_gate_reads_the_output_format_not_a_dash_i_flag() {
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

    // No -v and no -i: the format alone must open the gate.
    let args: &[&OsStr] = &[
        OsStr::new("--append-verify"),
        OsStr::new("--ignore-times"),
        OsStr::new("--stats"),
        OsStr::new("--out-format=%i%n"),
        &rsh_arg,
        &rsync_path_arg,
        &src_arg,
        &dest_arg,
    ];

    let outcome = run_oc_rsync_deadlined(&oc_bin, args).expect("run out-format gate client");
    assert!(
        outcome.status.success(),
        "out-format gate cell exited non-zero: {:?}\nstdout:\n{}\nstderr:\n{}",
        outcome.status,
        outcome.stdout,
        outcome.stderr,
    );

    let reconstructed = fs::read(dest_dir.join("payload.bin")).expect("read destination");
    assert_eq!(
        reconstructed, payload,
        "out-format gate cell: destination not byte-correct after the redo",
    );

    assert!(
        outcome.stderr.contains(UPSTREAM_WARNING),
        "a format carrying %i must open the gate on its own, with no -v and no \
         -i; a gate driven by an -i boolean is silent here\nwant: \
         {UPSTREAM_WARNING}\nstdout:\n{}\nstderr:\n{}",
        outcome.stdout,
        outcome.stderr,
    );
}
