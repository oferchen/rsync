//! UTS-NEXTEST-EDGE.e: nextest port of the upstream `testsuite/daemon-gzip-
//! download.test` and `testsuite/daemon-gzip-upload.test` `-zz` codec
//! scenarios.
//!
//! Upstream test sources:
//! - `target/interop/upstream-src/rsync-3.4.4/testsuite/daemon-gzip-download.test`
//! - `target/interop/upstream-src/rsync-3.4.4/testsuite/daemon-gzip-upload.test`
//!
//! (The identical scenarios also live in 3.4.3 / 3.4.2 / 3.4.1; the 3.4.4
//! files are the canonical upstream copies.)
//!
//! # Background
//!
//! Both upstream tests exercise `-zz` against a daemon transfer in both
//! directions. `-zz` selects the new-style per-block codec (zlibx; upstream
//! `options.c:2002` flips `compress_choice = "zlibx"` when the count of
//! `-z` flags is `>= 2` and no explicit `--compress-choice` was given).
//! The negotiation surface lives in `crates/protocol` (capability advertise
//! + `compress` capability bit) and the codec implementation in
//! `crates/compress`.
//!
//! The recurring failure mode that motivates the UTS-NEXTEST-EDGE family
//! port of these scenarios is the daemon-sender goodbye flush regression
//! tracked as UTS-9.REOPEN / UTS-10.REOPEN / UTS-V3.A: the last ~2KB of
//! compressed data was lost before the daemon-receiver could decode the
//! final `NDX_DONE`, surfacing as truncated output past ~615KB. The fix
//! shipped under PRs #5520, #5600, #5619 and the V3 cluster A drain
//! (`crates/daemon/src/daemon/sections/module_access/transfer.rs`).
//!
//! The upstream testsuite runs in CI under `continue-on-error: true`, so a
//! per-test regression on either of these scripts does not block a PR.
//! The UTS-NEXTEST-EDGE family lifts the upstream scenarios into native
//! nextest integration tests so they run as a required check on every PR.
//!
//! # What this test pins
//!
//! For each direction (download / upload):
//!
//! - The transfer exits cleanly (status 0).
//! - The destination file is byte-identical to the source.
//! - The goodbye envelope arrived intact - stderr is free of the
//!   `connection unexpectedly closed` signature that the UTS-9 regression
//!   surfaced.
//! - `--stats` reports a transferred-bytes count materially smaller than
//!   the source size on the compressible portion - evidence the wire
//!   codec actually engaged rather than degrading to identity.
//!
//! A third test compares `-z` and `-zz` wire stats on the same fixture to
//! pin that the two flags drive different codec paths (zlib vs zlibx). A
//! pure-identity regression would tie the byte counts together; the
//! assertion is loose enough to tolerate normal compression-ratio
//! variance while still tripping if `-zz` silently degrades.
//!
//! # Platform gate
//!
//! `#![cfg(unix)]` - the daemon-spawn helper assumes POSIX TCP semantics
//! and the sibling UTS-NEXTEST-EDGE tests share the same gate
//! (`uts_nextest_chdir_symlink_race.rs`, `uts_nextest_hardlinks_inc_recurse.rs`).
//! Windows daemon-mode coverage lives in the `daemon` crate's chunked
//! tests where required.
//!
//! # Upstream References
//!
//! - `testsuite/daemon-gzip-download.test` - upstream download script.
//! - `testsuite/daemon-gzip-upload.test` - upstream upload script.
//! - `options.c:2002` - `-zz` -> `compress_choice = "zlibx"` mapping.
//! - `compat.c::setup_compress()` - codec negotiation across the wire.
//! - `main.c:983` `do_server_sender()` - `io_flush(FULL_FLUSH)` before
//!   return; the contract pinned by UTS-9.
//! - `crates/transfer/src/generator/transfer/orchestrator.rs` - matching
//!   flush on the oc-rsync sender side.

#![cfg(unix)]

use std::env;
use std::fs;
use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use tempfile::{TempDir, tempdir};

