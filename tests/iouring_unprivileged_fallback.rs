//! Real-container verification that oc-rsync's io_uring fallback survives
//! the seccomp / capability restrictions imposed by an unprivileged
//! podman/docker runtime.
//!
//! The companion unit-level coverage for the in-process probe lives in
//! `crates/fast_io/tests/io_uring_probe_fallback.rs`; that file mocks the
//! probe outcomes. This test goes the other way: it shells out to
//! `tools/ci/test_iouring_unprivileged.sh`, which boots an actual
//! unprivileged container and runs a real local-to-local transfer.
//!
//! The test is opt-in because it requires a container runtime on the host.
//! Set `OC_RSYNC_IOURING_CONTAINER_TEST=1` to enable it. With the variable
//! unset (the default), the test exits successfully without doing any work
//! so that nextest stays green on developer machines and CI runners that
//! cannot launch containers. The driving script does the same kind of
//! graceful skip if the binary is present but cannot run containers (for
//! example, when the runtime daemon is unreachable).

use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Environment variable that gates this test. Set to `1` to actually run
/// the container scenarios.
const ENABLE_VAR: &str = "OC_RSYNC_IOURING_CONTAINER_TEST";

/// Locate the workspace root by walking up from `CARGO_MANIFEST_DIR` until
/// we find the top-level `tools/ci` directory. Returns `None` only if the
/// repository layout is unrecognisable, in which case the test skips.
fn workspace_root() -> Option<PathBuf> {
    let manifest_dir = env::var_os("CARGO_MANIFEST_DIR")?;
    let mut path = PathBuf::from(manifest_dir);
    loop {
        if path.join("tools").join("ci").is_dir() {
            return Some(path);
        }
        if !path.pop() {
            return None;
        }
    }
}

fn script_path(root: &Path) -> PathBuf {
    root.join("tools")
        .join("ci")
        .join("test_iouring_unprivileged.sh")
}

#[test]
fn iouring_unprivileged_container_fallback() {
    if env::var_os(ENABLE_VAR).is_none() {
        eprintln!("skip: {ENABLE_VAR} is unset; set it to 1 to run the container scenarios");
        return;
    }

    let root = match workspace_root() {
        Some(p) => p,
        None => {
            eprintln!("skip: could not locate workspace root from CARGO_MANIFEST_DIR");
            return;
        }
    };

    let script = script_path(&root);
    if !script.is_file() {
        eprintln!(
            "skip: driver script not found at {} - was it removed from the tree?",
            script.display()
        );
        return;
    }

    // Always invoke through `bash` so the file does not need an executable
    // bit set on every checkout (Windows checkouts strip it, and some
    // sparse-checkout flows reset it). The script itself has a clean
    // shebang for direct invocation as well.
    let mut cmd = Command::new("bash");
    cmd.arg(&script);
    cmd.current_dir(&root);

    eprintln!("running {}", script.display());
    let status = cmd
        .status()
        .expect("failed to spawn bash for the unprivileged io_uring container test");

    assert!(
        status.success(),
        "tools/ci/test_iouring_unprivileged.sh exited with {status:?}"
    );
}
