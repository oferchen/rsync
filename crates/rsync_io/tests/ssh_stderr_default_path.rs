//! Regression guard for the default SSH stderr surfacing path.
//!
//! The `ssh-socketpair-stderr` Cargo feature is opt-in (see
//! `crates/rsync_io/Cargo.toml` and
//! `docs/design/socketpair-stderr-channel.md`). When the feature is OFF,
//! the production default, building an [`SshCommand`], spawning a child
//! that writes to stderr, and consuming the result via the public
//! `SshConnection::wait_with_stderr` API must still surface the child's
//! stderr to the caller. On Unix the underlying transport may be either
//! a socketpair (preferred) or an anonymous pipe (fallback); both paths
//! must honour the same `wait_with_stderr` contract.
//!
//! This test only runs on Unix targets because it relies on
//! `/bin/sh -c 'echo hi >&2; exit 0'` to produce deterministic stderr
//! output without needing a real SSH server.

#![cfg(unix)]
#![cfg(not(feature = "ssh-socketpair-stderr"))]

use rsync_io::SshCommand;
use std::ffi::OsString;

/// Spawns `/bin/sh -c 'echo hi >&2; exit 0'` through the [`SshCommand`]
/// builder (with `sh` substituted for the SSH program) and asserts that
/// the default stderr drain surfaces the `hi` payload via
/// `SshConnection::wait_with_stderr`.
///
/// The substitution exercises the same `Command::spawn` -> drain ->
/// `wait_with_stderr` plumbing the real SSH path uses, without requiring
/// an external SSH daemon. If a future change accidentally moves the
/// stderr drain behind the opt-in `ssh-socketpair-stderr` feature, this
/// test fails because the default build would no longer surface stderr.
#[test]
fn default_pipe_path_surfaces_child_stderr() {
    // An empty host suppresses the SSH target operand so the spawned
    // argv is exactly `/bin/sh -c 'echo hi >&2; exit 0'`. Setting the
    // program to `/bin/sh` also disables every SSH-only flag
    // (`-oBatchMode=yes`, keepalive, ConnectTimeout, etc.).
    let mut builder = SshCommand::new("");
    builder
        .set_program("/bin/sh")
        .set_remote_command([OsString::from("-c"), OsString::from("echo hi >&2; exit 0")]);

    let connection = builder.spawn().expect("spawn /bin/sh child");
    let (status, stderr) = connection
        .wait_with_stderr()
        .expect("collect stderr from child");

    assert!(
        status.success(),
        "child exited unsuccessfully: status={status:?}, stderr={:?}",
        String::from_utf8_lossy(&stderr),
    );
    let text = String::from_utf8_lossy(&stderr);
    assert!(
        text.contains("hi"),
        "default stderr path did not surface payload: {text:?}",
    );
}
