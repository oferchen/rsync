//! ISI.e - wire-byte parity of the INC_RECURSE sender.
//!
//! An rsync 3.5.2 client pulls the same fixture twice through a remote shell
//! that copies everything the sender writes: once from an oc-rsync sender,
//! once from a 3.5.2 sender. Mtimes are pinned, `--checksum-seed` is fixed
//! and `--checksum-choice` is given, so the two outbound streams must be
//! byte-identical; the first differing offset is reported with a hex window
//! around it.
//!
//! Both pulls must also succeed and land the source tree, so a pass shows the
//! remote shell reached a real 3.5.2 peer and captured a complete stream.

#![cfg(all(unix, not(target_os = "macos")))]

mod integration;

use std::fs;
use std::path::Path;

use integration::isi_harness::{peers, pin_mtimes, pull_both, snapshot, write_fixture_file};
use test_support::{UpstreamVersion, require_upstream_rsync};

const SIZES: [usize; 10] = [0, 1, 7, 64, 256, 1024, 4096, 4097, 8192, 12345];
/// `--checksum-choice` keeps the checksum list off the wire (compat.c:545-546
/// sends it only without one), so the stream does not depend on whether the
/// oracle was built with xxhash; CI's builds it with `--disable-xxhash`.
const ARGS: [&str; 3] = ["-rt", "--checksum-seed=12345", "--checksum-choice=md5"];

fn build(src: &Path, leaves: &[&str]) {
    for (dir_idx, leaf) in leaves.iter().enumerate() {
        let dir = src.join(leaf);
        fs::create_dir_all(&dir).expect("create dir");
        for (idx, size) in SIZES.iter().enumerate() {
            let seed = (dir_idx * SIZES.len() + idx) as u32;
            write_fixture_file(&dir.join(format!("file_{idx:02}.bin")), seed, *size);
        }
    }
    pin_mtimes(src);
}

fn hex_window(bytes: &[u8], at: usize) -> String {
    let start = at.saturating_sub(16);
    let end = (at + 16).min(bytes.len());
    bytes[start..end]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn assert_wire_parity(leaves: &[&str]) {
    let Some(peers) = peers(require_upstream_rsync(UpstreamVersion::V3_5_2)) else {
        return;
    };
    let work = tempfile::tempdir().expect("work dir");
    let src = work.path().join("src");
    build(&src, leaves);
    let source = snapshot(&src);
    let ((oc, oc_tree), (up, up_tree)) = pull_both(&peers, work.path(), &src, &ARGS);
    assert_eq!(up.code, Some(0), "3.5.2 sender: {}", up.stderr);
    assert_eq!(oc.code, Some(0), "oc sender: {}", oc.stderr);
    assert_eq!(up_tree, source);
    assert_eq!(oc_tree, source);
    assert!(
        !up.wire.is_empty(),
        "the remote shell captured no 3.5.2 sender bytes"
    );
    if let Some(at) = oc.wire.iter().zip(&up.wire).position(|(a, b)| a != b) {
        panic!(
            "sender streams differ at byte {at} (oc {} bytes, 3.5.2 {} bytes)\noc:    {}\n3.5.2: {}",
            oc.wire.len(),
            up.wire.len(),
            hex_window(&oc.wire, at),
            hex_window(&up.wire, at),
        );
    }
    assert_eq!(oc.wire.len(), up.wire.len(), "sender stream lengths differ");
}

#[test]
fn single_segment_sender_wire_byte_parity_3_5_2() {
    assert_wire_parity(&[""]);
}

#[test]
fn multi_segment_sender_wire_byte_parity_3_5_2() {
    assert_wire_parity(&["a/b/c", "a/b/d", "a/e/f"]);
}
