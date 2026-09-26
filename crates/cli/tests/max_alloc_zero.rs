//! `--max-alloc=0` means "the largest limit this build supports".
//!
//! upstream: rsync 3.5.1 `options.c:2072-2086` - `parse_size_arg(..,
//! unlimited_0 = True)` accepts 0, and `if (!max_alloc) max_alloc =
//! SIZE_ARG_MAX;` resolves it to SIZE_MAX/2: bounded, not unlimited.
//! `server_options()` then forwards `max_alloc_arg` - the operator's spelling,
//! not the resolved number - whenever the limit differs from the default
//! (options.c:3039-3040). That is what makes 0 portable: a 64-bit client and a
//! 32-bit peer each resolve it against their own SIZE_MAX, where any number
//! big enough to matter on the client is "too large" for the smaller peer.
//!
//! These cells drive the real binary, because forwarding is only observable
//! in the argv a remote shell is handed.

use std::path::Path;
use std::process::{Command, Output, Stdio};

use tempfile::TempDir;
use test_support::oc_rsync_bin;

fn combined(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn make_source(root: &Path) -> std::path::PathBuf {
    let src = root.join("src");
    std::fs::create_dir(&src).expect("create src");
    std::fs::write(src.join("f.txt"), b"payload\n").expect("write source file");
    src
}

#[test]
fn client_accepts_max_alloc_zero() {
    // upstream: testsuite/max-alloc-zero_test.py - "0 is accepted, and a
    // transfer using it works". 3.5.0 refused it; 3.5.1 restored it with a
    // bounded meaning.
    let tmp = TempDir::new().expect("tempdir");
    let src = make_source(tmp.path());
    let dst = tmp.path().join("dst");
    let output = Command::new(oc_rsync_bin())
        .arg("-r")
        .arg("--max-alloc=0")
        .arg(format!("{}/", src.display()))
        .arg(format!("{}/", dst.display()))
        .output()
        .expect("spawn oc-rsync");

    assert!(
        output.status.success(),
        "--max-alloc=0 must transfer, got: {}",
        combined(&output)
    );
    assert_eq!(
        std::fs::read(dst.join("f.txt")).expect("read destination file"),
        b"payload\n"
    );
}

#[test]
fn client_rejects_a_value_at_the_ceiling() {
    // upstream: testsuite/max-alloc-zero_test.py part 3 - accepting 0 must not
    // bring back an unbounded value: 8192P reaches SIZE_MAX/2 on a 64-bit
    // build, and on a 32-bit one the P multiplier alone already exceeds it.
    let tmp = TempDir::new().expect("tempdir");
    let output = Command::new(oc_rsync_bin())
        .arg("--max-alloc=8192P")
        .arg(tmp.path().join("src"))
        .arg(tmp.path().join("dst"))
        .output()
        .expect("spawn oc-rsync");

    let text = combined(&output);
    assert_eq!(output.status.code(), Some(1), "got: {text}");
    assert!(
        text.contains("--max-alloc=8192P is too large"),
        "expected upstream's too-large text, got: {text}"
    );
}

#[test]
fn server_accepts_a_peer_forwarded_max_alloc_zero() {
    // The server runs the same parse_arguments() block over its peer's argv,
    // so a forwarded 0 resolves there instead of failing the session.
    // Option decoding happens before any protocol byte is read, so the
    // rejection this replaced surfaced with a closed stdin; with the value
    // accepted, the session instead ends on the closed stream.
    let tmp = TempDir::new().expect("tempdir");
    let output = Command::new(oc_rsync_bin())
        .arg("--server")
        .arg("--sender")
        .arg("-e.LsfxCIvu")
        .arg("--max-alloc=0")
        .arg(".")
        .arg(tmp.path())
        .stdin(Stdio::null())
        .output()
        .expect("spawn oc-rsync --server");

    let text = combined(&output);
    assert!(
        !text.contains("max-alloc"),
        "server must accept a forwarded --max-alloc=0, got: {text}"
    );
}

/// Pulls one file over a remote shell that logs the argv it is handed,
/// returning that log.
#[cfg(unix)]
fn forwarded_argv(max_alloc: &str) -> String {
    use std::os::unix::fs::PermissionsExt;

    let tmp = TempDir::new().expect("tempdir");
    let src = make_source(tmp.path());
    let dst = tmp.path().join("dst");
    let log = tmp.path().join("server-argv");
    let rsh = tmp.path().join("log-rsh.sh");
    // Log the command line built for the peer, then run it the way
    // upstream's support/lsh.sh does: drop the host and execute the rest.
    std::fs::write(
        &rsh,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nshift\nexec sh -c \"$*\"\n",
            log.display()
        ),
    )
    .expect("write rsh wrapper");
    std::fs::set_permissions(&rsh, std::fs::Permissions::from_mode(0o755)).expect("chmod");

    let output = Command::new(oc_rsync_bin())
        .arg("-r")
        .arg(format!("--max-alloc={max_alloc}"))
        .arg(format!("--rsh={}", rsh.display()))
        .arg(format!("--rsync-path={}", oc_rsync_bin().display()))
        .arg(format!("localhost:{}/", src.display()))
        .arg(format!("{}/", dst.display()))
        .output()
        .expect("spawn oc-rsync");
    assert!(
        output.status.success(),
        "--max-alloc={max_alloc} pull must succeed, got: {}",
        combined(&output)
    );
    assert_eq!(
        std::fs::read(dst.join("f.txt")).expect("read destination file"),
        b"payload\n"
    );
    std::fs::read_to_string(&log).expect("the wrapper logged a command line")
}

#[cfg(unix)]
#[test]
fn zero_is_forwarded_to_the_peer_unresolved() {
    // upstream: testsuite/max-alloc-zero_test.py part 2 - the peer must be
    // sent the literal "0". A resolved number would be refused as "too large"
    // by a peer with a smaller SIZE_MAX.
    let logged = forwarded_argv("0");
    assert!(
        format!(" {logged} ")
            .replace('\n', " ")
            .contains(" --max-alloc=0 "),
        "--max-alloc=0 was not forwarded verbatim; the peer was sent:\n{logged}"
    );
}

#[cfg(unix)]
#[test]
fn a_value_is_forwarded_in_the_operator_spelling() {
    let logged = forwarded_argv("2G");
    assert!(
        logged.contains("--max-alloc=2G"),
        "the peer must get the value as typed, got:\n{logged}"
    );
}

#[cfg(unix)]
#[test]
fn the_default_value_is_not_forwarded() {
    // upstream: options.c:3039 - `max_alloc != DEFAULT_MAX_ALLOC` gates the
    // forward, so spelling out the 1 GiB default sends nothing.
    let logged = forwarded_argv("1G");
    assert!(
        !logged.contains("--max-alloc"),
        "the default limit must not be forwarded, got:\n{logged}"
    );
}
