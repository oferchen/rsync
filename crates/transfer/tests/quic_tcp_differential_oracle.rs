//! QUIC-7 differential oracle: a daemon transfer carried over oc's QUIC
//! transport must produce the same observable result as the byte-identical
//! transfer over oc's TCP transport.
//!
//! # Why a differential oracle, and what "== TCP" means here
//!
//! QUIC is TLS-over-UDP; a QUIC session and a TCP session are not comparable at
//! the wire-byte level (one is encrypted datagrams, the other a cleartext
//! stream). A raw transcript diff is therefore the wrong oracle. What QUIC
//! promises is narrower and checkable: it replaces the transport socket and
//! nothing above it - the `@RSYNCD:` handshake, module negotiation, and the
//! Protocol 32 delta stream are carried unchanged (docs/design/
//! quic-transport-policy.md, decision framing). So the invariant this test
//! pins is *outcome equivalence*: for the same source, the same options, and
//! the same pre-existing basis, the two transports must yield
//!
//!   1. the same reconstructed destination tree (content + mode + owner +
//!      mtime, i.e. `DirDiffOptions::archive`),
//!   2. the same client exit code, and
//!   3. the same `--itemize-changes` report of what changed.
//!
//! These are exactly the observables the existing daemon e2e harness can
//! capture; a byte-for-byte wire diff cannot be one of them because the two
//! encodings are, by construction, different.
//!
//! # Shape
//!
//! One loopback `oc-rsync --daemon` serves a read-only module. The client pulls
//! it twice: once over `rsync://` (TCP), once over `quic://` (QUIC, trusting
//! the daemon's self-signed cert via `--quic-ca`). A single transfer-and-capture
//! driver ([`run_pull`]) parameterizes only the transport, so the two legs
//! differ in nothing else. The source mixes files and subdirectories and
//! includes one larger file with a pre-seeded, partially-overlapping basis at
//! the destination, so the receiver runs the block-matching delta path rather
//! than a full send - the delta case the transport must carry faithfully.
//!
//! The daemon's QUIC listener is one-shot per endpoint (it serves exactly one
//! session), so the QUIC leg dials exactly once, gated on the daemon log's
//! "QUIC listener serving on" line rather than a blind retry that could waste
//! the single connection.
//!
//! # Non-vacuity
//!
//! The tree equality is anchored by asserting the TCP destination equals the
//! source: without that, two identically-failed (e.g. empty) destinations would
//! compare equal and the oracle would pass vacuously. The RED->GREEN proof for
//! the differential assertion itself is recorded in the PR: excluding one file
//! from only the QUIC leg makes the destinations diverge and reddens the
//! `DirDiff` assertion.
//!
//! # Platform / feature gate
//!
//! `#![cfg(all(unix, feature = "quic"))]` - the daemon QUIC listener is
//! Unix-only, and `feature = "quic"` gates both this test and the
//! `test_support` cert helper. The test self-skips (loud `skipping:` line, no
//! silent pass) only when the spawned binary genuinely lacks QUIC or the
//! environment cannot allocate a daemon; under the workspace `--all-features`
//! test job the binary has QUIC and the test runs for real.

#![cfg(all(unix, feature = "quic"))]

use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use tempfile::{TempDir, tempdir};
use test_support::{DirDiff, DirDiffOptions};

/// The delta target's size. Large enough that a single perturbed region leaves
/// most blocks matchable, so the receiver exercises real block matching.
const DELTA_FILE_LEN: usize = 128 * 1024;

/// Which transport the client dials.
#[derive(Clone, Copy)]
enum Transport {
    Tcp,
    Quic,
}

/// Deterministic content for the delta target file.
fn delta_source_bytes() -> Vec<u8> {
    (0..DELTA_FILE_LEN).map(|i| (i % 251) as u8).collect()
}

