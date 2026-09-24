//! Byte-neutrality transcript cells for lazy-receiver-consumption (A5a).
//!
//! This is the receiver-side analog of the sender/PUSH gate in
//! `wire_transcript_byte_neutrality.rs` (LF-0b). Where that file captures a
//! PUSH (`oc-rsync SRC/ host:DEST`, client = SENDER), this one captures a
//! PULL (`oc-rsync host:SRC/ DEST`, client = RECEIVER). The `capture-rsh`
//! trampoline is direction-agnostic - it appends every byte the client writes
//! toward the server to the client->server file and every byte the server
//! writes toward the client to the server->client file - so on a pull the
//! client->server stream is the RECEIVER/generator's outbound wire: its
//! per-file NDX, block-signature headers and NDX_DONE phase markers. That is
//! precisely the stream A5a's lazy-receiver-consumption work will re-time, so
//! this harness becomes its byte-identity gate.
//!
//! Cells (mirroring the LF-0b three):
//!
//! 1. Determinism - two PULL runs of the same binary on the same fixture,
//!    under the harness normalization set (fixed `--checksum-seed`, pinned
//!    `RSYNC_CHECKSUM_LIST`, backdated fixture mtimes; see
//!    `test_support::transcript` for each lever's upstream citation), produce
//!    byte-identical transcripts in BOTH directions.
//! 2. Sensitivity (non-vacuity) - a one-byte fixture content change moves the
//!    transcript. On a pull the changed bytes are the sender's literal data,
//!    which crosses server->client, so this cell asserts on that direction;
//!    an independently rebuilt un-mutated fixture then restores byte-identity
//!    in both directions (the revert arm), proving the difference was exactly
//!    the planted byte and not fixture-construction noise. Without the revert
//!    arm the gate would be vacuous - it could not be shown to actually see a
//!    change rather than to always disagree.
//! 3. Receiver-stream substance (the A5a gate) - a multi-level directory
//!    fixture makes the receiver walk several entries, so the client->server
//!    stream carries a per-file NDX/signature/NDX_DONE sequence rather than a
//!    bare handshake. The cell pins that this stream is non-trivial AND that
//!    it GROWS with the entry count (a deep multi-file tree's receiver-outbound
//!    stream is strictly larger than a single-file tree's). That growth is the
//!    non-vacuity proof: the bytes A5a will change are demonstrably present and
//!    counted here, so a future change to the receiver's NDX_DONE timing moves
//!    this stream and fails the gate rather than slipping past a fixed
//!    handshake.
//!
//! The captures are the raw byte streams of both directions, multiplex
//! framing included (upstream: io.c:1155 keeps the 4-byte frame headers inside
//! the same stream), taken by the `capture-rsh` trampoline.
#![cfg(unix)]
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use filetime::FileTime;
use test_support::WireTranscript;
use test_support::transcript::TranscriptRecorder;
/// Fixed mtime for every fixture entry (2023-11-14T22:13:20Z).
///
/// The flist carries each entry's mtime, and rsync's quick-check compares
/// size+mtime, so an un-pinned mtime moves wire bytes between fixture builds
/// and can silently skip transfers built within the same second.
const FIXED_MTIME_SECS: i64 = 1_700_000_000;
/// Deterministic, NON-PERIODIC filler (seeded xorshift).
///
/// Periodic filler creates duplicate blocks, and with duplicates the rolling
/// hash probe order decides which basis block a window resolves to - a choice
/// the protocol does not make canonical, so two correct runs could emit
/// different (equally valid) token streams.
fn filler(seed: u64, len: usize) -> Vec<u8> {
    let mut state = seed | 1;
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state >> 24) as u8
        })
        .collect()
}
/// Build the multi-level fixture: a small tree (files + subdir + symlink) plus
/// a deeper chain that exercises recursion while staying CI-fast, every entry
/// backdated to [`FIXED_MTIME_SECS`]. The receiver walks every entry, so the
/// captured client->server stream carries one NDX/signature/NDX_DONE unit per
/// file.
///
/// `mutation` XORs byte 0 of `a.bin` - the sensitivity cell's one-byte delta.
/// Length and mtime are unchanged, so the CONTENT byte is the only fixture
/// difference between the two arms.
fn build_fixture(root: &Path, mutation: bool) {
    let sub = root.join("sub");
    let mut deep = root.join("deep");
    fs::create_dir_all(&sub).expect("mkdir sub");
    let mut a = filler(0x5eed_0001, 4096);
    if mutation {
        a[0] ^= 0xff;
    }
    fs::write(root.join("a.bin"), &a).expect("write a.bin");
    fs::write(sub.join("b.bin"), filler(0x5eed_0002, 2048)).expect("write b.bin");
    std::os::unix::fs::symlink("a.bin", root.join("ln")).expect("symlink ln");
    for level in 0..4u64 {
        fs::create_dir_all(&deep).expect("mkdir deep level");
        fs::write(
            deep.join(format!("f{level}.bin")),
            filler(0x5eed_0100 + level, 512 + level as usize * 64),
        )
        .expect("write deep file");
        deep = deep.join(format!("d{level}"));
    }
    backdate_tree(root);
}
/// Build a single-file fixture - one backdated regular file, no subdirs. Its
/// receiver-outbound stream is the floor the multi-level tree's must exceed in
/// cell 3.
fn build_single_file_fixture(root: &Path) {
    fs::write(root.join("only.bin"), filler(0x5eed_0007, 4096)).expect("write only.bin");
    backdate_tree(root);
}
/// Pin every entry's mtime, deepest-first so directory mtimes survive their
/// children's writes.
fn backdate_tree(root: &Path) {
    let stamp = FileTime::from_unix_time(FIXED_MTIME_SECS, 0);
    let mut stack = vec![root.to_path_buf()];
    let mut dirs: Vec<PathBuf> = Vec::new();
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).expect("read_dir fixture") {
            let path = entry.expect("dir entry").path();
            let meta = fs::symlink_metadata(&path).expect("lstat fixture entry");
            if meta.file_type().is_symlink() {
                filetime::set_symlink_file_times(&path, stamp, stamp).expect("backdate symlink");
            } else if meta.is_dir() {
                stack.push(path);
            } else {
                filetime::set_file_mtime(&path, stamp).expect("backdate file");
            }
        }
        dirs.push(dir);
    }
    for dir in dirs.iter().rev() {
        filetime::set_file_mtime(dir, stamp).expect("backdate dir");
    }
}
/// Run one PULL transfer of `src` into a fresh destination and return the full
/// wire transcript. The client is the RECEIVER (`host:SRC/ DEST`), so the
/// captured client->server stream is the receiver/generator's outbound wire.
/// `configure` runs last, so a cell can override any env var or append flags
/// on top of the harness normalization.
fn capture(src: &Path, label: &str, configure: impl FnOnce(&mut Command)) -> WireTranscript {
    let workdir = tempfile::tempdir().expect("capture workdir");
    let dest = workdir.path().join("dest");
    fs::create_dir(&dest).expect("mkdir dest");
    let recorder = TranscriptRecorder::new(workdir.path());
    let binary = test_support::oc_rsync_bin();
    // `capture-rsh` is a bin target of this crate, so Cargo builds it before
    // this integration test and hands us its path via CARGO_BIN_EXE_ - no
    // freshness guard or CI build step needed (unlike oc-rsync, which lives in
    // the `bin` crate and is resolved+built separately).
    let capture_rsh = Path::new(env!("CARGO_BIN_EXE_capture-rsh"));
    let mut cmd = recorder.command(&binary, &binary, capture_rsh);
    // -rlt: recursion, symlinks and mtimes - enough surface for the receiver
    // to walk a multi-level tree without dragging in uid/gid variance across
    // hosts. The remote operand pins the client as the RECEIVER; `transcripthost`
    // is the host token capture-rsh drops before exec'ing the server as sender.
    cmd.arg("-rlt")
        .arg(format!("transcripthost:{}/", src.display()))
        .arg(&dest);
    configure(&mut cmd);
    let output = cmd.output().expect("run oc-rsync client");
    assert!(
        output.status.success(),
        "transfer failed ({label}): {}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    let transcript = recorder.finish().expect("read captures");
    // Print the sizes so a reviewer can see real bytes were captured; the
    // zero-byte case is already a loud TranscriptError in finish().
    println!(
        "transcript[{label}]: client->server {} bytes (receiver outbound), \
         server->client {} bytes (sender flist+data)",
        transcript.client_to_server.len(),
        transcript.server_to_client.len()
    );
    transcript
}
fn assert_transcripts_eq(left: &WireTranscript, right: &WireTranscript, what: &str) {
    assert!(
        left.client_to_server == right.client_to_server,
        "{what}: client->server streams differ ({} vs {} bytes)",
        left.client_to_server.len(),
        right.client_to_server.len()
    );
    assert!(
        left.server_to_client == right.server_to_client,
        "{what}: server->client streams differ ({} vs {} bytes)",
        left.server_to_client.len(),
        right.server_to_client.len()
    );
}
/// Cell 1: the instrument is stable - same binary, same fixture, twice.
#[test]
fn pull_transcript_is_deterministic_for_same_binary_and_fixture() {
    let src = tempfile::tempdir().expect("src");
    build_fixture(src.path(), false);
    let first = capture(src.path(), "determinism run 1", |_| {});
    let second = capture(src.path(), "determinism run 2", |_| {});
    assert_transcripts_eq(&first, &second, "same binary, same fixture");
}
/// Cell 2: the instrument can SEE change - and only the planted change.
#[test]
fn pull_transcript_moves_on_a_one_byte_fixture_change_and_reverts() {
    let base_src = tempfile::tempdir().expect("base src");
    build_fixture(base_src.path(), false);
    let base = capture(base_src.path(), "sensitivity base", |_| {});
    let mutated_src = tempfile::tempdir().expect("mutated src");
    build_fixture(mutated_src.path(), true);
    let mutated = capture(mutated_src.path(), "sensitivity mutated", |_| {});
    // On a pull the changed content is the SENDER's literal data, which crosses
    // server->client. A comparison blind to it would be a vacuous gate for
    // every later transfer change.
    assert!(
        base.server_to_client != mutated.server_to_client,
        "a one-byte content change must move the server->client stream on a pull"
    );
    // Revert arm: an independently built fixture WITHOUT the mutation must
    // restore byte-identity in BOTH directions, proving the difference above
    // was exactly the planted byte and not fixture-construction noise.
    let reverted_src = tempfile::tempdir().expect("reverted src");
    build_fixture(reverted_src.path(), false);
    let reverted = capture(reverted_src.path(), "sensitivity reverted", |_| {});
    assert_transcripts_eq(&base, &reverted, "reverted fixture");
}
/// Cell 3 (the A5a gate): the receiver's outbound stream is substantive and
/// scales with the entry count.
///
/// The client->server stream on a pull is the receiver/generator's outbound
/// wire - the per-file NDX, block-signature headers and NDX_DONE phase markers
/// that A5a's lazy-receiver-consumption work will re-time. This cell proves
/// those bytes are actually captured here, two ways: the multi-level tree's
/// receiver-outbound stream is non-trivial (well past a bare handshake), and it
/// is strictly LARGER than a single-file tree's, because each extra entry the
/// receiver walks adds its own NDX/signature/NDX_DONE unit to the stream. A gate
/// that only ever saw a fixed-size handshake could not observe an NDX_DONE
/// timing change; this growth check shows it can.
#[test]
fn pull_receiver_outbound_stream_is_substantive_and_scales_with_entries() {
    let single_src = tempfile::tempdir().expect("single-file src");
    build_single_file_fixture(single_src.path());
    let single = capture(single_src.path(), "single-file tree", |_| {});

    let multi_src = tempfile::tempdir().expect("multi-level src");
    build_fixture(multi_src.path(), false);
    let multi = capture(multi_src.path(), "multi-level tree", |_| {});

    // Non-triviality floor: even a single file forces a real handshake plus one
    // NDX/signature/NDX_DONE unit (64 bytes, measured and deterministic). 32 is
    // comfortably below that yet far above zero, so it fails loudly if the
    // receiver side ever collapses to nothing while still (wrongly) reporting a
    // success.
    assert!(
        single.client_to_server.len() >= 32,
        "single-file receiver-outbound stream is implausibly small ({} bytes) - \
         the pull may not be capturing the receiver's NDX stream",
        single.client_to_server.len()
    );
    // Growth is the load-bearing proof that a PER-FILE receiver stream is
    // present in client->server: each extra entry the receiver walks adds its
    // own NDX/signature/NDX_DONE unit, so the multi-level tree's stream must be
    // strictly - and substantially - larger. The `+ 32` margin (measured delta
    // is ~95 bytes over six extra entries) rules out a single incidental byte:
    // it demands several per-file units, so the bytes A5a re-times are
    // demonstrably counted here and equal sizes could never pass vacuously.
    assert!(
        multi.client_to_server.len() >= single.client_to_server.len() + 32,
        "multi-level receiver-outbound stream ({} bytes) must exceed the \
         single-file one ({} bytes) by a per-file margin; a per-file NDX/NDX_DONE \
         stream grows with the entry count",
        multi.client_to_server.len(),
        single.client_to_server.len()
    );
    println!(
        "receiver-outbound client->server: single-file {} bytes < multi-level {} bytes",
        single.client_to_server.len(),
        multi.client_to_server.len()
    );
}
