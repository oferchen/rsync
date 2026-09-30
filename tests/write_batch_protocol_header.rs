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

/// Writes a batch for a local copy of one file holding `content` and returns
/// the batch bytes.
fn write_batch(batch_flag: &str, flags: &[&str], content: &[u8]) -> Vec<u8> {
    let temp = tempfile::tempdir().expect("tempdir");
    let src = temp.path().join("src");
    fs::create_dir(&src).expect("create src");
    fs::write(src.join("f"), content).expect("write source");
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

    fs::read(&batch).expect("read batch")
}

/// Returns the protocol recorded in a one-file batch's header (the i32 after
/// the stream-flags bitmap).
fn recorded_protocol(batch_flag: &str, flags: &[&str]) -> i32 {
    let bytes = write_batch(batch_flag, flags, b"payload\n");
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
    for protocol in 28..=ProtocolVersion::NEWEST.as_u8() {
        let option = format!("--protocol={protocol}");
        for batch_flag in ["--write-batch", "--only-write-batch"] {
            assert_eq!(
                recorded_protocol(batch_flag, &[&option]),
                i32::from(protocol),
                "{batch_flag} {option} must stamp the --protocol value"
            );
        }
    }
}

/// upstream: main.c:933-935 `read_final_goodbye()` - a sender echoes the
/// goodbye `NDX_DONE` only at protocol >= 31, so an older batch ends at the
/// stats trailer. 3.4.4, 3.5.0 and 3.5.1 reject a protocol-30 batch that
/// carries the extra byte ("Invalid packet at end of run", main.c:1130).
#[test]
fn only_a_protocol_31_batch_ends_with_the_goodbye_ndx_done() {
    // 0x1234 bytes: the stats trailer's total_size (main.c:376) is then the
    // varlong30 [0x00, 0x34, 0x12], followed by the build and transfer times
    // (3 bytes each) and, at >= 31 only, the goodbye byte.
    const TOTAL_SIZE: [u8; 3] = [0x00, 0x34, 0x12];
    let content = vec![b'a'; 0x1234];
    for protocol in [30u8, 31, 32] {
        let option = format!("--protocol={protocol}");
        let bytes = write_batch("--write-batch", &[&option], &content);
        let size_at = bytes
            .windows(TOTAL_SIZE.len())
            .rposition(|w| w == TOTAL_SIZE)
            .expect("stats total_size");
        let tail = &bytes[size_at + TOTAL_SIZE.len()..];
        let expected = if protocol >= 31 { 7 } else { 6 };
        assert_eq!(tail.len(), expected, "{option} trailer after total_size");
        if protocol >= 31 {
            assert_eq!(tail.last(), Some(&0x00), "{option} goodbye NDX_DONE");
        }
    }
}
