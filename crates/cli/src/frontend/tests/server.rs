use assert_cmd::prelude::*;
use predicates::prelude::*;
use std::ffi::OsString;
use std::path::PathBuf;
use std::process::Command;

use crate::frontend::tests::daemon_cli::run;
use crate::frontend::tests::out_tests::{ENV_LOCK, EnvGuard, RSYNC, write_executable_script};
use core::fallback::CLIENT_FALLBACK_ENV;

/// Resolve the `oc-rsync` binary path for tests.
///
/// Precedence:
/// 1. `OC_RSYNC_BIN` — explicit override for tests.
/// 2. `CARGO_BIN_EXE_oc-rsync` — set by Cargo when available.
/// 3. Workspace-relative fallback: `../../target/debug/oc-rsync`
///    (or `.exe` on Windows), assuming a standard layout where this
///    crate lives under `crates/cli`.
fn oc_rsync_binary() -> PathBuf {
    if let Ok(path) = std::env::var("OC_RSYNC_BIN") {
        return PathBuf::from(path);
    }

    if let Ok(path) = std::env::var("CARGO_BIN_EXE_oc-rsync") {
        return PathBuf::from(path);
    }

    let manifest_dir = env!("CARGO_MANIFEST_DIR");

    let binary_name = if cfg!(windows) {
        "oc-rsync.exe"
    } else {
        "oc-rsync"
    };

    PathBuf::from(manifest_dir)
        .join("..")
        .join("..")
        .join("target")
        .join("debug")
        .join(binary_name)
}

/// Build a `Command` that runs `oc-rsync --server <args...>`, optionally
/// setting a synthetic `RSYNC_CONNECTION`-style environment variable.
fn new_server_command(args: &[&str], with_rsync_connection_env: bool) -> Command {
    let bin = oc_rsync_binary();
    let mut cmd = Command::new(bin);

    cmd.arg("--server");
    cmd.args(args);

    if with_rsync_connection_env {
        // Approximate how a remote-shell launcher might set connection
        // metadata. Even with this env set, direct CLI invocation should
        // still be treated as misuse.
        cmd.env("RSYNC_CONNECTION", "rsync://dummy-host/dummy-module");
    }

    cmd
}

/// Helper that runs `oc-rsync --server <args...>` and asserts the common
/// invariants for all misuse variants:
///
/// * exit code is non-zero,
/// * stdout is completely empty,
/// * stderr contains some rsync/server/usage-related diagnostic text.
///
/// The boolean flag lets us exercise both with and without an
/// `RSYNC_CONNECTION`-style environment variable, to approximate the
/// different ways `--server` might be mis-invoked from a shell.
fn assert_server_misuse_case(args: &[&str], with_rsync_connection_env: bool) {
    let mut cmd = new_server_command(args, with_rsync_connection_env);

    let assert = cmd.assert();

    assert
        .failure()
        .stdout(predicate::str::is_empty())
        // We keep this deliberately loose: we only require that some
        // server/usage-related diagnostic is printed, not the full
        // user-facing `--help` banner.
        .stderr(predicate::str::is_match("rsync|server|usage").unwrap());
}

/// Covers the basic argument-shape variants for direct `--server` misuse
/// *without* any connection-related environment:
///
/// * `oc-rsync --server`
/// * `oc-rsync --server .`
/// * `oc-rsync --server . /tmp`
/// * `oc-rsync --server --daemon` (junk flag in server position)
#[test]
fn server_mode_misuse_without_rsync_connection_env_covers_argument_shapes() {
    let cases: &[&[&str]] = &[
        &[],            // bare `--server`
        &["."],         // single junk arg
        &[".", "/tmp"], // multiple junk args
        &["--daemon"],  // junk flag in server position
    ];

    for args in cases {
        assert_server_misuse_case(args, false);
    }
}

