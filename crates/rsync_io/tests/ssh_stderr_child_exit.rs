//! Integration test for SSE-7 (#2376) covering the SSH child-exit and
//! stderr-capture contract established by SSE-3 (#2372) and SSE-4
//! (#2373). The socketpair-backed stderr drain configured under the
//! `ssh-socketpair-stderr` feature must surface both the non-zero
//! `ExitStatus` returned by the spawned child and the bytes the child
//! wrote to its stderr endpoint before exiting. Losing either signal
//! turns a failed SSH connection into a silent failure with no
//! diagnostic context for the user.
//!
//! The test is gated on Unix because socketpair-backed stderr is
//! Unix-only until SSE-5 lands the Windows shim. The feature gate
//! mirrors the staging plan in
//! `docs/design/socketpair-stderr-channel.md`: the channel must remain
//! opt-in until parity tests ship, and this file is one of those tests.

#![cfg(all(unix, feature = "ssh-socketpair-stderr"))]

use std::ffi::OsString;

use rsync_io::ssh::SshCommand;

/// Spawns `/bin/sh -c 'echo "fatal: cannot connect" >&2; exit 7'` via
/// the SSH command builder, drains stderr through the auxiliary
/// channel, and asserts both the exit code and the captured diagnostic
/// reach the caller untouched.
#[test]
fn child_non_zero_exit_and_stderr_surface_to_caller() {
    let mut command = SshCommand::new("ignored");
    command.set_program("/bin/sh");
    command.set_batch_mode(false);
    command.push_option("-c");
    command.push_option("echo \"fatal: cannot connect\" >&2; exit 7");
    command.set_target_override(Some(OsString::new()));

    let connection = command.spawn().expect("spawn /bin/sh");
    let (status, stderr) = connection
        .wait_with_stderr()
        .expect("wait_with_stderr returns the child status");

    assert!(
        !status.success(),
        "child exited with status {status:?}; expected non-zero"
    );
    assert_eq!(
        status.code(),
        Some(7),
        "exit code 7 must surface verbatim, got {:?}",
        status.code()
    );

    let text = String::from_utf8_lossy(&stderr);
    assert!(
        text.contains("fatal: cannot connect"),
        "stderr capture must include the child's diagnostic message, got: {text:?}"
    );
}

/// Same scenario via `SshConnection::split()` + `SshChildHandle`. The
/// pipeline used by `ssh_transfer.rs` and `remote_to_remote.rs` reaches
/// the child handle, so the exit-status + stderr contract must hold on
/// that path as well.
#[test]
fn child_non_zero_exit_and_stderr_surface_through_child_handle() {
    let mut command = SshCommand::new("ignored");
    command.set_program("/bin/sh");
    command.set_batch_mode(false);
    command.push_option("-c");
    command.push_option("echo \"fatal: cannot connect\" >&2; exit 7");
    command.set_target_override(Some(OsString::new()));

    let connection = command.spawn().expect("spawn /bin/sh");
    let (_reader, _writer, child_handle) = connection.split().expect("split connection");

    let (status, stderr) = child_handle
        .wait_with_stderr()
        .expect("child handle wait_with_stderr returns the child status");

    assert!(
        !status.success(),
        "child exited with status {status:?}; expected non-zero"
    );
    assert_eq!(
        status.code(),
        Some(7),
        "exit code 7 must surface through the split child handle, got {:?}",
        status.code()
    );

    let text = String::from_utf8_lossy(&stderr);
    assert!(
        text.contains("fatal: cannot connect"),
        "stderr capture must include the child's diagnostic message after split, got: {text:?}"
    );
}
