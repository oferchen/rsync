//! A remote source with a remote destination is a syntax error.
//!
//! upstream: main.c:1486-1489 start_client() - when the source operand and
//! the destination operand are both host specs, upstream prints "The source
//! and destination cannot both be remote." and exits RERR_SYNTAX (1) before
//! any remote shell is spawned.
//!
//! The `--rsh` command is a script that drops a marker file, so a client that
//! reaches the spawn step at all fails the "no remote shell" assertion even
//! if it later exits 1 for another reason.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::process::Command;

use test_support::oc_rsync_bin;

const REFUSAL: &str = "The source and destination cannot both be remote.";

#[test]
fn remote_to_remote_is_refused_before_any_remote_shell_runs() {
    let tmp = test_support::create_tempdir();
    let marker = tmp.path().join("rsh-ran");
    let rsh = tmp.path().join("fake-rsh");
    std::fs::write(
        &rsh,
        format!("#!/bin/sh\ntouch '{}'\nexit 0\n", marker.display()),
    )
    .expect("write fake rsh");
    std::fs::set_permissions(&rsh, std::fs::Permissions::from_mode(0o755)).expect("chmod fake rsh");

    let output = Command::new(oc_rsync_bin())
        .arg(format!("--rsh={}", rsh.display()))
        .arg("a:/x")
        .arg("b:/y")
        .output()
        .expect("spawn oc-rsync");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "stderr: {stderr}");
    assert!(stderr.contains(REFUSAL), "stderr: {stderr}");
    assert!(
        !marker.exists(),
        "the remote shell was spawned; upstream refuses before any connection"
    );
}
