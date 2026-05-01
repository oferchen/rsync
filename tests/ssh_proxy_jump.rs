//! Integration tests for SSH proxy-jump (`--jump-host`) propagation.
//!
//! Issue #1882 - exercises the proxy-jump feature added in #1881.
//!
//! These tests verify that when `--jump-host=<value>` is passed to oc-rsync,
//! the value is propagated as `-J <value>` to the spawned SSH subprocess in
//! the correct position (after SSH options, before the destination operand).
//!
//! Topology covered:
//! - Single hop: `--jump-host=bastion` -> `ssh ... -J bastion <target> rsync ...`
//! - Multi hop:  `--jump-host=h1,h2`   -> `ssh ... -J h1,h2  <target> rsync ...`
//! - Hop with port: `--jump-host=user@bastion:2200`
//!
//! Verification approach:
//! The tests substitute the system `ssh` binary with a tiny shell-script
//! wrapper named exactly `ssh` (so `is_ssh_program()` in the SSH builder
//! treats it as SSH and injects `-J`). The wrapper records its full argv to
//! a file, then exits non-zero so the transfer aborts deterministically.
//! Each test parses the recorded argv and asserts the `-J <value>` pair
//! appears before the SSH destination argument.
//!
//! Skip conditions:
//! - Windows: skipped via `#![cfg(unix)]` (POSIX shell-script wrapper).
//! - End-to-end test: marked `#[ignore]`; runs only with `--ignored` and
//!   silently skips if sshd or remote rsync are unreachable.
//!
//! End-to-end transfer through a real two-hop SSH topology is intentionally
//! gated behind `#[ignore]`: spinning up two sshd instances on different
//! ports with distinct host keys and authorized_keys is too invasive for
//! the in-tree test suite. The wrapper-based verification proves the
//! wire-level contract (oc-rsync emits `-J` correctly); end-to-end
//! behaviour reduces to OpenSSH's own well-tested `-J` handling. A
//! follow-up test that assumes a pre-staged sshd may be added to the
//! interop harness.
//!
//! Related: issue #1881 (implementation), this test addresses #1882.

#![cfg(unix)]

use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use tempfile::TempDir;

/// Wall-clock cap for each oc-rsync invocation. The wrapper exits immediately
/// after recording its argv, so any single test should finish in milliseconds.
const RUN_TIMEOUT: Duration = Duration::from_secs(30);

/// Resolve the oc-rsync binary path the same way other root-tests do
/// (search the workspace `target/{debug,release,dist}` directories).
fn oc_rsync_binary() -> PathBuf {
    if let Some(env_path) = std::env::var_os("CARGO_BIN_EXE_oc-rsync") {
        let path = PathBuf::from(env_path);
        if path.is_file() {
            return path;
        }
    }
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    for profile in ["debug", "release", "dist"] {
        let path = PathBuf::from(manifest_dir)
            .join("target")
            .join(profile)
            .join("oc-rsync");
        if path.is_file() {
            return path;
        }
    }
    PathBuf::from("oc-rsync")
}

/// Create a POSIX shell-script "ssh" wrapper inside `dir` that writes its
/// full argv (one argument per line) to `record` and exits 255.
///
/// The wrapper is named exactly `ssh` so the SSH builder's `is_ssh_program()`
/// check (basename match) returns true and `-J` is appended.
///
/// Exit 255 mimics OpenSSH's "connection failed" exit status; oc-rsync maps
/// that to a transfer failure, which is exactly what we want - the test
/// shouldn't try to complete a real transfer.
fn write_recorder_ssh(dir: &Path, record: &Path) -> PathBuf {
    let script = dir.join("ssh");
    let body = format!(
        "#!/bin/sh\n\
         : >'{record}'\n\
         for arg in \"$@\"; do\n\
           printf '%s\\n' \"$arg\" >>'{record}'\n\
         done\n\
         exit 255\n",
        record = record.display()
    );
    fs::write(&script, body).expect("write recorder ssh script");
    let mut perms = fs::metadata(&script)
        .expect("stat recorder ssh script")
        .permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&script, perms).expect("chmod recorder ssh script");
    script
}