/// Builds the module source tree: a top-level file, two subdirectories, and one
/// larger file that will be delta-transferred against a pre-seeded basis.
fn build_source(root: &Path) -> io::Result<()> {
    fs::create_dir_all(root.join("sub"))?;
    fs::create_dir_all(root.join("sub2"))?;
    fs::write(root.join("top.txt"), b"top-level file\n")?;
    fs::write(root.join("sub/a.txt"), b"nested small file\n")?;
    fs::write(root.join("sub/b.bin"), delta_source_bytes())?;
    fs::write(root.join("sub2/c.txt"), b"another subdirectory file\n")?;
    Ok(())
}

/// Pre-seeds a destination with a stale, partially-overlapping basis for the
/// delta target so the receiver runs the block-matching delta path rather than
/// a full send. The basis shares every block with the source except a perturbed
/// middle region, and is backdated so rsync's quick-check cannot skip it.
///
/// Applied identically to both destinations, so the delta path is exercised the
/// same way on each transport.
fn seed_delta_basis(dest: &Path) -> io::Result<()> {
    fs::create_dir_all(dest.join("sub"))?;
    let mut basis = delta_source_bytes();
    for byte in &mut basis[60_000..60_800] {
        *byte ^= 0xff;
    }
    let path = dest.join("sub/b.bin");
    fs::write(&path, &basis)?;
    // 2000-01-01: older than the just-written source, so quick-check (size+mtime)
    // cannot skip the file and the delta transfer actually runs.
    let old = filetime::FileTime::from_unix_time(946_684_800, 0);
    filetime::set_file_mtime(&path, old)?;
    Ok(())
}

/// Writes an `rsyncd.conf` exposing one read-only module and enabling the QUIC
/// listener via an operator-supplied cert/key pair. With no `quic port`
/// directive the listener shares the daemon's TCP `--port` on UDP
/// (`effective_quic_port` = the TCP port), so a single free port serves both
/// transports and the TCP port-readiness retry also covers the UDP bind.
fn write_daemon_config(
    config_path: &Path,
    pid_path: &Path,
    log_path: &Path,
    module_name: &str,
    module_root: &Path,
    cert_path: &Path,
    key_path: &Path,
) -> io::Result<()> {
    let body = format!(
        "pid file = {pid}\n\
         log file = {log}\n\
         use chroot = false\n\
         max connections = 4\n\
         quic cert file = {cert}\n\
         quic key file = {key}\n\
         \n\
         [{module}]\n\
         path = {root}\n\
         comment = quic/tcp differential oracle\n\
         read only = true\n\
         list = true\n",
        pid = pid_path.display(),
        log = log_path.display(),
        cert = cert_path.display(),
        key = key_path.display(),
        module = module_name,
        root = module_root.display(),
    );
    fs::write(config_path, body)
}

/// Kills the daemon child on drop so a panicking test never leaks the listener.
struct DaemonGuard {
    child: Child,
}

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Spawns `oc-rsync --daemon` on a race-free free loopback port and waits until
/// it is confirmed listening (TCP). The QUIC/UDP socket binds on the same port
/// before the daemon begins serving; a total QUIC bind failure is fatal, so the
/// free-port retry also yields a port on which the UDP socket bound.
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

/// Whether `oc-rsync --help` advertises the QUIC transport, whose help section
/// (the `quic://` scheme and the `--quic-*` tuning flags) is shown only when
/// the binary was built with the `quic` feature (`quic_unavailable` hides it
/// otherwise). Used to skip loudly rather than fail confusingly when a stale
/// non-QUIC binary is on disk; under `--all-features` the section is present
/// and the test runs.
fn binary_supports_quic(oc_bin: &Path) -> bool {
    Command::new(oc_bin)
        .arg("--help")
        .stdin(Stdio::null())
        .output()
        .map(|out| {
            let text = String::from_utf8_lossy(&out.stdout);
            text.contains("quic://")
        })
        .unwrap_or(false)
}

