//! Wire-equivalence integration test that shells out to the tcpdump-based
//! harness in `scripts/wire-equivalence-tcpdump.sh`.
//!
//! The test only runs end-to-end when the host can actually capture loopback
//! traffic and has both an upstream `rsync` binary and an `oc-rsync` binary
//! reachable. On every other host (CI runners without CAP_NET_RAW, macOS,
//! Windows, or hosts missing `tshark`) the test prints a skip notice and
//! returns successfully so it never blocks the default workspace build.
//!
//! The harness asserts: given the same source tree, the SHA-256 of the
//! tshark-extracted, port-normalized application payload exchanged with the
//! upstream rsync 3.4.1 daemon equals the SHA-256 of the payload exchanged
//! with the oc-rsync daemon.
//!
//! Known limitations of the byte-level diff:
//!   * Per-session random seeds (checksum seed, MD5 negotiation nonce) make
//!     the modern protocol non-deterministic in the general case. The
//!     scenario keeps the file set small and deterministic so the diff
//!     stays useful as a regression signal, but a hash mismatch is not by
//!     itself proof of a protocol bug. Use the retained pcaps for triage.
//!   * Timestamps, TCP sequence numbers, and ephemeral source ports are
//!     stripped by the harness; only the application-layer payload bytes
//!     are hashed.

use std::path::PathBuf;
use std::process::Command;

fn repo_root() -> PathBuf {
    // CARGO_MANIFEST_DIR points at crates/protocol; walk up two levels.
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    PathBuf::from(manifest_dir)
        .parent()
        .and_then(|p| p.parent())
        .expect("workspace root above crates/protocol")
        .to_path_buf()
}

fn tool_on_path(name: &str) -> bool {
    Command::new(name)
        .arg("--version")
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

#[test]
fn wire_equivalence_tcpdump_harness() {
    if !cfg!(target_os = "linux") {
        eprintln!("skip: wire-equivalence harness is Linux-only");
        return;
    }

    for tool in ["tcpdump", "tshark", "sha256sum", "rsync"] {
        if !tool_on_path(tool) {
            eprintln!("skip: required tool not on PATH: {tool}");
            return;
        }
    }

    let script = repo_root().join("scripts/wire-equivalence-tcpdump.sh");
    if !script.exists() {
        eprintln!("skip: harness script missing at {}", script.display());
        return;
    }

    // Run via `sh` so the test does not depend on the executable bit being
    // preserved by the checkout (Windows shares, zipballs, etc.).
    let output = match Command::new("sh").arg(&script).output() {
        Ok(out) => out,
        Err(err) => {
            eprintln!("skip: failed to spawn harness: {err}");
            return;
        }
    };

    // Exit code 77 is the harness "skip" signal (missing CAP_NET_RAW, no
    // upstream binary, etc.). Treat it as a non-failing skip here too.
    if output.status.code() == Some(77) {
        eprintln!(
            "skip: harness reported missing prerequisites\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        return;
    }

    assert!(
        output.status.success(),
        "wire-equivalence harness reported divergence or failure (exit {:?})\nstdout:\n{}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}
