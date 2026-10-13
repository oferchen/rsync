//! `--protocol=N --read-batch` must refuse a batch recorded at a newer protocol.
//!
//! upstream: compat.c:611-614 setup_protocol() - under `read_batch` the batch
//! header's protocol becomes `remote_protocol`, and anything above the reader's
//! `protocol_version` (lowered by `--protocol`) aborts with RERR_PROTOCOL. The
//! ceiling is the requested protocol, not the newest one this build speaks:
//! replaying a protocol-32 stream as protocol 31 would decode it with the wrong
//! wire rules.
//!
//! Measured against rsync 3.5.1: a `--protocol=32` batch replayed with
//! `--protocol=31` exits 2 printing
//! `The protocol version in the batch file is too new (32 > 31).`, while the
//! same batch replayed with `--protocol=32` succeeds.
mod integration;
use integration::helpers::{RsyncCommand, TestDir};

/// Records a `--protocol=32` batch of a one-file tree and returns its path.
fn record_protocol_32_batch(test_dir: &TestDir) -> std::path::PathBuf {
    let src = test_dir.mkdir("src").expect("create src");
    test_dir
        .write_file("src/a.txt", b"payload")
        .expect("write source file");
    let batch = test_dir.path().join("BATCH");
    RsyncCommand::new()
        .arg("-a")
        .arg("--protocol=32")
        .arg(format!("--only-write-batch={}", batch.display()))
        .arg(format!("{}/", src.display()))
        .arg(format!("{}/", test_dir.path().join("unused").display()))
        .assert_success();
    // The fixture is only meaningful if the header really says 32: a writer
    // that ignored --protocol would record the build's newest protocol and the
    // refusal below would then be testing a different comparison.
    let bytes = std::fs::read(&batch).expect("read batch");
    let recorded = i32::from_le_bytes(bytes[4..8].try_into().expect("header protocol"));
    assert_eq!(recorded, 32, "batch header must record --protocol=32");
    batch
}

#[test]
fn read_batch_refuses_batch_newer_than_requested_protocol() {
    let test_dir = TestDir::new().expect("create test dir");
    let batch = record_protocol_32_batch(&test_dir);
    let dest = test_dir.mkdir("dest").expect("create dest");

    let output = RsyncCommand::new()
        .arg("-a")
        .arg("--protocol=31")
        .arg(format!("--read-batch={}", batch.display()))
        .arg(format!("{}/", dest.display()))
        .assert_failure();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        output.status.code(),
        Some(2),
        "a too-new batch is RERR_PROTOCOL: {stderr}"
    );
    assert!(
        stderr.contains("The protocol version in the batch file is too new (32 > 31)."),
        "expected upstream's compat.c:612 diagnostic, got: {stderr}"
    );
    assert!(
        !dest.join("a.txt").exists(),
        "a refused batch must not be replayed"
    );
}

/// Control: the same batch at its own protocol replays, so the refusal above
/// is caused by the ceiling and not by a broken fixture.
#[test]
fn read_batch_accepts_batch_at_requested_protocol() {
    let test_dir = TestDir::new().expect("create test dir");
    let batch = record_protocol_32_batch(&test_dir);
    let dest = test_dir.mkdir("dest").expect("create dest");

    RsyncCommand::new()
        .arg("-a")
        .arg("--protocol=32")
        .arg(format!("--read-batch={}", batch.display()))
        .arg(format!("{}/", dest.display()))
        .assert_success();
    assert_eq!(
        std::fs::read(dest.join("a.txt")).expect("replayed file"),
        b"payload"
    );
}
