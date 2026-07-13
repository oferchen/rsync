//! NXT-4: reverse-daemon-delta - the daemon is the *sender* of a delta and
//! the client is the *receiver* that reconstructs a file from its own basis.
//!
//! # Background
//!
//! Upstream's `testsuite/reverse-daemon-delta_test.py` (3.4.4) exercises the
//! direction where an `rsyncd` module holds the authoritative copy and a
//! client *pulls* it while already owning a perturbed basis. The receiver
//! computes block signatures over its basis (`sum_head` + the `SignatureBlock`
//! array), ships them to the daemon-sender, and the daemon-sender matches the
//! authoritative payload against them and streams back a Copy/Literal token
//! stream. "Reverse" names the unusual roles: the daemon generates the delta,
//! the client applies it. See `docs/design/uts-v3-c-reverse-daemon-delta-audit.md`
//! and the NXT harness notes in `docs/design/nxt-1-nextest-harness.md`.
//!
//! oc-rsync's matching call sites:
//!
//! - daemon-sender delta generation:
//!   `crates/transfer/src/generator/delta.rs::generate_delta_from_signature`
//!   (refs `sender.c:389-430`), with wire decode of the signature in
//!   `crates/protocol/src/wire/signature.rs::read_signature`.
//! - client receiver applying the delta:
//!   `crates/transfer/src/receiver/` (`delta_apply`, `quick_check`).
//!
//! # What this test pins
//!
//! 1. `oc-rsync -r -t --ignore-times --stats rsync://127.0.0.1:PORT/data/ DEST/`
//!    where DEST already holds a *perturbed* copy of the module's `payload.bin`
//!    exits 0.
//! 2. DEST ends byte-for-byte equal to the daemon-side authoritative source -
//!    the receiver corrected its basis from the Copy/Literal token stream.
//! 3. `--stats` reports non-zero `Matched data` - proving the reverse-delta
//!    path actually matched basis blocks (Copy tokens) rather than degenerating
//!    to a whole-file resend. A regression that broke daemon-sender delta
//!    generation, or that made the receiver discard its basis, would either
//!    drop `Matched data` to zero (caught here) or corrupt the reconstruction
//!    (caught by assertion 2).
//!
//! # Why a perturbed basis + `--ignore-times`
//!
//! The receiver only runs the delta algorithm against an *existing* basis, and
//! the quick-check (`crates/transfer/src/receiver/quick_check.rs:46-87`) skips a
//! file whose size and mtime both match. The basis here is the same size as the
//! authoritative file (so a same-second mtime could otherwise skip it), so
//! `--ignore-times` forces the transfer to run and exercise the delta path. The
//! middle third of the basis is overwritten while the leading and trailing
//! thirds are preserved, so the outer blocks match (Copy) and the middle is
//! resent (Literal) - guaranteeing `Matched data > 0` without a whole-file
//! fallback.
//!
//! # Platform gate
//!
//! `#![cfg(unix)]` - matches the sibling daemon-spawning tests
//! (`uts_15_e_daemon_pull_write_batch.rs`, `uts_nextest_daemon_delete_stats.rs`).
//! The module's `use chroot = false` toggle needs Unix process semantics.
//!
//! # Skip semantics
//!
//! Self-skips (prints `skipping:` and returns) when the workspace `oc-rsync`
//! binary cannot be located, a loopback port cannot be allocated, or the daemon
//! does not start within the daemon boot timeout. Non-zero exit,
//! a diverged destination, or zero `Matched data` are real regressions.

#![cfg(unix)]

use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use tempfile::{TempDir, tempdir};

/// Size of the delta-bearing payload. Large enough to span many signature
/// blocks so the preserved outer thirds yield multiple Copy tokens.
const PAYLOAD_LEN: usize = 64 * 1024;

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

/// Write an `rsyncd.conf` exposing one read-only module rooted at
/// `module_root`. Binds loopback only and sets no `hosts allow` line, avoiding
/// the malformed-`localhost`-address rejection seen in pipe-transport configs.
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
         comment = NXT-4 reverse-daemon-delta\n\
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

/// Spawn `oc-rsync --daemon` on `port` and wait until it accepts connections.
fn spawn_oc_daemon(oc_bin: &Path, config_path: &Path) -> io::Result<(DaemonGuard, u16)> {
    // Acquire a race-free free port and start the daemon on it. Because the
    // default daemon binds with SO_REUSEADDR only (upstream socket.c:447), a
    // port collision is a clean EADDRINUSE daemon exit - never a silent
    // SO_REUSEPORT co-bind - so the helper simply retries with a fresh port.
    // See `test_support::daemon_port`.
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

/// Deterministic authoritative payload bytes.
fn authoritative_payload() -> Vec<u8> {
    (0..PAYLOAD_LEN).map(|i| (i % 251) as u8).collect()
}

/// Build the daemon-side authoritative source: two small files and one
/// multi-KiB binary that the receiver will reconstruct via delta.
fn seed_authoritative_source(dir: &Path) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    fs::write(dir.join("alpha.txt"), b"alpha contents\n")?;
    fs::write(dir.join("beta.txt"), b"beta contents\n")?;
    fs::write(dir.join("payload.bin"), authoritative_payload())?;
    Ok(())
}

