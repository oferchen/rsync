//! A remote shell whose command exits before the protocol handshake ends the
//! stream with no bytes. Upstream reports that on two lines: the end-of-stream
//! whine, then the exit trailer naming the winning code.
//!
//! upstream: io.c:298-303 whine_about_eof() - `rprintf(FERROR, RSYNC_NAME ":
//! connection unexpectedly closed (%s bytes received so far) [%s]\n", ...)`
//! then `exit_cleanup(RERR_STREAMIO)`, whose log_exit() line (log.c:937-963)
//! names the worse of RERR_STREAMIO and the remote shell's exit status
//! (cleanup.c:150-152).
#![cfg(unix)]
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

/// A remote shell that drops the host and runs the joined remote command
/// through `sh -c`, as ssh does.
fn write_rsh_shim(dir: &Path) -> PathBuf {
    let script = dir.join("rsh.sh");
    fs::write(&script, "#!/bin/sh\nshift\nexec sh -c \"$*\"\n").expect("write rsh shim");
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).expect("chmod shim");
    script
}

/// Runs a transfer against a remote command that reads the client greeting,
/// runs `then` and exits without speaking the protocol. Reading the greeting
/// first means the client always meets EOF on a read, never a failed write.
/// Returns (exit code, stderr lines).
fn run_against_silent_peer(then: &str, push: bool) -> (Option<i32>, Vec<String>) {
    let temp = tempfile::tempdir().expect("tempdir");
    let src = temp.path().join("src");
    fs::create_dir_all(&src).expect("create src");
    fs::write(src.join("f"), b"x").expect("write file");
    let rsh = write_rsh_shim(temp.path());
    let local = format!("{}/", src.display());
    let remote = format!("peer:{}/", temp.path().join("dst").display());
    let (from, to) = if push {
        (local.as_str(), remote.as_str())
    } else {
        (remote.as_str(), local.as_str())
    };
    let output = Command::new(env!("CARGO_BIN_EXE_oc-rsync"))
        .arg("-a")
        .arg("-e")
        .arg(&rsh)
        .arg(format!("--rsync-path=head -c 4 >/dev/null; {then}"))
        .args([from, to])
        .output()
        .expect("run oc-rsync");
    let stderr = String::from_utf8_lossy(&output.stderr);
    let lines = stderr
        .lines()
        .filter(|line| !line.starts_with("sh: "))
        .map(str::to_owned)
        .collect();
    (output.status.code(), lines)
}

/// The whine is its own line and the trailer carries only the exit code's
/// name; merging them hides the whine from tools that grep for it.
#[test]
fn push_to_a_peer_that_exits_prints_the_whine_then_the_trailer() {
    let (code, lines) = run_against_silent_peer("true", true);
    assert_eq!(code, Some(12), "{lines:#?}");
    assert_eq!(lines.len(), 2, "{lines:#?}");
    assert_eq!(
        lines[0], "oc-rsync: connection unexpectedly closed (0 bytes received so far) [sender]",
        "{lines:#?}"
    );
    assert!(
        lines[1].starts_with("oc-rsync error: error in rsync protocol data stream (code 12) at ")
            && lines[1].contains(" [sender="),
        "{lines:#?}"
    );
}

#[test]
fn pull_from_a_peer_that_exits_prints_the_whine_then_the_trailer() {
    let (code, lines) = run_against_silent_peer("true", false);
    assert_eq!(code, Some(12), "{lines:#?}");
    assert_eq!(lines.len(), 2, "{lines:#?}");
    assert!(
        lines[0]
            .starts_with("oc-rsync: connection unexpectedly closed (0 bytes received so far) ["),
        "{lines:#?}"
    );
    assert!(
        lines[1].starts_with("oc-rsync error: error in rsync protocol data stream (code 12) at "),
        "{lines:#?}"
    );
}

/// A remote shell status worse than RERR_STREAMIO wins the exit code, but the
/// whine is still printed first: upstream whines before exit_cleanup() picks
/// the code.
#[test]
fn a_worse_remote_shell_status_keeps_the_whine_and_names_its_own_code() {
    let (code, lines) = run_against_silent_peer("oc-rsync-no-such-command", true);
    assert_eq!(code, Some(127), "{lines:#?}");
    assert_eq!(lines.len(), 2, "{lines:#?}");
    assert_eq!(
        lines[0], "oc-rsync: connection unexpectedly closed (0 bytes received so far) [sender]",
        "{lines:#?}"
    );
    assert!(
        lines[1].starts_with("oc-rsync error: remote command not found (code 127) at ")
            && lines[1].contains(" [sender="),
        "{lines:#?}"
    );
}
