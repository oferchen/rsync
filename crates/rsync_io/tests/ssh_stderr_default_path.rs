//! Regression guards for the default SSH stderr surfacing path.
//!
//! The `ssh-socketpair-stderr` Cargo feature is on by default (see
//! `crates/rsync_io/Cargo.toml` and
//! `docs/design/socketpair-stderr-channel.md`). The production
//! `SshCommand::spawn` path does not depend on it: building an
//! [`SshCommand`], spawning a child that writes to stderr, and consuming
//! the result via the public `SshConnection::wait_with_stderr` API must
//! surface the child's stderr with the feature on and under
//! `--no-default-features`. On Unix the underlying transport may be
//! either a socketpair (preferred) or an anonymous pipe (fallback); both
//! paths must honour the same `wait_with_stderr` contract.

use std::path::Path;

/// Pins the default feature set: `ssh-socketpair-stderr` ships in default
/// builds. Reads the manifest rather than `cfg!` so the assertion holds
/// whatever features the test binary itself was built with.
#[test]
fn ssh_socketpair_stderr_is_a_default_feature() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
    let text = std::fs::read_to_string(&manifest).expect("read rsync_io Cargo.toml");
    let default = text
        .lines()
        .find_map(|line| line.trim().strip_prefix("default = "))
        .expect("rsync_io declares a default feature list");
    assert!(
        default.contains("\"ssh-socketpair-stderr\""),
        "ssh-socketpair-stderr must be a default feature, found {default}",
    );
}

/// Spawns `/bin/sh -c 'echo hi >&2; exit 0'` through the [`SshCommand`]
/// builder (with `sh` substituted for the SSH program) and asserts that
/// the default stderr drain surfaces the `hi` payload via
/// `SshConnection::wait_with_stderr`.
///
/// The substitution exercises the same `Command::spawn` -> drain ->
/// `wait_with_stderr` plumbing the real SSH path uses, without requiring
/// an external SSH daemon. It runs with and without the feature, so a
/// change that made stderr surfacing depend on the feature fails here
/// under `--no-default-features`.
#[cfg(unix)]
#[test]
fn spawn_path_surfaces_child_stderr() {
    use rsync_io::SshCommand;
    use std::ffi::OsString;

    // An empty host suppresses the SSH target operand so the spawned
    // argv is exactly `/bin/sh -c 'echo hi >&2; exit 0'`. Setting the
    // program to `/bin/sh` also disables every SSH-only flag
    // (keepalive, ConnectTimeout, etc.).
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
