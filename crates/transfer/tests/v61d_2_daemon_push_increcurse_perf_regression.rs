//! V61D-2 - v0.6.1 daemon-push performance regression repro.
//!
//! Locks in the symptom the V61D-1 audit (`docs/audit/v061-daemon-push-
//! regression.md`) traced to PR #3557's flip of `inc_recursive_send` to
//! `true` by default: push transfers (client-as-sender) against an
//! upstream rsync daemon ran 95-201x slower than the v0.6.0 baseline.
//! The mitigation (PR #3744) restored the default to `false` and gated
//! the flip behind the `sender-inc-recurse` cargo feature.
//!
//! This test rebuilds the failure mode under that feature. It pushes a
//! small (~1 MiB) source tree from `oc-rsync` (sender) to an upstream
//! `rsync --daemon` (receiver) and times the transfer, then repeats the
//! same push with upstream `rsync` as the sender to capture a baseline.
//! Asserts the oc-rsync push completes within 5x of the upstream
//! baseline. The v0.6.1 symptom was 95-201x, so 5x is a generous
//! threshold that still trips on a real regression while tolerating
//! normal benchmark noise on CI runners.
//!
//! ## Feature gate
//!
//! `#[cfg(feature = "sender-inc-recurse")]` - mirrors ISI.c/d/e. Without
//! the feature, `ClientConfigBuilder::build()` leaves
//! `inc_recursive_send = false`, the capability string omits `'i'`, the
//! upstream peer clears `allow_inc_recurse`, and both sides walk the
//! fully-baked non-INC_RECURSE sender path - exactly the post-mitigation
//! state where no regression is observable. The feature must be on for
//! the test to exercise the at-risk path.
//!
//! ## Platform gate
//!
//! `#[cfg(all(unix, not(target_os = "macos")))]` - the upstream rsync
//! binaries the harness depends on are only pre-built for Linux in
//! `tools/ci/run_interop.sh`. macOS has no `target/interop/upstream-
//! install` tree in standard CI, so the test would be a perpetual skip
//! there; gating at compile time keeps the cfg surface honest.
//!
//! ## Upstream availability
//!
//! Looked up from `target/interop/upstream-install/3.4.1/bin/rsync`. If
//! the binary is missing the test logs `skip:` and returns successfully -
//! it does not fail purely because the binary is absent. Run
//! `bash tools/ci/run_interop.sh` to populate the install tree.
//!
//! ## References
//!
//! - V61D-1 audit (`docs/audit/v061-daemon-push-regression.md`) - root
//!   cause and mitigation summary.
//! - V61D-5 design (planned `docs/design/v061-daemon-push-increcurse-
//!   disable.md`) - documents the disable plan this test guards.
//! - ISI.a (`docs/design/isi-a-sender-inc-recurse-call-graph.md`) -
//!   sender-side call graph.
//! - PR #3557 / commit `39d47722b` - the regression-introducing flip.
//! - PR #3744 / commit `b3a264061` - the mitigation that restored the
//!   default to `false`.

#![cfg(all(unix, not(target_os = "macos"), feature = "sender-inc-recurse"))]

use std::env;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use tempfile::TempDir;

/// Total fixture size in bytes - ~1 MiB across many files.
///
/// Kept small so CI wall-clock stays bounded even under the regressed
/// path: the v0.6.1 symptom was a 95-201x slowdown on initial sync, so
/// at 5x of the baseline this test still completes in seconds.
const TOTAL_BYTES: usize = 1_048_576;

/// Per-file payload size in bytes. 1 KiB keeps the file count at ~1 024
/// so the source has enough entries to exercise the sender's flist build
/// path without inflating tempdir teardown cost.
const FILE_PAYLOAD_BYTES: usize = 1_024;

/// Files per directory in the fixture. Spreads the file count across
/// directories so the sender walks a tree, not a flat list - the v0.6.1
/// regression's flist-construction cost grew with both file count and
/// directory count.
const FILES_PER_DIR: usize = 64;