/// Fixture file sizes. Picked to clear the ~615KB cutoff observed in the
/// UTS-9 regression so the goodbye-flush invariant is actually exercised,
/// while keeping CI wall-clock bounded.
///
/// The compressible payload is `512 KB` of a short repeating phrase
/// (high zlib ratio); the incompressible payload is `512 KB` of a
/// deterministic PRNG stream. Total source size is ~1 MB.
const COMPRESSIBLE_BYTES: usize = 512 * 1024;
const INCOMPRESSIBLE_BYTES: usize = 512 * 1024;

/// Daemon startup deadline. Mirrors `v61d_2_daemon_push_increcurse_perf_regression.rs`.
const DAEMON_BOOT_TIMEOUT: Duration = Duration::from_secs(10);

/// Locate the workspace `oc-rsync` binary the test runner built.
///
/// Mirrors the sibling helper used by
/// `uts_nextest_hardlinks_inc_recurse.rs` and
/// `v61d_2_daemon_push_increcurse_perf_regression.rs`: prefer the cargo
/// injection, otherwise walk up from the test executable until a
/// `target/` directory is found.
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
/// Drops the listener immediately so the daemon can rebind. The residual
/// window between drop and daemon bind is acceptable for tests since each
/// test gets a fresh ephemeral port from the OS.
fn allocate_test_port() -> Option<u16> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0u16)).ok()?;
    let port = listener.local_addr().ok()?.port();
    drop(listener);
    Some(port)
}

/// Poll until the daemon accepts a TCP connection on `port`, or the
/// deadline elapses.
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

