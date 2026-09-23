//! Behavioural tests for the `capture-rsh` teeing trampoline.
//!
//! These live in an integration test (not the lib's unit tests) because
//! Cargo only provides `CARGO_BIN_EXE_<name>` to integration tests and
//! benches, and resolving the freshly built binary through that variable is
//! what keeps these tests immune to a stale profile-dir copy.

use std::fs;
use std::process::Command;

use test_support::transcript::{TRANSCRIPT_C2S_ENV, TRANSCRIPT_S2C_ENV, TranscriptRecorder};

/// Drive the real trampoline binary with `/bin/cat` standing in for the
/// server: what goes in must come out, and both capture files must hold
/// exactly the forwarded bytes.
#[cfg(unix)]
#[test]
fn trampoline_tees_both_directions_through_cat() {
    use std::io::Write;
    use std::process::Stdio;

    let dir = tempfile::tempdir().expect("tempdir");
    let recorder = TranscriptRecorder::new(dir.path());
    let payload = b"transcript round-trip payload\n";

    let mut child = Command::new(env!("CARGO_BIN_EXE_capture-rsh"))
        .args(["-q", "somehost", "/bin/cat"])
        .env(TRANSCRIPT_C2S_ENV, dir.path().join("client-to-server.bin"))
        .env(TRANSCRIPT_S2C_ENV, dir.path().join("server-to-client.bin"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn capture-rsh");
    child
        .stdin
        .take()
        .expect("piped stdin")
        .write_all(payload)
        .expect("feed payload");
    let output = child.wait_with_output().expect("wait capture-rsh");

    assert!(output.status.success(), "trampoline exit: {output:?}");
    assert_eq!(output.stdout, payload, "cat must echo through the relay");
    let transcript = recorder.finish().expect("captures present");
    assert_eq!(transcript.client_to_server, payload);
    assert_eq!(transcript.server_to_client, payload);
}

/// The trampoline must strip leading options plus the host token and spawn
/// exactly the remaining argv (the fake_rsh.sh contract).
#[cfg(unix)]
#[test]
fn trampoline_drops_options_and_host_before_the_server_argv() {
    use std::process::Stdio;

    let dir = tempfile::tempdir().expect("tempdir");
    let output = Command::new(env!("CARGO_BIN_EXE_capture-rsh"))
        .args(["-l", "-q", "host", "/bin/echo", "server-argv-marker"])
        .env(TRANSCRIPT_C2S_ENV, dir.path().join("c2s.bin"))
        .env(TRANSCRIPT_S2C_ENV, dir.path().join("s2c.bin"))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .output()
        .expect("run capture-rsh");
    assert!(output.status.success(), "trampoline exit: {output:?}");
    assert_eq!(output.stdout, b"server-argv-marker\n");
    // The s2c capture holds echo's output; c2s legitimately captured nothing
    // because stdin was closed at spawn.
    assert_eq!(
        fs::read(dir.path().join("s2c.bin")).expect("s2c capture"),
        b"server-argv-marker\n"
    );
    assert_eq!(
        fs::read(dir.path().join("c2s.bin")).expect("c2s capture"),
        b""
    );
}

/// A trampoline that silently relayed uncaptured would make every downstream
/// comparison read stale files from a prior run, so a missing sink refuses.
#[test]
fn trampoline_refuses_to_run_without_capture_sinks() {
    let output = Command::new(env!("CARGO_BIN_EXE_capture-rsh"))
        .args(["host", "true"])
        .env_remove(TRANSCRIPT_C2S_ENV)
        .env_remove(TRANSCRIPT_S2C_ENV)
        .output()
        .expect("run capture-rsh");
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains(TRANSCRIPT_C2S_ENV),
        "refusal must name the missing variable: {output:?}"
    );
}