/// Pre-seed the destination with a perturbed copy of `payload.bin`: same length
/// as the authoritative file, with only the middle third overwritten. The
/// preserved outer thirds match, so the daemon-sender emits Copy tokens for
/// them (Matched data) and Literal tokens for the middle.
fn write_perturbed_basis(dest: &Path) -> io::Result<()> {
    fs::create_dir_all(dest)?;
    let mut basis = authoritative_payload();
    let third = PAYLOAD_LEN / 3;
    for byte in &mut basis[third..2 * third] {
        *byte = byte.wrapping_add(7) ^ 0xA5;
    }
    fs::write(dest.join("payload.bin"), basis)
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

/// Parse the `Matched data: N bytes` line emitted by `--stats`
/// (`crates/cli/src/frontend/progress/render.rs:503`). Returns the byte count
/// with thousands separators stripped, or `None` if the line is absent.
fn parse_matched_data(stats: &str) -> Option<u64> {
    let line = stats.lines().find(|l| l.contains("Matched data:"))?;
    let digits: String = line
        .chars()
        .skip_while(|c| !c.is_ascii_digit())
        .take_while(|c| c.is_ascii_digit() || *c == ',')
        .filter(|c| *c != ',')
        .collect();
    digits.parse().ok()
}

/// Recursively collect `(relative_path, bytes)` for every regular file.
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

/// Assert two file trees are byte-for-byte identical.
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

/// Per-test scratch state: tempdir, daemon log/pid/config paths, loopback port.
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

/// A daemon-pull against a perturbed basis must reconstruct the authoritative
/// tree via the delta path (non-zero `Matched data`), proving the
/// daemon-sender generated a Copy/Literal token stream the client applied.
#[test]
fn reverse_daemon_delta_reconstructs_via_copy_tokens() {
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

    seed_authoritative_source(&module_root).expect("seed daemon-side authoritative source");
    write_perturbed_basis(&dest_dir).expect("seed receiver-side perturbed basis");

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

    let src_url = std::ffi::OsString::from(format!("rsync://127.0.0.1:{port}/data/"));
    let mut dest_arg = dest_dir.clone().into_os_string();
    dest_arg.push("/");

    // `--ignore-times` defeats the same-size/same-second quick-check skip so the
    // receiver runs the delta algorithm against its perturbed basis.
    let args: &[&std::ffi::OsStr] = &[
        std::ffi::OsStr::new("--recursive"),
        std::ffi::OsStr::new("--times"),
        std::ffi::OsStr::new("--ignore-times"),
        std::ffi::OsStr::new("--stats"),
        &src_url,
        &dest_arg,
    ];

    let (status, stdout, stderr) =
        run_oc_rsync_capture(&oc_bin, args).expect("spawn oc-rsync client (reverse daemon delta)");

    assert!(
        status.success(),
        "reverse daemon-delta pull exited non-zero: {status:?}\nstdout:\n{stdout}\nstderr:\n{stderr}",
    );

    // The receiver must reconstruct the authoritative tree byte-for-byte from
    // its perturbed basis plus the daemon-sender's token stream.
    assert_trees_match(
        &module_root,
        "daemon authoritative source",
        &dest_dir,
        "client reconstructed destination",
    );

    // `Matched data` must be non-zero: the preserved outer thirds of the basis
    // produce Copy tokens. Zero would mean the daemon-sender resent the whole
    // file (delta path disengaged), which this test exists to catch.
    let matched = parse_matched_data(&stdout).unwrap_or_else(|| {
        panic!("--stats output did not contain a `Matched data:` line\nstdout:\n{stdout}")
    });
    assert!(
        matched > 0,
        "reverse daemon-delta produced zero Matched data - the delta path did not engage\nstdout:\n{stdout}",
    );
}

#[cfg(test)]
mod unit {
    use super::parse_matched_data;

    #[test]
    fn parses_matched_data_with_separators() {
        assert_eq!(
            parse_matched_data("Matched data: 111,111 bytes\n"),
            Some(111_111)
        );
    }

    #[test]
    fn parses_zero_matched_data() {
        assert_eq!(parse_matched_data("Matched data: 0 bytes\n"), Some(0));
    }

    #[test]
    fn absent_matched_data_is_none() {
        assert_eq!(parse_matched_data("Literal data: 5 bytes\n"), None);
    }
}