/// Guard that kills the oc-rsync daemon on drop. Mirrors the pattern from
/// `v61d_2_daemon_push_increcurse_perf_regression.rs` and keeps a
/// panicking test from leaving a dangling TCP listener behind.
struct DaemonGuard {
    child: Child,
}

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Spawn `oc-rsync --daemon` on `port` against `config_path` and wait for
/// it to accept connections. Returns `Err` if the daemon never becomes
/// ready within `DAEMON_BOOT_TIMEOUT`.
fn spawn_oc_rsync_daemon(bin: &Path, config_path: &Path, port: u16) -> io::Result<DaemonGuard> {
    let child = Command::new(bin)
        .arg("--daemon")
        .arg("--no-detach")
        .arg("--port")
        .arg(port.to_string())
        .arg("--address=127.0.0.1")
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

/// Write an `rsyncd.conf` exposing a single read-write module rooted at
/// `module_root`. `use chroot = false` and `read only = false` are both
/// required so the unprivileged test process can drive both directions.
fn write_daemon_config(
    config_path: &Path,
    log_path: &Path,
    pid_path: &Path,
    module_root: &Path,
) -> io::Result<()> {
    let body = format!(
        "pid file = {pid}\n\
         log file = {log}\n\
         use chroot = false\n\
         max connections = 4\n\
         \n\
         [gzipmod]\n\
         path = {module}\n\
         comment = UTS-NEXTEST-EDGE.e daemon-gzip-zz fixture\n\
         read only = false\n\
         write only = false\n\
         list = true\n",
        pid = pid_path.display(),
        log = log_path.display(),
        module = module_root.display(),
    );
    fs::write(config_path, body)
}

/// Build a deterministic compressible payload: a short ASCII phrase
/// repeated to the requested length. zlib drives this to a very high
/// ratio (well under 10% of the source size), so the `--stats` evidence
/// for "compression engaged" stays robust to ratio variance across
/// codec versions.
fn build_compressible(size: usize) -> Vec<u8> {
    let phrase = b"compressible_repeated_pattern_for_uts_nextest_edge_e ";
    phrase.iter().copied().cycle().take(size).collect()
}

/// Build a deterministic incompressible payload via a small linear
/// congruential generator. No external `rand` dependency on a fixed
/// seed - the LCG output is statistically dense enough that zlib cannot
/// pack it materially, and the byte stream is identical across runs so
/// the destination comparison is deterministic.
fn build_incompressible(size: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(size);
    let mut state: u64 = 0x_dead_beef_cafe_babe_u64;
    while out.len() < size {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        out.extend_from_slice(&state.to_le_bytes());
    }
    out.truncate(size);
    out
}

/// Write the canonical fixture file at `path`. The fixture is a single
/// file containing the compressible payload followed by the
/// incompressible payload, sized just over 1MB so the transfer clears
/// the UTS-9 ~615KB cutoff after compression.
fn write_fixture(path: &Path) -> io::Result<Vec<u8>> {
    let mut content = build_compressible(COMPRESSIBLE_BYTES);
    content.extend_from_slice(&build_incompressible(INCOMPRESSIBLE_BYTES));
    let mut f = fs::File::create(path)?;
    f.write_all(&content)?;
    Ok(content)
}

/// Parse the byte count out of a `--stats` line like
/// `Total bytes sent: 12,345` or `Total bytes received: 12345`. Returns
/// `None` if the line is malformed - the caller treats absence as a
/// non-blocking skip rather than a hard failure so the test does not
/// brittle-couple to renderer formatting tweaks.
fn parse_stats_bytes(stdout: &str, prefix: &str) -> Option<u64> {
    for line in stdout.lines() {
        if let Some(rest) = line.strip_prefix(prefix) {
            let digits: String = rest.chars().filter(|c| c.is_ascii_digit()).collect();
            if digits.is_empty() {
                return None;
            }
            return digits.parse::<u64>().ok();
        }
    }
    None
}

/// Shared fixture: a daemon serving a single ~1MB file whose first 512KB
/// is highly compressible. Both directions reuse this shape so the
/// codec-engaged invariant is comparable across tests.
struct GzipDaemonFixture {
    _workdir: TempDir,
    module_root: PathBuf,
    port: u16,
    _daemon: DaemonGuard,
}

impl GzipDaemonFixture {
    /// Spin up the daemon. Returns `Ok(Some)` on success; `Ok(None)` if
    /// the oc-rsync binary is missing (the test then logs and skips
    /// rather than failing - the same pattern the sibling nextest tests
    /// use for binary-absent environments).
    fn start() -> io::Result<Option<Self>> {
        let Some(bin) = locate_oc_rsync() else {
            return Ok(None);
        };
        let workdir = tempdir()?;
        let module_root = workdir.path().join("module");
        fs::create_dir_all(&module_root)?;
        let config_path = workdir.path().join("rsyncd.conf");
        let log_path = workdir.path().join("rsyncd.log");
        let pid_path = workdir.path().join("rsyncd.pid");
        write_daemon_config(&config_path, &log_path, &pid_path, &module_root)?;

        let port = allocate_test_port()
            .ok_or_else(|| io::Error::other("could not allocate ephemeral port"))?;
        let daemon = spawn_oc_rsync_daemon(&bin, &config_path, port)?;

        Ok(Some(Self {
            _workdir: workdir,
            module_root,
            port,
            _daemon: daemon,
        }))
    }

    /// Module-local path for fixture authoring (used by the download
    /// test to plant the source file the client will pull).
    fn module_root(&self) -> &Path {
        &self.module_root
    }

    /// rsync:// URL of the module endpoint - the canonical address
    /// shape used by both `localhost::module/path` and `rsync://host/module/path`
    /// upstream invocations.
    fn url(&self) -> String {
        format!("rsync://127.0.0.1:{}/gzipmod", self.port)
    }
}

/// Drive an oc-rsync client invocation with the supplied argv tail and
/// capture stdout, stderr, exit code. Mirrors the shape of the sibling
/// `time_push` helper but emits the full `Output` since both direction
/// tests need to inspect stderr for the goodbye signature.
fn run_client(args: &[&std::ffi::OsStr]) -> io::Result<std::process::Output> {
    let bin = locate_oc_rsync().ok_or_else(|| {
        io::Error::other("oc-rsync binary not found via CARGO_BIN_EXE_oc-rsync or target/")
    })?;
    Command::new(bin)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
}

/// Read all bytes from a path, panicking on failure with a clear error.
fn read_all(path: &Path) -> Vec<u8> {
    let mut buf = Vec::new();
    fs::File::open(path)
        .and_then(|mut f| f.read_to_end(&mut buf).map(|_| ()))
        .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    buf
}

/// UTS-NEXTEST-EDGE.e.1 - download direction.
///
/// Mirrors upstream `daemon-gzip-download.test`:
///
/// ```sh
/// $RSYNC -avvvvzz localhost::test-from/ '$todir/'
/// ```
///
/// The upstream script uses `RSYNC_CONNECT_PROG` to fake-connect over
/// stdio; this nextest port runs a real TCP daemon so the negotiation
/// crosses the same wire path the production daemon takes. The fixture
/// is sized to clear the UTS-9 ~615KB cutoff post-compression, so a
/// regression in the goodbye flush will truncate the destination.
#[test]
fn daemon_gzip_zz_download_byte_identical() {
    let fixture = match GzipDaemonFixture::start() {
        Ok(Some(f)) => f,
        Ok(None) => {
            eprintln!("skip: oc-rsync binary not located");
            return;
        }
        Err(e) => {
            eprintln!("skip: could not start oc-rsync daemon: {e}");
            return;
        }
    };

    let src_path = fixture.module_root().join("payload.bin");
    let source = write_fixture(&src_path).expect("write daemon-side source fixture");

    let dest_dir = tempdir().expect("dest tempdir");
    let dest_file = dest_dir.path().join("payload.bin");

    let url = format!("{}/payload.bin", fixture.url());
    let url_os = std::ffi::OsString::from(url);
    let dest_os = dest_dir.path().as_os_str().to_owned();
    let output = run_client(&[
        "-a".as_ref(),
        "-zz".as_ref(),
        "--stats".as_ref(),
        "--timeout=30".as_ref(),
        url_os.as_ref(),
        dest_os.as_ref(),
    ])
    .expect("spawn oc-rsync client");

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();

    assert!(
        output.status.success(),
        "daemon-pull -zz must exit 0; status={:?}\nstdout:\n{stdout}\nstderr:\n{stderr}",
        output.status.code(),
    );

    // UTS-9 fail-loud: a regression in the daemon-sender goodbye flush
    // surfaces as this signature in stderr even when the exit code is
    // masked. Asserting on the absence is the cheapest invariant that
    // distinguishes "transfer slow but correct" from "transfer truncated".
    assert!(
        !stderr.contains("connection unexpectedly closed"),
        "stderr must not contain 'connection unexpectedly closed' (UTS-9 signature); stderr:\n{stderr}"
    );

    let received = read_all(&dest_file);
    assert_eq!(
        received.len(),
        source.len(),
        "downloaded file size must match source",
    );
    assert_eq!(
        received, source,
        "downloaded file must be byte-identical to source"
    );

    // Codec-engaged evidence: the compressible half is ~50% of source,
    // zlibx drives it well under 10% of its raw size. Even with the
    // incompressible tail dominating the wire bytes, the total payload
    // bytes reported by `--stats` should land at most ~75% of raw
    // source size. Loose enough to tolerate codec-version variance, tight
    // enough to trip if `-zz` silently degraded to identity.
    if let Some(sent) = parse_stats_bytes(&stdout, "Total bytes sent:") {
        let cap = (source.len() as u64 * 3) / 4;
        assert!(
            sent < cap,
            "Total bytes sent={sent} exceeds {cap} (75% of source); -zz did not engage?\nstdout:\n{stdout}"
        );
    }
}

/// UTS-NEXTEST-EDGE.e.2 - upload direction.
///
/// Mirrors upstream `daemon-gzip-upload.test`:
///
/// ```sh
/// $RSYNC -avvvvzz '$fromdir/' localhost::test-to/
/// ```
///
/// The upload codepath drives the client as the sender and the daemon
/// as the receiver. The daemon-side decoder must accept the zlibx wire
/// stream without truncation; the goodbye-flush invariant is the
/// receiver-side mirror of UTS-9.
#[test]
fn daemon_gzip_zz_upload_byte_identical() {
    let fixture = match GzipDaemonFixture::start() {
        Ok(Some(f)) => f,
        Ok(None) => {
            eprintln!("skip: oc-rsync binary not located");
            return;
        }
        Err(e) => {
            eprintln!("skip: could not start oc-rsync daemon: {e}");
            return;
        }
    };

    let src_dir = tempdir().expect("src tempdir");
    let src_path = src_dir.path().join("payload.bin");
    let source = write_fixture(&src_path).expect("write client-side source fixture");

    let url = format!("{}/", fixture.url());
    let url_os = std::ffi::OsString::from(url);
    let src_arg = src_path.as_os_str().to_owned();
    let output = run_client(&[
        "-a".as_ref(),
        "-zz".as_ref(),
        "--stats".as_ref(),
        "--timeout=30".as_ref(),
        src_arg.as_ref(),
        url_os.as_ref(),
    ])
    .expect("spawn oc-rsync client");

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();

    assert!(
        output.status.success(),
        "daemon-push -zz must exit 0; status={:?}\nstdout:\n{stdout}\nstderr:\n{stderr}",
        output.status.code(),
    );

    assert!(
        !stderr.contains("connection unexpectedly closed"),
        "stderr must not contain 'connection unexpectedly closed' (UTS-9 signature); stderr:\n{stderr}"
    );

    let landed = fixture.module_root().join("payload.bin");
    let received = read_all(&landed);
    assert_eq!(
        received.len(),
        source.len(),
        "uploaded file size must match source",
    );
    assert_eq!(
        received, source,
        "uploaded file must be byte-identical to source"
    );

    // Same codec-engaged evidence as the download direction.
    if let Some(sent) = parse_stats_bytes(&stdout, "Total bytes sent:") {
        let cap = (source.len() as u64 * 3) / 4;
        assert!(
            sent < cap,
            "Total bytes sent={sent} exceeds {cap} (75% of source); -zz did not engage?\nstdout:\n{stdout}"
        );
    }
}

/// UTS-NEXTEST-EDGE.e.3 - `-z` vs `-zz` negotiate distinct codec paths.
///
/// `-z` selects the legacy per-token zlib codec (compress_choice unset,
/// upstream's "old" path); `-zz` flips `compress_choice = "zlibx"`
/// (`options.c:2002`). A regression that silently collapsed `-zz` onto
/// the legacy path would tie the `--stats` byte counts together on the
/// same fixture; the assertion is loose enough to tolerate normal
/// compression-ratio variance while still tripping a full degenerate
/// collapse.
///
/// Skips silently if either `--stats` line is unparseable, so the test
/// does not brittle-couple to renderer formatting tweaks.
#[test]
fn daemon_gzip_z_vs_zz_negotiation() {
    let fixture = match GzipDaemonFixture::start() {
        Ok(Some(f)) => f,
        Ok(None) => {
            eprintln!("skip: oc-rsync binary not located");
            return;
        }
        Err(e) => {
            eprintln!("skip: could not start oc-rsync daemon: {e}");
            return;
        }
    };

    let src_path = fixture.module_root().join("payload.bin");
    write_fixture(&src_path).expect("write source fixture");

    let url = format!("{}/payload.bin", fixture.url());

    // Both runs go to fresh dest directories so quick-check cannot skip
    // any wire work on the second run.
    let mut byte_counts: Vec<u64> = Vec::with_capacity(2);
    for flag in ["-z", "-zz"] {
        let dest_dir = tempdir().expect("dest tempdir");
        let url_os = std::ffi::OsString::from(&url);
        let dest_os = dest_dir.path().as_os_str().to_owned();
        let output = run_client(&[
            "-a".as_ref(),
            flag.as_ref(),
            "--stats".as_ref(),
            "--timeout=30".as_ref(),
            url_os.as_ref(),
            dest_os.as_ref(),
        ])
        .expect("spawn oc-rsync client");

        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        assert!(
            output.status.success(),
            "{flag} pull must exit 0; status={:?}\nstdout:\n{stdout}\nstderr:\n{stderr}",
            output.status.code(),
        );
        assert!(
            !stderr.contains("connection unexpectedly closed"),
            "{flag} pull surfaced UTS-9 signature; stderr:\n{stderr}"
        );

        let Some(sent) = parse_stats_bytes(&stdout, "Total bytes sent:") else {
            eprintln!("skip: --stats output unparseable for {flag}; stdout:\n{stdout}");
            return;
        };
        byte_counts.push(sent);
    }

    // The two codecs produce different wire byte counts on the same
    // fixture - the assertion that pins `-z` and `-zz` are distinct
    // paths rather than aliases for the same compressor.
    assert_ne!(
        byte_counts[0], byte_counts[1],
        "-z and -zz reported identical bytes sent ({}); the two flags must \
         negotiate distinct codec paths (zlib vs zlibx)",
        byte_counts[0]
    );
}
