//! ISI.e - wire parity of the INC_RECURSE sender.
//!
//! An rsync 3.5.2 client pulls the same fixture twice through a remote shell
//! that copies everything the sender writes: once from an oc-rsync sender,
//! once from a 3.5.2 sender. Mtimes are pinned, `--checksum-seed` is fixed
//! and `--checksum-choice` is given, so every record the two senders write
//! must be byte-identical; the first differing offset is reported with a hex
//! window around it.
//!
//! rsync 3.5.2's own stream varies from run to run in its multiplex framing
//! and in four stats values, so `assert_same_records` compares the records
//! in canonical form; see its documentation in the harness.
//!
//! Both pulls must also succeed and land the source tree, so a pass shows the
//! remote shell reached a real 3.5.2 peer and captured a complete stream.
#![cfg(all(unix, not(target_os = "macos")))]
mod integration;
use integration::isi_harness::{
    MPLEX_BASE, assert_same_records, peers, pin_mtimes, pull_both, snapshot, write_fixture_file,
};
use std::fs;
use std::path::Path;
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
    assert_same_records(&oc.wire, &up.wire);
}
#[test]
fn single_segment_sender_wire_byte_parity_3_5_2() {
    assert_wire_parity(&[""]);
}
#[test]
fn multi_segment_sender_wire_byte_parity_3_5_2() {
    assert_wire_parity(&["a/b/c", "a/b/d", "a/e/f"]);
}
fn frame(payload: &[u8]) -> Vec<u8> {
    let mut out = ((MPLEX_BASE << 24) + payload.len() as u32)
        .to_le_bytes()
        .to_vec();
    out.extend_from_slice(payload);
    out
}
/// A stream shaped like the multi-segment fixture's tail as 3.5.2 wrote it:
/// tail NDX_DONEs framed by `groups`, then the stats record and the goodbye
/// NDX_DONE.
fn tail(groups: &[usize], stats: [u8; 15]) -> Vec<u8> {
    let mut out = vec![0x1f, 0, 0, 0];
    out.extend(frame(b"records"));
    for &n in groups {
        out.extend(frame(&vec![0; n]));
    }
    out.extend(frame(&[stats.as_slice(), &[0u8]].concat()));
    out
}
/// Stats from two 3.5.2 runs: total_read, total_written and
/// flist_buildtime differ, total_size does not.
const STATS_A: [u8; 15] = [0, 0x79, 2, 1, 0xb1, 0x67, 1, 0x86, 0x60, 0, 1, 0, 0, 0, 0];
const STATS_B: [u8; 15] = [0, 0x75, 2, 1, 0xad, 0x67, 1, 0x86, 0x60, 0, 2, 0, 0, 0, 0];
#[test]
fn canonical_compare_ignores_framing_and_timing_stats() {
    assert_same_records(&tail(&[1, 2, 1, 2], STATS_A), &tail(&[1, 3, 2], STATS_B));
}
#[test]
#[should_panic(expected = "sender data")]
fn canonical_compare_catches_a_dropped_ndx_done() {
    assert_same_records(&tail(&[1, 2, 1, 1], STATS_A), &tail(&[1, 2, 1, 2], STATS_A));
}
#[test]
#[should_panic(expected = "total_size differs")]
fn canonical_compare_catches_a_different_total_size() {
    let mut stats = STATS_A;
    stats[8] ^= 1;
    assert_same_records(&tail(&[1, 2, 1, 2], stats), &tail(&[1, 2, 1, 2], STATS_A));
}
#[test]
#[should_panic(expected = "sender data differs")]
fn canonical_compare_catches_swapped_payload_bytes() {
    let up = tail(&[1, 2, 1, 2], STATS_A);
    let mut oc = up.clone();
    oc.swap(8, 9);
    assert_same_records(&oc, &up);
}
