//! NXT-2: upstream-compat port of `testsuite/daemon-gzip-download.test`.
//!
//! Pairs an upstream rsync client (3.4.4 by default) against an oc-rsync
//! daemon over a real TCP socket. The transfer pulls a ~1 MB fixture with
//! `-zz` (new-style zlibx codec). The invariant under test is that the
//! goodbye envelope reaches the upstream client without a premature
//! connection drop - the regression family that UTS-9.REOPEN /
//! UTS-10.REOPEN / UTS-V3.A surfaced and which PRs #5520, #5600, #5619,
//! and #5725 fixed.
//!
//! # Design references
//!
//! - `docs/design/nxt-1-nextest-harness.md` - NXT-1 harness shape.
//! - `docs/design/uts-nextest-edge-b-test-harness.md` - sibling internal
//!   harness (oc-rsync drives oc-rsync). NXT-2 uses the cross-binary
//!   surface from `test-support::upstream_compat`.
//! - `docs/audits/uts-9-daemon-gzip-download-goodbye.md` - UTS-9
//!   regression analysis the original goodbye flush patch addressed.
//!
//! # Upstream references
//!
//! - `target/interop/upstream-src/rsync-3.4.4/testsuite/daemon-gzip-download.test`
//!   - upstream script this port replaces.
//! - `main.c:983` `do_server_sender()` - `io_flush(FULL_FLUSH)` before
//!   return; contract pinned by UTS-9.
//! - `options.c:2002` - `-zz` -> `compress_choice = "zlibx"` mapping.
//!
//! # Gating
//!
//! The test self-skips on three conditions:
//!
//! 1. `OC_RSYNC_UPSTREAM_COMPAT` env var is not `1` - the standard PR
//!    nextest cell does not set this, so the test no-ops in well under
//!    1 ms.
//! 2. The upstream rsync 3.4.4 binary cannot be located at
//!    `target/interop/upstream-install/3.4.4/bin/rsync` (run
//!    `tools/ci/run_interop.sh` to build it). The `OC_RSYNC_UPSTREAM_BIN_3_4_4`
//!    env var overrides the path.
//! 3. The oc-rsync binary cannot be located via `CARGO_BIN_EXE_oc-rsync`
//!    or by walking up from the current test executable. CI builds it
//!    before invoking nextest, so this only trips in pathological local
//!    setups.
//!
//! The test is `#[cfg(unix)]` because the daemon TCP layer assumes
//! POSIX socket semantics. Windows daemon coverage lives in the daemon
//! crate's chunked tests.

#![cfg(unix)]

use std::env;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use tempfile::{TempDir, tempdir};
use test_support::{UpstreamVersion, require_upstream_rsync, upstream_compat_enabled};

/// Total payload size. Picked to clear the ~615 KB cutoff observed in
/// the UTS-9 capture so the goodbye-flush invariant is exercised on the
/// wire even after the compressible prefix is heavily reduced by zlibx.
const COMPRESSIBLE_BYTES: usize = 512 * 1024;
/// Incompressible tail keeps total wire bytes above the cutoff after
/// compression engages.
const INCOMPRESSIBLE_BYTES: usize = 512 * 1024;

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

/// RAII guard that kills the oc-rsync daemon on drop so a panicking
/// assertion does not leave a dangling listener.
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
/// for it to accept connections.
fn spawn_oc_rsync_daemon(bin: &Path, config_path: &Path) -> io::Result<(DaemonGuard, u16)> {
    // Race-free free port: the default daemon binds SO_REUSEADDR only (upstream
    // socket.c:447), so a collision is a clean EADDRINUSE exit the helper
    // retries. See `test_support::daemon_port`.
    let (child, port) = test_support::spawn_daemon_on_free_port(|port| {
        Command::new(bin)
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
            .spawn()
    })?;
    Ok((DaemonGuard { child }, port))
}

/// Write an `rsyncd.conf` exposing a single read-write module rooted at
/// `module_root`. Matches the sibling tests' minimum-viable shape.
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
         comment = NXT-2 upstream-compat daemon-gzip goodbye fixture\n\
         read only = false\n\
         list = true\n",
        pid = pid_path.display(),
        log = log_path.display(),
        module = module_root.display(),
    );
    fs::write(config_path, body)
}

/// Deterministic compressible payload - high zlib ratio.
fn build_compressible(size: usize) -> Vec<u8> {
    let phrase = b"compressible_repeated_pattern_for_nxt_2_goodbye_flush ";
    phrase.iter().copied().cycle().take(size).collect()
}

