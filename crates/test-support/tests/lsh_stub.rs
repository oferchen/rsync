//! Integration test for the `lsh-stub` remote-shell helper.
//!
//! Unit tests in `src/lsh.rs` cannot exercise the compiled stub because
//! `CARGO_BIN_EXE_lsh-stub` is only injected for integration-test targets.
//! This test resolves the built binary through [`LshRunnerStub`] and drives it
//! exactly as oc-rsync would use `--rsh`: spawn it with a `localhost` host and
//! a remote command, and confirm the command ran locally.

#![cfg(unix)]

use std::process::Command;

use test_support::LshRunnerStub;

#[test]
fn stub_runs_remote_command_locally_via_localhost() {
    // Why: the whole point of the stub is to make oc-rsync's remote-shell path
    // execute the "remote" argv on the local host. If the host token were
    // mishandled or the argv dropped, no output would appear - so a matching
    // stdout is proof the remote-shell seam works.
    let stub = LshRunnerStub::locate().expect("lsh-stub must be built for integration tests");

    let out = Command::new(stub.path())
        .args(["localhost", "printf", "%s", "remote-ran"])
        .output()
        .expect("spawn lsh-stub");

    assert!(
        out.status.success(),
        "stub exited non-zero: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "remote-ran");
}

#[test]
fn stub_rejects_unknown_host_loudly() {
    // Why: upstream lsh.sh refuses any host other than localhost/lh with a
    // non-zero exit. Mirroring that keeps a misconfigured test from silently
    // "succeeding" against a host the stub cannot honour.
    let stub = LshRunnerStub::locate().expect("lsh-stub must be built for integration tests");

    let out = Command::new(stub.path())
        .args(["some-remote-host", "echo", "x"])
        .output()
        .expect("spawn lsh-stub");

    assert!(!out.status.success(), "unknown host must fail");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("unable to connect"),
        "expected a connect-refusal message on stderr"
    );
}

#[test]
fn stub_strips_bare_ssh_style_flags_before_the_host() {
    // Why: oc-rsync may pass bundled ssh-style flags (e.g. -oBatchMode=yes)
    // ahead of the host. The stub must drop such bare `-*` tokens the way
    // upstream lsh.sh does and still reach the host token. (Flags that take a
    // separate value are not honoured by lsh.sh either: its `-*) shift` drops
    // only the flag, so a lone value would be misread as the host - we mirror
    // that exactly rather than diverge.)
    let stub = LshRunnerStub::locate().expect("lsh-stub must be built for integration tests");

    let out = Command::new(stub.path())
        .args(["-oBatchMode=yes", "-q", "localhost", "printf", "ok"])
        .output()
        .expect("spawn lsh-stub");

    assert!(
        out.status.success(),
        "stub exited non-zero: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "ok");
}
