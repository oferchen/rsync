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

/// Deterministic payload with no two equal signature blocks.
///
/// [`authoritative_payload`] repeats with period 251, so a block at source
/// offset X is byte-identical to a basis block at offset Y whenever
/// `X == Y (mod 251)`. That is harmless when only the reconstructed bytes are
/// asserted, but it lets a delta match blocks the retained tail never covered -
/// measured at 204,400 matched bytes on a first run - which destroys any bound
/// on *which* bytes the redo matched. [`daemon_pull_redo_redeltas_against_the_retained_basis`]
/// needs distinct blocks; derived purely from the index by a fixed integer hash,
/// so the fixture stays reproducible.
fn distinct_block_payload() -> Vec<u8> {
    (0..SOURCE_LEN)
        .map(|i| {
            let x = (i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
            ((x >> 24) ^ x) as u8
        })
        .collect()
}

/// Value of one `--stats` label, with grouping commas and the trailing ` bytes`
/// unit stripped, so `Literal data: 205,300 bytes` reads back as `205300`.
fn stat_field(stdout: &str, label: &str) -> Option<u64> {
    stdout.lines().find_map(|line| {
        let (found, rest) = line.split_once(':')?;
        if found.trim() != label {
            return None;
        }
        let trimmed = rest.trim();
        let unitless = trimmed.strip_suffix("bytes").unwrap_or(trimmed).trim_end();
        unitless
            .chars()
            .filter(|c| *c != ',')
            .collect::<String>()
            .parse()
            .ok()
    })
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

/// The phase-2 redo must re-delta against the RETAINED partial, not re-send the
/// whole file as literal data.
///
/// Upstream re-enters the ordinary `recv_generator()` for a redo index
/// (`generator.c:2200`). That re-stats the destination - which still holds the
/// phase-1 update, because `--append` implies `--inplace` (`options.c:2411`) and
/// `receiver.c:1029` finishes the transfer in place even when `recv_ok == 0` -
/// and re-sends a block signature built from it (`generator.c:1967`). Only
/// `csum_length` (`:2178`) and the sign of `append_mode` (`:2186`) differ from
/// phase 1; the latter is what stops the redo tripping the append short-circuit
/// at `generator.c:1842`, where the destination now equals `F_LENGTH`.
///
/// Before the fix the receiver requested every redo index with a null sum head,
/// so the sender fell into `match.c:403-409`'s literal branch and re-sent all
/// 204,800 bytes: literal 307,200 / matched 0. The bytes still landed correctly,
/// which is why every other cell in this file passes either way - the cost is
/// invisible unless the delta split is asserted.
#[test]
fn daemon_pull_redo_redeltas_against_the_retained_basis() {
    let oc_bin = test_support::oc_rsync_bin();
    let Some(scratch) = DaemonScratch::new() else {
        eprintln!("skipping: tempdir allocation failed");
        return;
    };

    let module_root = scratch.root.join("source");
    let dest_dir = scratch.root.join("dest");
    fs::create_dir_all(&module_root).expect("create module root");
    fs::create_dir_all(&dest_dir).expect("create dest dir");

    // Not `seed_fixture`: this cell bounds *which* bytes were matched, so it
    // needs the distinct-block payload (see `distinct_block_payload`).
    let payload = distinct_block_payload();
    fs::write(module_root.join("payload.bin"), &payload).expect("seed authoritative source");
    fs::write(dest_dir.join("payload.bin"), vec![0u8; BAD_PREFIX_LEN])
        .expect("seed wrong-prefix basis");

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
        OsStr::new(VERBOSE),
        &src_url,
        &dest_arg,
    ];

    let outcome = run_oc_rsync_deadlined(&oc_bin, args).expect("run daemon-pull redo client");
    // The shared oracle first: exit 0, byte-correct destination, upstream's
    // verbatim warning on stderr and nowhere else, and the two transfers that
    // prove the redo ran at all.
    assert_recovered(
        "daemon pull delta split",
        &outcome,
        &dest_dir.join("payload.bin"),
        &payload,
    );

    let literal = stat_field(&outcome.stdout, "Literal data").expect("--stats prints Literal data");
    let matched = stat_field(&outcome.stdout, "Matched data").expect("--stats prints Matched data");

    // Frame: phase 1 appends the tail as literal, phase 2 re-transfers the whole
    // file as some literal/matched split, so the two phases together always
    // account for exactly this many bytes however the redo is delta'd. Pinning
    // the sum keeps the bounds below honest - a redo that silently skipped bytes
    // would satisfy a lone lower bound on `matched`.
    let appended_tail = (SOURCE_LEN - BAD_PREFIX_LEN) as u64;
    assert_eq!(
        literal + matched,
        appended_tail + SOURCE_LEN as u64,
        "phase-1 append plus phase-2 resend must account for every byte \
         (literal {literal}, matched {matched})\nstdout:\n{}",
        outcome.stdout,
    );

    // The redo's basis is the destination as phase 1 left it: BAD_PREFIX_LEN
    // zeros followed by the correctly appended tail, SOURCE_LEN bytes in all.
    // Derive its block geometry the way the receiver does rather than hardcoding
    // it (upstream: generator.c:sum_sizes_sqroot()).
    let layout = signature::calculate_signature_layout(signature::SignatureLayoutParams::new(
        SOURCE_LEN as u64,
        None,
        protocol::ProtocolVersion::NEWEST,
        std::num::NonZeroU8::new(16).expect("redo strong-sum length"),
    ))
    .expect("basis layout");
    let block_len = u64::from(layout.block_length().get());

    // Non-vacuous guard for the ceiling below: with duplicate basis blocks a
    // source block outside the retained tail could legitimately match one inside
    // it, and no upper bound on `matched` would hold.
    let mut seen_blocks = std::collections::HashSet::new();
    for block in payload.chunks(block_len as usize) {
        assert!(
            seen_blocks.insert(block),
            "fixture payload has duplicate {block_len}-byte blocks, so the \
             matched-data ceiling would assert nothing",
        );
    }

    // Every FULL basis block lying entirely inside the correctly-appended region
    // is byte-identical to the source at the same offset, so the redo must match
    // all of them. The first starts at the first block boundary at or after
    // BAD_PREFIX_LEN; the last ends at the last boundary at or before SOURCE_LEN.
    let first_full = (BAD_PREFIX_LEN as u64).div_ceil(block_len);
    let full_blocks = SOURCE_LEN as u64 / block_len - first_full;
    let full_block_bytes = full_blocks * block_len;
    assert!(
        full_blocks > 0,
        "fixture geometry no longer yields whole matchable blocks",
    );

    assert!(
        matched >= full_block_bytes,
        "the phase-2 redo did not re-delta against the retained basis: matched \
         {matched} < {full_block_bytes} (the {full_blocks} whole basis blocks \
         inside the retained tail). A null sum head on the redo request reports \
         matched 0.\nstdout:\n{}",
        outcome.stdout,
    );

    // Upper bound: the zero prefix must NOT be counted as matched (upstream's
    // append pass never calls matched() for the retained prefix - match.c:389-390
    // sets `last_match = s->flength` and zeroes `s->count`). The only byte range
    // matchable beyond the whole blocks is the basis's trailing short block.
    assert!(
        matched <= full_block_bytes + u64::from(layout.remainder()),
        "matched {matched} exceeds the retained tail plus its short trailing \
         block - the redo counted unmatchable bytes\nstdout:\n{}",
        outcome.stdout,
    );
}
