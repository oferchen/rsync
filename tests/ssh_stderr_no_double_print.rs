//! Regression tests: the SSH client must not print a remote/child stderr line
//! twice.
//!
//! Over a remote shell, oc-rsync drains the child's stderr on a background
//! thread that forwards each line to our own stderr in real time - mirroring
//! upstream rsync, which inherits ssh's fd 2 so ssh (and the remote rsync's own
//! `msgs2stderr=2` diagnostics) print straight to the terminal. A prior bug
//! also appended the same captured bytes to the final `rsync error:` line under
//! an invented `SSH stderr:` header, so every diagnostic printed twice.
//!
//! Upstream never re-emits ssh's stderr after the child exits; the live stream
//! is the one and only copy. These tests spawn the real binary (the only way to
//! observe the process's actual stderr) and assert each diagnostic appears
//! exactly once, while still confirming genuine ssh-transport errors survive.
//!
//! Two paths are covered:
//!  - exit 255 handshake-failure path: a fake ssh that fails before any
//!    handshake (auth failure / connection refused / command not found). This
//!    is deterministic and needs no external rsync.
//!  - exit 23 post-transfer path: oc-rsync pulling a missing source from a real
//!    upstream rsync server, whose sender writes `link_stat ... failed` to its
//!    own fd 2 (upstream `log.c:330`, default `msgs2stderr=2`). Gated on a real
//!    rsync being available; skipped cleanly otherwise.

#![cfg(unix)]

use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use tempfile::TempDir;

const RUN_TIMEOUT: Duration = Duration::from_secs(30);

/// Locates the binary under test. `CARGO_BIN_EXE_oc-rsync` is a compile-time
/// variable, so it must be read with `env!`, never `env::var_os`.
fn oc_rsync_binary() -> PathBuf {
    let built = PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync"));
    assert!(
        built.is_file(),
        "oc-rsync binary missing at {}; refusing to fall back to a stale build",
        built.display()
    );
    built
}

/// Writes an executable POSIX script at `path` with `body` and `0o755` perms.
fn write_script(path: &Path, body: &str) {
    fs::write(path, body).expect("write script");
    let mut perms = fs::metadata(path).expect("stat script").permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms).expect("chmod script");
}

/// Spawns oc-rsync with a wall-clock cap; returns the captured output.
fn run_oc_rsync(args: &[OsString]) -> Output {
    let mut child = Command::new(oc_rsync_binary())
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn oc-rsync");

    let deadline = Instant::now() + RUN_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return child.wait_with_output().expect("collect oc-rsync output"),
            Ok(None) => {
                assert!(
                    Instant::now() < deadline,
                    "oc-rsync did not exit within {RUN_TIMEOUT:?}"
                );
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => panic!("waiting for oc-rsync failed: {e}"),
        }
    }
}

/// Counts non-overlapping occurrences of `needle` in `haystack`.
fn count(haystack: &str, needle: &str) -> usize {
    haystack.matches(needle).count()
}

/// A genuine ssh-transport failure (bad target, auth failure, command not
/// found) writes to ssh's stderr and exits 255 before any rsync handshake. The
/// error must still reach the user - exactly once - and must not be echoed a
/// second time under an `SSH stderr:` header.
#[test]
fn genuine_ssh_error_prints_exactly_once() {
    let temp = TempDir::new().expect("tempdir");
    let dst = temp.path().join("dst");
    fs::create_dir_all(&dst).expect("create dst");

    // A fake ssh that mimics OpenSSH's auth failure: one stderr line, exit 255.
    let fake_ssh = temp.path().join("fake_ssh.sh");
    let marker = "Permission denied (publickey).";
    write_script(
        &fake_ssh,
        &format!("#!/bin/sh\nprintf '%s\\n' '{marker}' 1>&2\nexit 255\n"),
    );

    let rsh = format!("--rsh={}", fake_ssh.display());
    let dst_arg = format!("{}/", dst.display());
    let args: Vec<OsString> = ["--no-aes", &rsh, "dummyhost:/src", &dst_arg]
        .iter()
        .map(OsString::from)
        .collect();

    let out = run_oc_rsync(&args);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert!(
        !out.status.success(),
        "transfer should fail when ssh exits 255: {stderr}"
    );
    assert_eq!(
        count(&stderr, marker),
        1,
        "genuine ssh error must print exactly once (not zero, not doubled): {stderr}"
    );
    assert!(
        !stderr.contains("SSH stderr:"),
        "must not re-emit captured stderr under an invented header: {stderr}"
    );
}

/// Locates a usable upstream rsync for the exit-23 path. Honours
/// `OC_RSYNC_UPSTREAM_RSYNC`, else falls back to `rsync` on PATH. Returns
/// `None` (test skips) when none runs.
fn upstream_rsync() -> Option<PathBuf> {
    let candidates = std::env::var_os("OC_RSYNC_UPSTREAM_RSYNC")
        .map(PathBuf::from)
        .into_iter()
        .chain(std::iter::once(PathBuf::from("rsync")));
    for cand in candidates {
        let ok = Command::new(&cand)
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if ok {
            return Some(cand);
        }
    }
    None
}

/// oc-rsync pulling a missing source from a real upstream rsync server. The
/// remote sender reports `link_stat "<path>" failed` on its own fd 2 (upstream
/// default `msgs2stderr=2`), which flows back over the remote shell's stderr.
/// It must surface exactly once; before the fix it printed twice (once live,
/// once under `SSH stderr:`). Exercises the post-transfer (exit 23) code path.
#[test]
fn upstream_sender_error_prints_exactly_once() {
    let Some(rsync) = upstream_rsync() else {
        eprintln!("skipping: no usable upstream rsync found (set OC_RSYNC_UPSTREAM_RSYNC)");
        return;
    };

    let temp = TempDir::new().expect("tempdir");
    let dst = temp.path().join("dst");
    fs::create_dir_all(&dst).expect("create dst");

    // A remote-shell wrapper that drops the host operand and execs the server
    // command locally, emulating ssh handing the connection to the remote
    // rsync. The server's fd 2 rides the wrapper's stderr, exactly as ssh
    // forwards the remote fd 2.
    let wrapper = temp.path().join("rsh.sh");
    write_script(&wrapper, "#!/bin/sh\nshift\nexec \"$@\"\n");

    let missing = "/oc-rsync-nonexistent-source-xyz";
    let rsh = format!("--rsh={}", wrapper.display());
    let rsync_path = format!("--rsync-path={}", rsync.display());
    let src_arg = format!("dummyhost:{missing}");
    let dst_arg = format!("{}/", dst.display());
    let args: Vec<OsString> = ["-a", &rsh, &rsync_path, &src_arg, &dst_arg]
        .iter()
        .map(OsString::from)
        .collect();

    let out = run_oc_rsync(&args);
    let stderr = String::from_utf8_lossy(&out.stderr);

    // Any rsync 3.x reports the missing source this way; if this rsync does not
    // (e.g. a very old build), the scenario under test never triggers - skip
    // rather than assert against an absent diagnostic.
    if !stderr.contains("link_stat") {
        eprintln!(
            "skipping: rsync at {} did not emit a link_stat error: {stderr}",
            rsync.display()
        );
        return;
    }

    assert_eq!(
        out.status.code(),
        Some(23),
        "missing source must exit 23 (RERR_PARTIAL): {stderr}"
    );
    assert_eq!(
        count(&stderr, "link_stat"),
        1,
        "the remote sender error must print exactly once, not doubled: {stderr}"
    );
    assert!(
        !stderr.contains("SSH stderr:"),
        "must not re-emit the remote stderr under an invented header: {stderr}"
    );
}