/// Maximum slowdown factor over the upstream baseline before the test
/// fails. The v0.6.1 symptom was 95-201x; 5x catches a regression of
/// that magnitude with enough headroom to absorb CI runner noise (we
/// have observed ~2x variance across consecutive `cargo nextest` runs
/// on the same machine for sub-second push transfers).
const MAX_SLOWDOWN_RATIO: f64 = 5.0;

/// Locate the `oc-rsync` binary the test runner built.
///
/// Cargo injects `CARGO_BIN_EXE_oc-rsync` when the integration test sees
/// the workspace binary as a dev-dependency; for the `transfer` crate
/// that injection is not guaranteed, so fall back to walking the
/// `target/` tree, mirroring `tests/inc_recurse_single_segment_push_isi_c.rs`.
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

/// Locate the upstream rsync binary for the requested version.
///
/// Looks under `target/interop/upstream-install/<version>/bin/rsync`,
/// the canonical install path populated by `tools/ci/run_interop.sh`.
fn upstream_rsync_binary(version: &str) -> Option<PathBuf> {
    let path = PathBuf::from(format!(
        "target/interop/upstream-install/{version}/bin/rsync"
    ));
    if path.is_file() { Some(path) } else { None }
}

/// Build a ~1 MiB source tree spread across small directories.
///
/// `TOTAL_BYTES / FILE_PAYLOAD_BYTES` files, deterministic content, so
/// every run sees identical wire bytes and the timing comparison
/// reflects only sender behaviour, not fixture noise.
fn build_fixture(root: &Path) -> io::Result<()> {
    fs::create_dir_all(root)?;
    let file_count = TOTAL_BYTES / FILE_PAYLOAD_BYTES;
    let dir_count = file_count.div_ceil(FILES_PER_DIR);
    let mut payload = vec![0u8; FILE_PAYLOAD_BYTES];
    let mut written = 0usize;
    for d in 0..dir_count {
        let dir = root.join(format!("dir_{d:04}"));
        fs::create_dir_all(&dir)?;
        let in_this_dir = FILES_PER_DIR.min(file_count - written);
        for f in 0..in_this_dir {
            for (byte_idx, slot) in payload.iter_mut().enumerate() {
                *slot = ((d as u32)
                    .wrapping_mul(1009)
                    .wrapping_add((f as u32).wrapping_mul(31))
                    .wrapping_add(byte_idx as u32)) as u8;
            }
            let path = dir.join(format!("file_{f:04}.bin"));
            let mut file = File::create(&path)?;
            file.write_all(&payload)?;
        }
        written += in_this_dir;
        if written >= file_count {
            break;
        }
    }
    Ok(())
}

/// Write an `rsyncd.conf` exposing a single read-write module rooted at
/// `module_root`. `use chroot = false` and `read only = false` are both
/// required so the unprivileged test process can drive a push transfer
/// without root.
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
         [push]\n\
         path = {module}\n\
         comment = v61d-2 perf repro\n\
         read only = false\n\
         write only = false\n\
         list = true\n\
         uid = nobody\n\
         gid = nobody\n",
        pid = pid_path.display(),
        log = log_path.display(),
        module = module_root.display(),
    );
    // Strip the uid/gid lines on systems where the user cannot drop
    // privileges (rsync rejects the directive when not running as root).
    // Upstream rsync silently ignores uid/gid when the daemon is not
    // started as root, so leaving the lines in is harmless for the
    // unprivileged test runner.
    fs::write(config_path, body)
}