/// Blocks until the daemon log records the QUIC listener as serving, or the
/// timeout elapses. This is the readiness signal for the one-shot QUIC dial:
/// the line is logged immediately before `accept()`, after the acceptor's I/O
/// driver thread is running.
fn wait_for_quic_serving(log_path: &Path, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if let Ok(text) = fs::read_to_string(log_path) {
            if text.contains("QUIC listener serving on") {
                return true;
            }
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// The single transfer-and-capture driver, parameterized only by transport.
///
/// Pulls the module into `dest` with identical options on both transports;
/// only the operand scheme (and the QUIC-only trust/address flags) change.
/// Returns the client's `(exit status, stdout, stderr)`.
fn run_pull(
    oc_bin: &Path,
    port: u16,
    dest: &Path,
    transport: Transport,
    quic_ca: &Path,
) -> io::Result<(std::process::ExitStatus, String, String)> {
    let src_url = match transport {
        Transport::Tcp => OsString::from(format!("rsync://127.0.0.1:{port}/oracle/")),
        Transport::Quic => OsString::from(format!("quic://localhost:{port}/oracle/")),
    };
    let mut dest_arg = dest.to_path_buf().into_os_string();
    dest_arg.push("/");

    let mut args: Vec<OsString> = vec![
        OsString::from("--archive"),
        OsString::from("--itemize-changes"),
    ];
    if let Transport::Quic = transport {
        // Force IPv4 so `quic://localhost` resolves to the 127.0.0.1 the daemon
        // reaches, and trust the daemon's self-signed leaf as its own root.
        args.push(OsString::from("-4"));
        args.push(OsString::from("--quic-ca"));
        args.push(quic_ca.as_os_str().to_owned());
    }
    args.push(src_url);
    args.push(dest_arg);

    let arg_refs: Vec<&OsStr> = args.iter().map(OsString::as_os_str).collect();
    let output = Command::new(oc_bin)
        .args(&arg_refs)
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

/// Normalizes `--itemize-changes` stdout for transport-independent comparison:
/// trims, drops blank lines, and sorts, so the assertion pins the *set* of
/// per-file changes without depending on incidental line ordering.
fn normalize_itemize(stdout: &str) -> Vec<String> {
    let mut lines: Vec<String> = stdout
        .lines()
        .map(str::trim_end)
        .filter(|l| !l.is_empty())
        .map(str::to_owned)
        .collect();
    lines.sort();
    lines
}

/// Per-test scratch state: tempdir plus daemon config/log/pid and cert paths.
struct Scratch {
    _tmp: TempDir,
    root: PathBuf,
    config: PathBuf,
    log: PathBuf,
    pid: PathBuf,
    cert: PathBuf,
    key: PathBuf,
}

impl Scratch {
    fn new() -> Option<Self> {
        let tmp = tempdir().ok()?;
        let root = tmp.path().to_path_buf();
        Some(Self {
            config: root.join("rsyncd.conf"),
            log: root.join("rsyncd.log"),
            pid: root.join("rsyncd.pid"),
            cert: root.join("quic-cert.pem"),
            key: root.join("quic-key.pem"),
            root,
            _tmp: tmp,
        })
    }
}

/// A daemon pull over QUIC reconstructs byte-for-byte the same destination tree,
/// with the same exit code and the same itemize report, as the identical pull
/// over TCP - including the delta path against a pre-seeded basis. This is the
/// QUIC-7 differential oracle: QUIC is a faithful substitute for the TCP
/// transport, changing the transport socket and nothing above it.
#[test]
fn quic_daemon_pull_matches_tcp_daemon_pull() {
    let oc_bin = test_support::oc_rsync_bin();
    if !binary_supports_quic(&oc_bin) {
        eprintln!(
            "skipping: {} was built without the `quic` feature (no --quic-ca); \
             run under --all-features or --features quic",
            oc_bin.display()
        );
        return;
    }
    let Some(scratch) = Scratch::new() else {
        eprintln!("skipping: tempdir allocation failed");
        return;
    };

    // Operator-shaped self-signed identity: the daemon presents it, the client
    // trusts the same leaf via --quic-ca (a self-signed leaf is its own root).
    let pem = test_support::quic_cert::localhost_self_signed();
    fs::write(&scratch.cert, pem.cert_pem.as_bytes()).expect("write quic cert");
    fs::write(&scratch.key, pem.key_pem.as_bytes()).expect("write quic key");

    let module_root = scratch.root.join("source");
    build_source(&module_root).expect("build source tree");

    let dest_tcp = scratch.root.join("dest_tcp");
    let dest_quic = scratch.root.join("dest_quic");
    seed_delta_basis(&dest_tcp).expect("seed tcp basis");
    seed_delta_basis(&dest_quic).expect("seed quic basis");

    write_daemon_config(
        &scratch.config,
        &scratch.pid,
        &scratch.log,
        "oracle",
        &module_root,
        &scratch.cert,
        &scratch.key,
    )
    .expect("write daemon config");

    let (_daemon, port) = match spawn_oc_daemon(&oc_bin, &scratch.config) {
        Ok(daemon) => daemon,
        Err(e) => {
            eprintln!("skipping: could not start oc-rsync --daemon: {e}");
            return;
        }
    };

    // TCP leg. A success here also proves the daemon is fully serving.
    let (tcp_status, tcp_out, tcp_err) =
        run_pull(&oc_bin, port, &dest_tcp, Transport::Tcp, &scratch.cert)
            .expect("run TCP daemon pull");
    assert!(
        tcp_status.success(),
        "TCP daemon pull failed: status={tcp_status:?}\nstderr:\n{tcp_err}"
    );

    // The QUIC listener is one-shot per endpoint, so gate the single dial on the
    // daemon logging that it is serving rather than risk wasting it on a race.
    assert!(
        wait_for_quic_serving(&scratch.log, Duration::from_secs(10)),
        "daemon never logged 'QUIC listener serving on'; the QUIC listener did not \
         come up. daemon log:\n{}",
        fs::read_to_string(&scratch.log).unwrap_or_default()
    );

    // QUIC leg - byte-identical transfer, only the transport differs.
    let (quic_status, quic_out, quic_err) =
        run_pull(&oc_bin, port, &dest_quic, Transport::Quic, &scratch.cert)
            .expect("run QUIC daemon pull");
    assert!(
        quic_status.success(),
        "QUIC daemon pull failed: status={quic_status:?}\nstderr:\n{quic_err}\n\
         daemon log:\n{}",
        fs::read_to_string(&scratch.log).unwrap_or_default()
    );

    // Axis 1: exit codes agree (and both succeeded).
    assert_eq!(
        tcp_status.code(),
        quic_status.code(),
        "client exit code diverged between transports"
    );

    // Anchor (anti-vacuity): the TCP destination equals the source, so the
    // TCP-vs-QUIC tree equality below is not two identically-failed trees.
    match DirDiff::compare(&module_root, &dest_tcp, DirDiffOptions::structural()) {
        Ok(Ok(())) => {}
        Ok(Err(mismatch)) => panic!(
            "TCP destination does not match the source (vacuity guard tripped):\n{}",
            mismatch.into_panic_message()
        ),
        Err(e) => panic!("DirDiff infrastructure error comparing source vs TCP dest: {e}"),
    }

    // Axis 2 (the differential oracle): the QUIC destination tree is identical
    // to the TCP destination tree - content, mode, owner, and mtime.
    match DirDiff::compare(&dest_tcp, &dest_quic, DirDiffOptions::archive()) {
        Ok(Ok(())) => {}
        Ok(Err(mismatch)) => panic!(
            "QUIC destination tree diverged from the TCP destination tree:\n{}",
            mismatch.into_panic_message()
        ),
        Err(e) => panic!("DirDiff infrastructure error comparing TCP vs QUIC dest: {e}"),
    }

    // Axis 3: the itemize-changes report agrees across transports.
    assert_eq!(
        normalize_itemize(&tcp_out),
        normalize_itemize(&quic_out),
        "--itemize-changes output diverged between transports\nTCP stdout:\n{tcp_out}\n\
         QUIC stdout:\n{quic_out}"
    );
}
