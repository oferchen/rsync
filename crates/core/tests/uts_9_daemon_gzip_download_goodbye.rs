//! Regression test for UTS-9.REOPEN: daemon-gzip download goodbye flush.
//!
//! Symptom: when an oc-rsync client pulls a file from an oc-rsync daemon with
//! `-zz` (new-style compression), the receiver could observe a connection
//! close at protocol byte ~612425 mid-stream, before the goodbye exchange
//! completed. The wire ended without an `@ERROR` frame, so the receiver
//! reported `connection unexpectedly closed`.
//!
//! Root cause: `GeneratorContext::run()` did not call `writer.flush()` after
//! `handle_goodbye()` returned. Any diagnostic frame queued after the
//! goodbye exchange (`MSG_INFO`, stats summary, etc.) could race the
//! transport FIN and end up in a torn capture. The fix in
//! `crates/transfer/src/generator/transfer/orchestrator.rs` mirrors upstream
//! `main.c:983` `do_server_sender()` which calls `io_flush(FULL_FLUSH)`
//! immediately before returning.
//!
//! This test reproduces the daemon-pull `-zz` codepath that the UTS-9
//! audit identified: an oc-rsync client (receiver) pulling a deterministic
//! ~700 KB file from an oc-rsync daemon (sender) with `-zz` compression.
//! 700 KB is sized to clear the 612425-byte cutoff observed in the original
//! capture. The transfer must complete with exit 0, the destination must
//! match the source byte-for-byte, and stderr must not contain the
//! `connection unexpectedly closed` signature.
//!
//! Companion fix and analysis live in PR #5609 (UTS-15.c) which added the
//! same flush to handle the upstream batch-mode interop suite. This test
//! locks the contract for the UTS-9 daemon-pull `-zz` lineage so a future
//! refactor cannot regress either codepath without tripping a regression.
//!
//! Upstream references:
//! - `main.c:983` `do_server_sender()` - `io_flush(FULL_FLUSH)` before return
//! - `main.c:1344` `client_run()` - `io_flush(FULL_FLUSH)` before return
//! - `main.c:875-906` `read_final_goodbye()` - goodbye exchange contract

#[cfg(unix)]
mod common;

#[cfg(unix)]
use common::{DaemonBinary, TestDaemon, create_test_file};

#[cfg(unix)]
use std::env;
#[cfg(unix)]
use std::fs;
#[cfg(unix)]
use std::path::PathBuf;
#[cfg(unix)]
use std::process::Command;

#[cfg(unix)]
use tempfile::tempdir;

/// Deterministic ~700 KB payload - large enough to cross the 612425-byte
/// cutoff observed in the original UTS-9 capture, small enough to keep the
/// test fast on CI runners.
#[cfg(unix)]
const TEST_FILE_SIZE: usize = 700 * 1024;

/// Locate the oc-rsync binary for subprocess spawning. Mirrors the helper in
/// `partial_mid_transfer_kill.rs`; kept inline to avoid touching shared
/// `common::mod` from a regression test added late in the cycle.
#[cfg(unix)]
fn locate_oc_rsync() -> Option<PathBuf> {
    if let Some(path) = env::var_os("CARGO_BIN_EXE_oc-rsync") {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Some(path);
        }
    }

    let binary_name = format!("oc-rsync{}", env::consts::EXE_SUFFIX);
    let current_exe = env::current_exe().ok()?;
    let mut dir = current_exe.parent()?;

    while !dir.ends_with("target") {
        let candidate = dir.join(&binary_name);
        if candidate.is_file() {
            return Some(candidate);
        }
        dir = dir.parent()?;
    }

    for subdir in ["debug", "release"] {
        let candidate = dir.join(subdir).join(&binary_name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }

    None
}

/// Deterministic byte pattern so the destination can be compared
/// byte-for-byte without storing the full payload in the test binary.
#[cfg(unix)]
fn generate_test_data(size: usize) -> Vec<u8> {
    let pattern: Vec<u8> = (0..=255u8).collect();
    pattern.iter().copied().cycle().take(size).collect()
}

/// UTS-9.REOPEN.5 regression: oc-rsync client pulling a ~700 KB file from
/// an oc-rsync daemon with `-zz` (new-style compression) must complete
/// without a mid-stream connection drop.
///
/// The test is `#[ignore]` because it requires both the oc-rsync binary
/// and a free TCP port. Run with `cargo nextest run --run-ignored` or
/// `cargo test -- --ignored` during interop validation.
#[cfg(unix)]
#[test]
#[ignore = "requires oc-rsync binary"]
fn daemon_download_with_zz_completes_without_connection_drop() {
    let oc_rsync = match locate_oc_rsync() {
        Some(path) => path,
        None => {
            eprintln!("Skipping: oc-rsync binary not found");
            return;
        }
    };

    let daemon = TestDaemon::start(DaemonBinary::OcRsync).expect("start oc-rsync daemon");

    let test_data = generate_test_data(TEST_FILE_SIZE);
    create_test_file(&daemon.module_path().join("uts9.bin"), &test_data);

    let dest_dir = tempdir().expect("create dest dir");
    let dest_file = dest_dir.path().join("uts9.bin");

    // `-azz` archives + new-style compression (zlibx in upstream's
    // options.c:2012 mapping). The doubled `-z` is the trigger that
    // reproduces the original UTS-9 wire pattern; archive mode (`-a`)
    // is included so the codepath exercises the standard pull flow
    // rather than a degenerate single-file mode.
    let output = Command::new(&oc_rsync)
        .arg("-azz")
        .arg("--timeout=30")
        .arg(format!("{}/uts9.bin", daemon.url()))
        .arg(dest_dir.path().as_os_str())
        .output()
        .expect("spawn oc-rsync client");

    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();

    assert!(
        output.status.success(),
        "daemon-pull -zz must complete with exit 0; status={:?}\nstdout: {stdout}\nstderr: {stderr}\ndaemon log: {}",
        output.status.code(),
        daemon
            .log_contents()
            .unwrap_or_else(|_| "(unavailable)".into())
    );

    // Wire-level fail-loud: the original UTS-9 capture surfaced as
    // `connection unexpectedly closed`. If the goodbye flush regresses,
    // this signature reappears even when the exit code is masked.
    assert!(
        !stderr.contains("connection unexpectedly closed"),
        "stderr must not contain 'connection unexpectedly closed' (UTS-9 signature); stderr: {stderr}"
    );

    let received = fs::read(&dest_file).expect("read transferred file");
    assert_eq!(
        received.len(),
        test_data.len(),
        "transferred file size must match source"
    );
    assert_eq!(
        received, test_data,
        "transferred file must match source byte-for-byte"
    );
}
