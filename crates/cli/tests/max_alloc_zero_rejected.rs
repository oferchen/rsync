//! `--max-alloc=0` must be refused, on the operator path and on the wire.
//!
//! upstream: `options.c:parse_arguments()` gained
//! `if (size == 0) { snprintf(err_buf, sizeof err_buf, "max-alloc must be
//! greater than zero\n"); goto cleanup; }` (options.c:2069-2072). Before that,
//! zero fell through to `if (!max_alloc) max_alloc = SIZE_MAX;`, which removed
//! the `my_alloc()` ceiling entirely - the defence behind the earlier
//! allocation CVEs. Zero is now rejected outright (CVE-2026-53794).
//!
//! `parse_arguments()` is shared: the client runs it from `main()` and a server
//! runs it over the argv its peer sent, so the same check covers a forwarded
//! value. These cells drive the real binary on both paths, because the
//! in-process unit tests exercise the parser rather than the two entry points.

use std::process::{Command, Stdio};

use tempfile::TempDir;
use test_support::oc_rsync_bin;

/// The exact text upstream writes into `err_buf`.
const REJECT_MSG: &str = "max-alloc must be greater than zero";

#[test]
fn client_rejects_max_alloc_zero() {
    // upstream: testsuite/max-alloc-zero-rejected_test.py asserts the command
    // fails and the message appears; `goto cleanup` leaves `parse_arguments`
    // returning 0, so main.c reports err_buf and exits RERR_SYNTAX (1).
    let tmp = TempDir::new().expect("tempdir");
    let output = Command::new(oc_rsync_bin())
        .arg("--max-alloc=0")
        .arg(tmp.path().join("missing-src"))
        .arg(tmp.path().join("missing-dst"))
        .output()
        .expect("spawn oc-rsync");

    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        output.status.code(),
        Some(1),
        "--max-alloc=0 must exit 1, got: {combined}"
    );
    assert!(
        combined.contains(REJECT_MSG),
        "expected {REJECT_MSG:?}, got: {combined}"
    );
}

#[test]
fn client_accepts_a_non_zero_max_alloc() {
    // Positive control for `client_rejects_max_alloc_zero`: a suite where every
    // --max-alloc value were refused would pass that test vacuously.
    let tmp = TempDir::new().expect("tempdir");
    let src = tmp.path().join("src");
    let dst = tmp.path().join("dst");
    std::fs::create_dir(&src).expect("create src");
    std::fs::write(src.join("f.txt"), b"payload\n").expect("write source file");

    let output = Command::new(oc_rsync_bin())
        .arg("-a")
        .arg("--max-alloc=2G")
        .arg(format!("{}/", src.display()))
        .arg(format!("{}/", dst.display()))
        .output()
        .expect("spawn oc-rsync");

    assert!(
        output.status.success(),
        "valid --max-alloc must succeed, got: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        std::fs::read(dst.join("f.txt")).expect("read destination file"),
        b"payload\n"
    );
}

#[test]
fn server_rejects_a_peer_forwarded_max_alloc_zero() {
    // upstream: testsuite/daemon-max-alloc-zero_test.py - an older or modified
    // client still forwards `--max-alloc=0` on the wire, so the check has to
    // hold on the server's copy of `parse_arguments()` too, not only on the
    // client that typed the option. Rejection happens while decoding argv,
    // before any protocol byte is read, so a closed stdin cannot mask it.
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

    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_ne!(
        output.status.code(),
        Some(0),
        "server must refuse a forwarded --max-alloc=0, got: {combined}"
    );
    assert!(
        combined.contains(REJECT_MSG),
        "expected {REJECT_MSG:?} from the server, got: {combined}"
    );
}