/// Spawn `oc-rsync` with the supplied args and a process-level timeout.
/// Returns Some(output) on completion, None on timeout (after killing).
fn run_oc_rsync(args: &[OsString]) -> Option<std::process::Output> {
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
            Ok(Some(_)) => {
                return Some(child.wait_with_output().expect("collect oc-rsync output"));
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

/// Read the recorder file as a Vec<String>, one argv element per line.
fn read_recorded_argv(record: &Path) -> Vec<String> {
    let raw = fs::read_to_string(record)
        .unwrap_or_else(|e| panic!("read recorded argv at {}: {e}", record.display()));
    raw.lines().map(str::to_owned).collect()
}

/// Find the index of `flag` in `argv`, asserting it appears before any element
/// equal to `target`. Returns the position of `flag`.
fn assert_flag_before_target(argv: &[String], flag: &str, target: &str) -> usize {
    let flag_idx = argv
        .iter()
        .position(|a| a == flag)
        .unwrap_or_else(|| panic!("expected `{flag}` in recorded argv: {argv:?}"));
    let target_idx = argv
        .iter()
        .position(|a| a == target)
        .unwrap_or_else(|| panic!("expected target `{target}` in recorded argv: {argv:?}"));
    assert!(
        flag_idx < target_idx,
        "`{flag}` (idx {flag_idx}) must precede target `{target}` (idx {target_idx}): {argv:?}"
    );
    flag_idx
}

/// Build the standard pieces a jump-host test needs. The `_temp` field keeps
/// the tempdir alive until the fixture is dropped.
struct Fixture {
    _temp: TempDir,
    src_dir: PathBuf,
    record: PathBuf,
    fake_ssh: PathBuf,
}

fn fixture() -> Fixture {
    let temp = TempDir::new().expect("create tempdir");
    let src_dir = temp.path().join("src");
    fs::create_dir_all(&src_dir).expect("create src dir");
    fs::write(src_dir.join("hello.txt"), b"hello via proxy-jump").expect("seed src file");

    let record = temp.path().join("argv.log");
    let fake_ssh = write_recorder_ssh(temp.path(), &record);

    Fixture {
        _temp: temp,
        src_dir,
        record,
        fake_ssh,
    }
}

#[test]
fn jump_host_single_hop_emits_dash_j_before_target() {
    let fx = fixture();
    let target_host = "dest.example.com";
    let jump = "bastion.example.com";

    let src_arg = format!("{}/", fx.src_dir.display());
    let dst_arg = format!("{target_host}:/dest/");
    let rsh_arg = format!("--rsh={}", fx.fake_ssh.display());
    let jump_arg = format!("--jump-host={jump}");

    let args: Vec<OsString> = [
        "--no-aes",
        rsh_arg.as_str(),
        jump_arg.as_str(),
        src_arg.as_str(),
        dst_arg.as_str(),
    ]
    .iter()
    .map(OsString::from)
    .collect();

    let output = run_oc_rsync(&args).expect("oc-rsync did not exit within timeout");
    // The fake ssh exits 255, so oc-rsync is expected to fail; we only care
    // that the SSH child was spawned and wrote the argv log.
    assert!(
        !output.status.success(),
        "oc-rsync should fail because the fake ssh exits 255: stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );

    let argv = read_recorded_argv(&fx.record);
    assert!(
        !argv.is_empty(),
        "recorder script never received any argv (was the fake ssh actually invoked?)"
    );

    let j_idx = assert_flag_before_target(&argv, "-J", target_host);
    assert_eq!(
        argv.get(j_idx + 1).map(String::as_str),
        Some(jump),
        "`-J` must be followed by the verbatim jump-host value: {argv:?}"
    );
}

#[test]
fn jump_host_multi_hop_chain_forwarded_verbatim() {
    let fx = fixture();
    let target_host = "dest.example.com";
    let jump = "alice@a.example.com,bob@b.example.com";

    let src_arg = format!("{}/", fx.src_dir.display());
    let dst_arg = format!("{target_host}:/dest/");
    let rsh_arg = format!("--rsh={}", fx.fake_ssh.display());
    let jump_arg = format!("--jump-host={jump}");

    let args: Vec<OsString> = [
        "--no-aes",
        rsh_arg.as_str(),
        jump_arg.as_str(),
        src_arg.as_str(),
        dst_arg.as_str(),
    ]
    .iter()
    .map(OsString::from)
    .collect();

    let output = run_oc_rsync(&args).expect("oc-rsync did not exit within timeout");
    assert!(!output.status.success());

    let argv = read_recorded_argv(&fx.record);
    let j_idx = assert_flag_before_target(&argv, "-J", target_host);
    assert_eq!(
        argv.get(j_idx + 1).map(String::as_str),
        Some(jump),
        "comma-separated multi-hop chain must be forwarded as a single argument: {argv:?}"
    );
}

#[test]
fn jump_host_with_port_forwarded_verbatim() {
    let fx = fixture();
    let target_host = "dest.example.com";
    let jump = "user@bastion.example.com:2200";

    let src_arg = format!("{}/", fx.src_dir.display());
    let dst_arg = format!("{target_host}:/dest/");
    let rsh_arg = format!("--rsh={}", fx.fake_ssh.display());
    let jump_arg = format!("--jump-host={jump}");

    let args: Vec<OsString> = [
        "--no-aes",
        rsh_arg.as_str(),
        jump_arg.as_str(),
        src_arg.as_str(),
        dst_arg.as_str(),
    ]
    .iter()
    .map(OsString::from)
    .collect();

    let output = run_oc_rsync(&args).expect("oc-rsync did not exit within timeout");
    assert!(!output.status.success());

    let argv = read_recorded_argv(&fx.record);
    let j_idx = assert_flag_before_target(&argv, "-J", target_host);
    assert_eq!(
        argv.get(j_idx + 1).map(String::as_str),
        Some(jump),
        "host:port form must be forwarded verbatim: {argv:?}"
    );
}

#[test]
fn jump_host_omitted_when_flag_absent() {
    let fx = fixture();
    let target_host = "dest.example.com";

    let src_arg = format!("{}/", fx.src_dir.display());
    let dst_arg = format!("{target_host}:/dest/");
    let rsh_arg = format!("--rsh={}", fx.fake_ssh.display());

    let args: Vec<OsString> = [
        "--no-aes",
        rsh_arg.as_str(),
        src_arg.as_str(),
        dst_arg.as_str(),
    ]
    .iter()
    .map(OsString::from)
    .collect();

    let output = run_oc_rsync(&args).expect("oc-rsync did not exit within timeout");
    assert!(!output.status.success());

    let argv = read_recorded_argv(&fx.record);
    assert!(
        !argv.iter().any(|a| a == "-J"),
        "`-J` must not appear when --jump-host is absent: {argv:?}"
    );
    // Sanity: target must still be in argv, proving the wrapper actually ran.
    assert!(
        argv.iter().any(|a| a == target_host),
        "target host should appear in argv: {argv:?}"
    );
}

#[test]
fn jump_host_empty_value_does_not_emit_dash_j() {
    let fx = fixture();
    let target_host = "dest.example.com";

    let src_arg = format!("{}/", fx.src_dir.display());
    let dst_arg = format!("{target_host}:/dest/");
    let rsh_arg = format!("--rsh={}", fx.fake_ssh.display());

    let args: Vec<OsString> = [
        "--no-aes",
        rsh_arg.as_str(),
        "--jump-host=",
        src_arg.as_str(),
        dst_arg.as_str(),
    ]
    .iter()
    .map(OsString::from)
    .collect();

    let output = run_oc_rsync(&args).expect("oc-rsync did not exit within timeout");
    assert!(!output.status.success());

    let argv = read_recorded_argv(&fx.record);
    assert!(
        !argv.iter().any(|a| a == "-J"),
        "empty --jump-host value must be filtered out and not emit `-J`: {argv:?}"
    );
}

/// End-to-end placeholder that would exercise an actual two-hop SSH topology
/// against upstream rsync. Disabled by default because:
///   1. It requires a running sshd reachable on localhost with passwordless
///      key-based auth (`BatchMode=yes` must succeed end-to-end).
///   2. It requires `rsync` (or `oc-rsync`) installed on the remote side,
///      since the jump-host test does not control the destination shell.
///   3. CI workers vary in availability of sshd, so a green skip is
///      preferred over a flaky red.
///
/// To run locally on a machine with sshd and rsync configured:
///   `cargo nextest run --test ssh_proxy_jump -- --ignored end_to_end`
#[test]
#[ignore = "requires running sshd + rsync on localhost; run manually with --ignored"]
fn end_to_end_proxy_jump_through_localhost() {
    if !ssh_localhost_reachable() {
        eprintln!("Skipping: SSH to localhost not reachable in BatchMode");
        return;
    }
    if !rsync_on_localhost_via_ssh() {
        eprintln!("Skipping: `rsync` not in PATH on localhost via SSH");
        return;
    }

    let temp = TempDir::new().expect("create tempdir");
    let src_dir = temp.path().join("src");
    let dst_dir = temp.path().join("dst");
    fs::create_dir_all(&src_dir).expect("create src dir");
    fs::create_dir_all(&dst_dir).expect("create dst dir");
    fs::write(src_dir.join("hello.txt"), b"hello e2e").expect("seed src");

    // Loop the jump through localhost itself, so the connection is:
    //   client -> ssh -J localhost localhost -> sshd on the same host
    // OpenSSH handles the hop natively. This mirrors a real two-hop topology
    // without requiring two distinct sshd instances.
    let src_arg = format!("{}/", src_dir.display());
    let dst_arg = format!("localhost:{}/", dst_dir.display());

    let args: Vec<OsString> = [
        "--rsh=ssh -o BatchMode=yes -o StrictHostKeyChecking=accept-new",
        "--jump-host=localhost",
        "--times",
        src_arg.as_str(),
        dst_arg.as_str(),
    ]
    .iter()
    .map(OsString::from)
    .collect();

    let output = run_oc_rsync(&args).expect("oc-rsync did not exit within timeout");
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        panic!("end-to-end proxy-jump transfer failed: {stderr}");
    }
    let dest_file = dst_dir.join("hello.txt");
    assert!(
        dest_file.exists(),
        "transferred file should exist after proxy-jump transfer"
    );
    assert_eq!(fs::read(&dest_file).expect("read dst"), b"hello e2e");
}

/// Probe whether SSH to localhost works in BatchMode. Mirrors the helper in
/// `tests/ssh_transport.rs` to keep skip semantics consistent.
fn ssh_localhost_reachable() -> bool {
    Command::new("ssh")
        .args([
            "-o",
            "BatchMode=yes",
            "-o",
            "ConnectTimeout=3",
            "-o",
            "StrictHostKeyChecking=accept-new",
            "localhost",
            "true",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Probe whether `rsync` is on PATH on the localhost SSH endpoint.
fn rsync_on_localhost_via_ssh() -> bool {
    Command::new("ssh")
        .args([
            "-o",
            "BatchMode=yes",
            "-o",
            "ConnectTimeout=3",
            "localhost",
            "command",
            "-v",
            "rsync",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}