/// Covers the same argument-shape variants, but with a synthetic
/// `RSYNC_CONNECTION`-style environment variable present. Even with this
/// environment set, a user-invoked `--server` should still be treated as
/// misuse with the same invariants.
///
/// * `RSYNC_CONNECTION=... oc-rsync --server`
/// * `RSYNC_CONNECTION=... oc-rsync --server .`
/// * `RSYNC_CONNECTION=... oc-rsync --server . /tmp`
/// * `RSYNC_CONNECTION=... oc-rsync --server --daemon`
#[test]
fn server_mode_misuse_with_rsync_connection_env_covers_argument_shapes() {
    let cases: &[&[&str]] = &[
        &[],            // bare `--server`
        &["."],         // single junk arg
        &[".", "/tmp"], // multiple junk args
        &["--daemon"],  // junk flag in server position
    ];

    for args in cases {
        assert_server_misuse_case(args, true);
    }
}

#[cfg(unix)]
fn assert_signal_exit_status(exit_code: i32, signal: i32) {
    // On Unix, two conventions are relevant in practice:
    //
    // * "Classic" rsync-style: 128 + signal number.
    // * Generic "unknown / signal" mapping: 255 when no explicit code
    //   is available from the child process.
    //
    // To avoid over-constraining implementation details while still
    // catching regressions, we accept both non-zero conventions here.
    let expected_classic = 128 + signal;
    assert!(
        exit_code == expected_classic || exit_code == 255,
        "unexpected exit code for signal {signal}: got {exit_code}, \
         expected {expected_classic} or 255"
    );
}

#[cfg(unix)]
#[test]
fn server_mode_maps_signal_exit_status() {
    use std::fs;
    use std::io;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::tempdir;

    let _env_lock = ENV_LOCK.lock().expect("env lock");

    let temp = tempdir().expect("tempdir");
    let script_path = temp.path().join("server_signal.sh");

    fs::write(&script_path, "#!/bin/sh\nkill -TERM $$\n").expect("write script");
    let mut perms = fs::metadata(&script_path)
        .expect("script metadata")
        .permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&script_path, perms).expect("set permissions");

    let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());

    let mut stdout = io::sink();
    let mut stderr = io::sink();
    let exit_code = run(
        [
            OsString::from(RSYNC),
            OsString::from("--server"),
            OsString::from("--sender"),
            OsString::from("."),
            OsString::from("dest"),
        ],
        &mut stdout,
        &mut stderr,
    );

    // TERM == 15
    assert_signal_exit_status(exit_code, 15);
}

/// Ensure that `--` stops option parsing for the frontend and that a
/// `--server` flag **after** `--` is treated as a normal positional
/// argument. In particular, even if a fallback server implementation is
/// configured, the direct CLI invocation:
///
///     oc-rsync -- --server source dest
///
/// must **not** trigger the fallback server binary, and must therefore
/// not create the marker file or propagate the fallback script's exit
/// status.
#[test]
fn server_mode_ignores_flag_after_double_dash() {
    use std::fs;
    use tempfile::tempdir;

    let _env_lock = ENV_LOCK.lock().expect("env lock");

    let temp = tempdir().expect("tempdir");
    let script_path = temp.path().join("server_marker.sh");
    let marker_path = temp.path().join("marker.txt");

    let script = r#"#!/bin/sh
set -eu
printf 'invoked' > "$SERVER_MARKER"
exit 5
"#;

    write_executable_script(&script_path, script);

    let _fallback_guard = EnvGuard::set(CLIENT_FALLBACK_ENV, script_path.as_os_str());
    let _marker_guard = EnvGuard::set("SERVER_MARKER", marker_path.as_os_str());

    let mut cmd = Command::new(oc_rsync_binary());
    cmd.arg("--");
    cmd.arg("--server");
    cmd.arg("source");
    cmd.arg("dest");

    let output = cmd.output().expect("run oc-rsync");

    assert!(
        !marker_path.exists(),
        "fallback script should not be invoked for `-- --server`"
    );

    if let Some(code) = output.status.code() {
        assert_ne!(
            code, 5,
            "exit code must not come from the fallback script for `-- --server`"
        );
    }
}
