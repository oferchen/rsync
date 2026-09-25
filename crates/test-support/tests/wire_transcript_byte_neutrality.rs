//! Byte-neutrality transcript cells for the lazy-flist programme (LF-0b).
//!
//! Every later flist-producer change must leave the wire byte-identical with
//! its staging flag off. These cells prove the INSTRUMENT before anyone
//! touches the producer:
//!
//! 1. Determinism - two runs of the same binary on the same fixture produce
//!    identical transcripts, given the harness's normalization set (fixed
//!    `--checksum-seed`, pinned `RSYNC_CHECKSUM_LIST`, backdated fixture
//!    mtimes; see `test_support::transcript` for each lever's upstream
//!    citation).
//! 2. Sensitivity (non-vacuity) - a one-byte fixture change moves the
//!    transcript, and reverting that byte restores byte-identity to the
//!    original capture. The revert arm is the mutation evidence: if the
//!    comparison could not see the flipped byte, the `assert_ne` arm fails.
//! 3. Flag-env - `OC_RSYNC_LAZY_FLIST=off` equals the-variable-unset. The
//!    flag does not exist yet (LF-0c), so both arms are identical today by
//!    construction; the cell runs BOTH arms unconditionally so it becomes
//!    load-bearing the day the flag lands, rather than silently self-skipping
//!    until then.
//!
//! The captures are the raw byte streams of both directions, multiplex
//! framing included (upstream: io.c:1173 keeps the 4-byte frame headers
//! inside the same stream), taken by the `capture-rsh` trampoline.

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

/// Build the fixture: a small tree (files + subdir + symlink) plus a deeper
/// chain that exercises recursion while staying CI-fast, every entry
/// backdated to [`FIXED_MTIME_SECS`].
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

/// Run one push transfer of `src` into a fresh destination and return the
/// full wire transcript. `configure` runs last, so a cell can override any
/// env var or append flags on top of the harness normalization.
fn capture(src: &Path, label: &str, configure: impl FnOnce(&mut Command)) -> WireTranscript {
    let workdir = tempfile::tempdir().expect("capture workdir");
    let dest = workdir.path().join("dest");
    fs::create_dir(&dest).expect("mkdir dest");

    let recorder = TranscriptRecorder::new(workdir.path());
    let binary = test_support::oc_rsync_bin();
    // `capture-rsh` is a bin target of this crate, so Cargo builds it before
    // this integration test and hands us its path via CARGO_BIN_EXE_ - no
    // freshness guard or CI build step needed (unlike oc-rsync, which lives
    // in the `bin` crate and is resolved+built separately).
    let capture_rsh = Path::new(env!("CARGO_BIN_EXE_capture-rsh"));
    let mut cmd = recorder.command(&binary, &binary, capture_rsh);
    // -rlt: recursion, symlinks and mtimes - enough surface for the flist
    // work without dragging in uid/gid variance across hosts.
    cmd.arg("-rlt")
        .arg(format!("{}/", src.display()))
        .arg(format!("transcripthost:{}", dest.display()));
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
        "transcript[{label}]: client->server {} bytes, server->client {} bytes",
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
fn transcript_is_deterministic_for_same_binary_and_fixture() {
    let src = tempfile::tempdir().expect("src");
    build_fixture(src.path(), false);

    let first = capture(src.path(), "determinism run 1", |_| {});
    let second = capture(src.path(), "determinism run 2", |_| {});
    assert_transcripts_eq(&first, &second, "same binary, same fixture");
}

/// Cell 2: the instrument can SEE change - and only the planted change.
#[test]
fn transcript_moves_on_a_one_byte_fixture_change_and_reverts() {
    let base_src = tempfile::tempdir().expect("base src");
    build_fixture(base_src.path(), false);
    let base = capture(base_src.path(), "sensitivity base", |_| {});

    let mutated_src = tempfile::tempdir().expect("mutated src");
    build_fixture(mutated_src.path(), true);
    let mutated = capture(mutated_src.path(), "sensitivity mutated", |_| {});
    // The flipped content byte crosses client->server on a push; a comparison
    // blind to it would be a vacuous gate for every later producer change.
    assert!(
        base.client_to_server != mutated.client_to_server,
        "a one-byte content change must move the client->server stream"
    );

    // Revert arm: an independently built fixture WITHOUT the mutation must
    // restore byte-identity, proving the difference above was exactly the
    // planted byte and not fixture-construction noise.
    let reverted_src = tempfile::tempdir().expect("reverted src");
    build_fixture(reverted_src.path(), false);
    let reverted = capture(reverted_src.path(), "sensitivity reverted", |_| {});
    assert_transcripts_eq(&base, &reverted, "reverted fixture");
}

/// Cell 3: the LF-0c staging-flag gate, wired ahead of the flag.
///
/// `OC_RSYNC_LAZY_FLIST` does not exist in the binary yet, so both arms are
/// identical by construction today. Both arms still RUN - the cell never
/// self-skips - so the day the flag lands, `off` diverging from unset fails
/// here first.
#[test]
fn transcript_unchanged_by_lazy_flist_env_off() {
    let src = tempfile::tempdir().expect("src");
    build_fixture(src.path(), false);

    // The recorder's command() removes the variable; this arm is "unset".
    let unset = capture(src.path(), "lazy-flist unset", |_| {});
    // configure() runs after command(), so this env call wins over the
    // removal and the child really sees OC_RSYNC_LAZY_FLIST=off.
    let off = capture(src.path(), "lazy-flist off", |cmd| {
        cmd.env("OC_RSYNC_LAZY_FLIST", "off");
    });
    assert_transcripts_eq(&unset, &off, "OC_RSYNC_LAZY_FLIST=off vs unset");
}

/// Cell 4 (LF-2c): setting `OC_RSYNC_LAZY_FLIST=1` does not perturb the wire.
///
/// The lazy producer is gated behind BOTH the staging flag AND a negotiated
/// INC_RECURSE (`GeneratorContext::lazy_producer_eligible`). INC_RECURSE is not
/// yet negotiated on a live oc transfer - the wire pull-conversion is a separate
/// pending track - so on this push the flag is inert and both arms take the same
/// eager path. This cell therefore guards that turning the flag on never leaks a
/// change into the currently-reachable path; it does NOT yet exercise the lazy
/// producer end-to-end.
///
/// The producer's actual byte-neutrality - initial segment plus the whole
/// sub-list sequence reproduced identically to the eager partition - is proven
/// non-vacuously at the unit level, where INC_RECURSE and the lazy decision are
/// forced:
/// `transfer::generator::tests::lazy_producer_reproduces_eager_partition_segments`
/// and `lazy_producer_scales_past_lookahead_boundary_and_matches_eager`. Once
/// the wire negotiation lands, this cell becomes the end-to-end gate unchanged.
#[test]
fn transcript_unchanged_by_lazy_flist_env_on() {
    let src = tempfile::tempdir().expect("src");
    build_fixture(src.path(), false);

    // Eager arm: variable unset (command() removes it).
    let unset = capture(src.path(), "lazy-flist unset", |_| {});
    // Flag-on arm: OC_RSYNC_LAZY_FLIST=1.
    let on = capture(src.path(), "lazy-flist on", |cmd| {
        cmd.env("OC_RSYNC_LAZY_FLIST", "1");
    });
    assert_transcripts_eq(&unset, &on, "OC_RSYNC_LAZY_FLIST=1 vs unset");
}
