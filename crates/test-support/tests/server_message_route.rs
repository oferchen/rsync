//! An rsh `--server` writes its error and warning lines to its own stderr, not
//! into the protocol stream, unless the client asked for frames or the
//! protocol is too old for the client to have a stderr set up for it - exactly
//! rsync 3.5.1's routing:
//!
//! ```c
//! /* options.c:103 */      int msgs2stderr = 2;
//! /* compat.c:653-654 */   if (!saw_stderr_opt && protocol_version <= 28 && am_server)
//!                                  msgs2stderr = 0;
//! /* log.c:357 */          if (am_server && msgs2stderr != 1 && (msgs2stderr != 2 || f != stderr)) {
//!                                  ... send_msg(msg, buf, len, !is_utf8) ...
//! ```
//!
//! The remote shell hands the server's stderr to the client's terminal, and
//! the client takes its exit code from the remote-shell child (main.c:1405),
//! so the run still prints the line and ends with 23 either way. What differs
//! is the wire, which this test reads directly: the teeing `capture-rsh`
//! records every byte the server sends, and the demultiplexed message frames
//! show whether the line travelled as a frame. Byte totals are deliberately
//! not compared - flush points legitimately move the `MSG_DATA` framing.
//!
//! Each cell pushes `a` into a 0555 destination, so the receiver's temp create
//! fails with a per-file `FERROR_XFER`. Skip condition (the test passes with a
//! printed reason): running as root, which bypasses the 0555 mode.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;

use test_support::transcript::TranscriptRecorder;

/// upstream: rsync.h:263-267 - `MSG_ERROR_XFER = 1`, `MSG_ERROR = 3`,
/// `MSG_WARNING = 4`.
const ERROR_CODES: [u8; 3] = [1, 3, 4];

struct Outcome {
    exit_code: Option<i32>,
    stderr: String,
    /// The server's error/warning frames, `(code, payload)`.
    error_frames: Vec<(u8, Vec<u8>)>,
}

fn running_as_root() -> bool {
    std::env::var("USER").is_ok_and(|u| u == "root")
}

/// Pushes one file into a sealed destination with `extra` client flags.
fn push(extra: &[&str]) -> Option<Outcome> {
    if running_as_root() {
        eprintln!("skip: root bypasses the 0555 fixture");
        return None;
    }
    let work = test_support::create_tempdir();
    let src = work.path().join("src");
    let dest = work.path().join("dest");
    fs::create_dir(&src).expect("mkdir src");
    fs::create_dir(&dest).expect("mkdir dest");
    fs::write(src.join("a"), b"payload-a\n").expect("seed a");
    fs::set_permissions(&dest, fs::Permissions::from_mode(0o555)).expect("seal dest");

    let binary = test_support::oc_rsync_bin();
    let recorder = TranscriptRecorder::new(work.path());
    let output = recorder
        .command(
            &binary,
            &binary,
            std::path::Path::new(env!("CARGO_BIN_EXE_capture-rsh")),
        )
        // `-t` gives the server a flag string; an empty one is refused by the
        // oc server at protocol 28 (a separate defect), and the routing under
        // test does not depend on it.
        .arg("-t")
        .args(extra)
        .arg(src.join("a"))
        .arg(format!("fakehost:{}/", dest.display()))
        .output()
        .expect("run push");
    fs::set_permissions(&dest, fs::Permissions::from_mode(0o755)).expect("unseal dest");

    let transcript = recorder.finish().expect("read captures");
    let error_frames = transcript
        .server_messages()
        .expect("demultiplex the server stream")
        .into_iter()
        .filter(|(code, _)| ERROR_CODES.contains(code))
        .collect();
    Some(Outcome {
        exit_code: output.status.code(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        error_frames,
    })
}

fn assert_line_and_exit(cell: &str, outcome: &Outcome) {
    assert!(
        outcome.stderr.contains("mkstemp") && outcome.stderr.contains("Permission denied (13)"),
        "{cell}: the failure line reaches the client's stderr either way:\n{}",
        outcome.stderr
    );
    assert_eq!(
        outcome.exit_code,
        Some(23),
        "{cell}: the server's transfer error still ends the run with RERR_PARTIAL"
    );
}

/// The default: the line goes to the server's stderr and never onto the wire.
#[test]
fn a_server_error_line_goes_to_stderr_not_the_wire_by_default() {
    let Some(outcome) = push(&[]) else { return };
    assert_line_and_exit("default", &outcome);
    assert!(
        outcome.error_frames.is_empty(),
        "default: no MSG_ERROR/MSG_ERROR_XFER/MSG_WARNING frame may be sent, got {:?}",
        outcome.error_frames
    );
}

/// `--no-msgs2stderr` (msgs2stderr = 0): every line is framed.
#[test]
fn no_msgs2stderr_frames_the_error_line() {
    let Some(outcome) = push(&["--no-msgs2stderr"]) else {
        return;
    };
    assert_line_and_exit("--no-msgs2stderr", &outcome);
    assert_framed("--no-msgs2stderr", &outcome);
}

/// Protocol 28 without an explicit option: framed, because that client may
/// not have a stderr set up for the server (compat.c:653-654).
#[test]
fn protocol_28_frames_the_error_line() {
    let Some(outcome) = push(&["--protocol=28"]) else {
        return;
    };
    assert_line_and_exit("--protocol=28", &outcome);
    assert_framed("--protocol=28", &outcome);
}

fn assert_framed(cell: &str, outcome: &Outcome) {
    assert!(
        outcome
            .error_frames
            .iter()
            .any(|(code, payload)| *code == 1
                && String::from_utf8_lossy(payload).contains("mkstemp")),
        "{cell}: the mkstemp line must travel as an MSG_ERROR_XFER frame, got {:?}",
        outcome.error_frames
    );
}