/// Guard that kills the upstream daemon on drop so a panicking test
/// does not leak a TCP listener.
struct DaemonGuard {
    child: Child,
}

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Spawn `rsync --daemon` on `port` against `config_path`. Waits for the
/// port to accept connections before returning.
fn spawn_upstream_daemon(rsync_bin: &Path, config_path: &Path) -> io::Result<(DaemonGuard, u16)> {
    // Race-free free port: upstream rsync binds SO_REUSEADDR only (socket.c:447),
    // so a collision is a clean EADDRINUSE exit the helper retries. See
    // `test_support::daemon_port`.
    let (child, port) = test_support::spawn_daemon_on_free_port(|port| {
        Command::new(rsync_bin)
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

/// Drive one push transfer against `rsync://127.0.0.1:<port>/push/` and
/// return the wall-clock duration. Fails if the sender's exit status is
/// non-zero so a regression that produces "no error, just slow" still
/// distinguishes itself from a hard failure mode the test cannot blame
/// on INC_RECURSE.
fn time_push(sender_bin: &Path, src: &Path, port: u16) -> io::Result<Duration> {
    let dst_url = format!("rsync://127.0.0.1:{port}/push/");
    let start = Instant::now();
    let output = Command::new(sender_bin)
        .arg("-a")
        .arg("--no-owner")
        .arg("--no-group")
        .arg(format!("{}/", src.display()))
        .arg(&dst_url)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()?;
    let elapsed = start.elapsed();

    if !output.status.success() {
        return Err(io::Error::other(format!(
            "push failed: sender={:?} status={:?}\nstdout:\n{}\nstderr:\n{}",
            sender_bin,
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        )));
    }
    Ok(elapsed)
}

/// End-to-end repro: push the same ~1 MiB tree twice (oc-rsync sender,
/// then upstream sender) against the same upstream rsync daemon and
/// assert the oc-rsync push stayed within `MAX_SLOWDOWN_RATIO` of the
/// upstream baseline.
///
/// The receiver-side module is reset between pushes so neither transfer
/// gets a quick-check head start from leftover destination files.
#[test]
fn daemon_push_under_inc_recurse_stays_within_5x_of_upstream_sender_baseline() {
    let oc_bin = match locate_oc_rsync() {
        Some(p) => p,
        None => {
            eprintln!("skip: oc-rsync binary not located");
            return;
        }
    };
    let up_bin = match upstream_rsync_binary("3.4.1") {
        Some(p) => p,
        None => {
            eprintln!(
                "skip: upstream rsync 3.4.1 not installed at \
                 target/interop/upstream-install/3.4.1/bin/rsync; \
                 run tools/ci/run_interop.sh"
            );
            return;
        }
    };

    let tmp = TempDir::new().expect("tempdir");
    let src = tmp.path().join("src");
    let module_root = tmp.path().join("module");
    fs::create_dir_all(&module_root).expect("module root");
    build_fixture(&src).expect("build fixture");

    let config_path = tmp.path().join("rsyncd.conf");
    let log_path = tmp.path().join("rsyncd.log");
    let pid_path = tmp.path().join("rsyncd.pid");
    write_daemon_config(&config_path, &log_path, &pid_path, &module_root)
        .expect("write daemon config");

    let (_daemon, port) = match spawn_upstream_daemon(&up_bin, &config_path) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("skip: could not start upstream rsync daemon: {e}");
            return;
        }
    };

    // Sender under test: oc-rsync built with sender-inc-recurse ON. This
    // is the at-risk path the V61D-1 audit identified.
    let oc_elapsed = time_push(&oc_bin, &src, port).expect("oc-rsync push must succeed");

    // Reset the destination so the baseline push does not get a
    // quick-check skip on every file from leftover destination state.
    fs::remove_dir_all(&module_root).expect("clear module root");
    fs::create_dir_all(&module_root).expect("recreate module root");

    // Baseline: upstream rsync as sender against the same daemon. Same
    // wire path, same fixture, only the sender binary differs.
    let upstream_elapsed = time_push(&up_bin, &src, port).expect("upstream push must succeed");

    let ratio = oc_elapsed.as_secs_f64() / upstream_elapsed.as_secs_f64().max(1e-6);

    assert!(
        ratio <= MAX_SLOWDOWN_RATIO,
        "v0.6.1 daemon-push regression: oc-rsync sender {oc_elapsed:?} vs upstream baseline {upstream_elapsed:?} \
         (ratio {ratio:.2}x, threshold {MAX_SLOWDOWN_RATIO:.1}x). See docs/audit/v061-daemon-push-regression.md."
    );
}
