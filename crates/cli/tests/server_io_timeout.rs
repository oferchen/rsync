//! `--timeout` on an rsh `--server` whose client goes silent.
//!
//! upstream: io.c:229-268 check_timeout() - the server polls its stdio with
//! a bound of ceil(timeout/2) seconds, and once nothing has moved for the
//! whole timeout prints `[<role>] io timeout after N seconds -- exiting` and
//! exits RERR_TIMEOUT (30). 3.5.1 run the same way prints
//! `[Receiver] io timeout after 2 seconds -- exiting` and exits 30.
//!
//! The client here sends its protocol version and then nothing, holding its
//! end open, so only the timeout can end the server. The test waits for the
//! exit; no duration is asserted (the exact cadence is pinned by the core's
//! clock-seam tests).
#![cfg(unix)]

use std::io::Write;
use std::process::{Command, Stdio};

use test_support::oc_rsync_bin;

/// Runs `oc-rsync --server [extra] --timeout=1 . <dir>` against a client
/// that goes silent after its version, returning the exit code and stderr.
fn silent_client_run(extra: &[&str]) -> (Option<i32>, String) {
    let tmp = test_support::create_tempdir();
    let mut child = Command::new(oc_rsync_bin())
        .arg("--server")
        .args(extra)
        .arg("--timeout=1")
        .arg("-logDtpre.iLsfxCIvu")
        .arg(".")
        .arg(tmp.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn oc-rsync --server");
    let mut client = child.stdin.take().expect("server stdin");
    client
        .write_all(&31u32.to_le_bytes())
        .expect("send protocol version");
    let status = child.wait().expect("wait for the server");
    drop(client);
    let mut stderr = String::new();
    std::io::Read::read_to_string(&mut child.stderr.take().unwrap(), &mut stderr)
        .expect("read stderr");
    (status.code(), stderr)
}

fn assert_io_timeout(extra: &[&str], who: &str) {
    let (code, stderr) = silent_client_run(extra);
    assert_eq!(code, Some(30), "RERR_TIMEOUT; stderr: {stderr}");
    let line = format!("[{who}] io timeout after ");
    let at = stderr
        .find(&line)
        .unwrap_or_else(|| panic!("no {line:?} in stderr: {stderr}"));
    let rest = &stderr[at + line.len()..];
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    assert!(digits > 0, "no idle seconds in stderr: {stderr}");
    assert!(
        rest[digits..].starts_with(" seconds -- exiting"),
        "upstream's wording; stderr: {stderr}"
    );
}

/// WHY: before the rsh server polled its stdio, `--timeout` never fired on
/// it - a silent client held the server forever.
#[test]
fn a_receiver_server_times_out_a_silent_client() {
    assert_io_timeout(&[], "Receiver");
}

#[test]
fn a_sender_server_times_out_a_silent_client() {
    assert_io_timeout(&["--sender"], "sender");
}