/// Deterministic incompressible payload via a small LCG so the byte
/// stream is identical across runs without an external `rand` dep.
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

/// Write the canonical fixture file at `path` and return its raw bytes
/// for later byte-equality comparison against the destination.
fn write_fixture(path: &Path) -> io::Result<Vec<u8>> {
    let mut content = build_compressible(COMPRESSIBLE_BYTES);
    content.extend_from_slice(&build_incompressible(INCOMPRESSIBLE_BYTES));
    let mut f = fs::File::create(path)?;
    f.write_all(&content)?;
    Ok(content)
}

/// Bundle of paths and the daemon guard for one fixture instance.
struct Fixture {
    _workdir: TempDir,
    module_root: PathBuf,
    port: u16,
    _daemon: DaemonGuard,
}

impl Fixture {
    fn start(oc_rsync_bin: &Path) -> io::Result<Self> {
        let workdir = tempdir()?;
        let module_root = workdir.path().join("module");
        fs::create_dir_all(&module_root)?;
        let config_path = workdir.path().join("rsyncd.conf");
        let log_path = workdir.path().join("rsyncd.log");
        let pid_path = workdir.path().join("rsyncd.pid");
        write_daemon_config(&config_path, &log_path, &pid_path, &module_root)?;

        let (daemon, port) = spawn_oc_rsync_daemon(oc_rsync_bin, &config_path)?;

        Ok(Self {
            _workdir: workdir,
            module_root,
            port,
            _daemon: daemon,
        })
    }

    fn url(&self) -> String {
        format!("rsync://127.0.0.1:{}/gzipmod", self.port)
    }
}

/// NXT-2: upstream rsync client pulls a `-zz`-compressed file from an
/// oc-rsync daemon and observes the full goodbye envelope.
///
/// On regression the upstream client surfaces `connection unexpectedly
/// closed` (or a non-zero exit) when the oc-rsync daemon-sender drops
/// the trailing frame before flushing the goodbye exchange.
///
/// Skips silently when `OC_RSYNC_UPSTREAM_COMPAT` is unset or the
/// upstream rsync 3.4.4 binary is missing - this keeps the standard PR
/// nextest cell costless and macOS/Windows cells green.
#[test]
fn daemon_gzip_goodbye_does_not_truncate() {
    if !upstream_compat_enabled() {
        return;
    }

    let Some(upstream) = require_upstream_rsync(UpstreamVersion::V3_4_4) else {
        return;
    };

    let Some(oc_rsync) = locate_oc_rsync() else {
        eprintln!("Skipping: oc-rsync binary not located via CARGO_BIN_EXE_oc-rsync or target/");
        return;
    };

    let fixture = match Fixture::start(&oc_rsync) {
        Ok(f) => f,
        Err(e) => {
            panic!("upstream-compat fixture: could not start oc-rsync daemon: {e}");
        }
    };

    let src_path = fixture.module_root.join("payload.bin");
    let source = write_fixture(&src_path).expect("write daemon-side source fixture");

    let dest_dir = tempdir().expect("dest tempdir");

    let url = format!("{}/payload.bin", fixture.url());
    let output = upstream
        .command()
        .arg("-a")
        .arg("-zz")
        .arg("--stats")
        .arg("--timeout=30")
        .arg(&url)
        .arg(dest_dir.path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("spawn upstream rsync client");

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();

    assert!(
        output.status.success(),
        "upstream rsync {} client pulling -zz from oc-rsync daemon must exit 0; \
         status={:?}\nstdout:\n{stdout}\nstderr:\n{stderr}",
        upstream.version().directory(),
        output.status.code(),
    );

    // UTS-9 fail-loud signature: a regression in the daemon-sender
    // goodbye flush surfaces as `connection unexpectedly closed` on
    // the upstream client's stderr even when the exit code is masked.
    assert!(
        !stderr.contains("connection unexpectedly closed"),
        "upstream client stderr must not contain 'connection unexpectedly closed' \
         (UTS-9 signature); stderr:\n{stderr}"
    );

    // Byte-equality on the destination: a truncated wire produces a
    // short file even when the exit code is masked.
    let dest_file = dest_dir.path().join("payload.bin");
    let received = fs::read(&dest_file).expect("read destination payload");
    assert_eq!(
        received.len(),
        source.len(),
        "destination size must match source; truncation indicates goodbye flush regression"
    );
    assert_eq!(
        received, source,
        "destination must be byte-identical to source"
    );
}
