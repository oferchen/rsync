//! A local `--write-batch` stamps the protocol the run speaks into the batch
//! header, so `--protocol` can record a batch an older reader accepts.
//!
//! upstream: io.c:2559 `write_int(batch_fd, protocol_version)`. Upstream
//! 3.4.x and 3.5.0 refuse a newer header outright ("The protocol version in
//! the batch file is too new", compat.c:612), so a protocol-33 build that
//! ignored `--protocol` here could never write a batch for them.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

use protocol::ProtocolVersion;

fn oc_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync"))
}

/// Writes a batch for a one-file local copy with `flags` and returns the
/// protocol recorded in its header (the i32 after the stream-flags bitmap).
fn recorded_protocol(batch_flag: &str, flags: &[&str]) -> i32 {
    let temp = tempfile::tempdir().expect("tempdir");
    let src = temp.path().join("src");
    fs::create_dir(&src).expect("create src");
    fs::write(src.join("f"), b"payload\n").expect("write source");
    let batch = temp.path().join("batch");

    let out = Command::new(oc_binary())
        .arg("-r")
        .args(flags)
        .arg(format!("{batch_flag}={}", batch.display()))
        .arg(format!("{}/", src.display()))
        .arg(temp.path().join("dst"))
        .output()
        .expect("run oc-rsync");
    assert!(
        out.status.success(),
        "{batch_flag} {flags:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let bytes = fs::read(&batch).expect("read batch");
    let word = bytes.get(4..8).expect("batch header too short");
    i32::from_le_bytes(word.try_into().expect("4-byte protocol word"))
}

#[test]
fn a_local_batch_records_the_newest_protocol_by_default() {
    let newest = i32::from(ProtocolVersion::NEWEST.as_u8());
    for batch_flag in ["--write-batch", "--only-write-batch"] {
        assert_eq!(recorded_protocol(batch_flag, &[]), newest, "{batch_flag}");
    }
}

#[test]
fn a_local_batch_records_the_protocol_option() {
    for batch_flag in ["--write-batch", "--only-write-batch"] {
        assert_eq!(
            recorded_protocol(batch_flag, &["--protocol=32"]),
            32,
            "{batch_flag} must stamp the --protocol value"
        );
    }
}
