//! A local `--no-whole-file` delta whose phase-1 strong sum false-matches a
//! block must fail verification and be resent with full-width sums.
//!
//! upstream: generator.c:745-754 `sum_sizes_sqroot()` truncates the strong
//! sum to 2 bytes for a small basis; receiver.c:534 fails the whole-file
//! check, receiver.c:1350-1375 warns and queues the phase-2 resend, and
//! generator.c:2650 resends with `SUM_LENGTH`.
//!
//! The fixture is the one upstream 3.5.1 reproduces with
//! `-I --no-whole-file --checksum-seed=1 --checksum-choice=md5`: the source's
//! first block differs from the basis's but shares its rolling sum and the
//! first two bytes of its seeded MD5. Driving the real binary also pins that
//! `--checksum-seed` and `--checksum-choice` reach the local delta, since
//! without both the collision does not exist.

use std::process::Command;

use filetime::{FileTime, set_file_mtime};
use test_support::oc_rsync_bin;

/// Raising byte 171 and lowering byte 172, then lowering byte 258 and raising
/// byte 259, keeps both rolling-sum halves and, for this block, the first two
/// bytes of `md5(seed || block)` with seed 1.
const COLLISION: (usize, usize) = (171, 258);

fn fixture() -> (Vec<u8>, Vec<u8>) {
    let block: Vec<u8> = (0..700u32).map(|i| (i * 89 % 241 + 5) as u8).collect();
    let mut window = block.clone();
    let (i, j) = COLLISION;
    window[i] += 1;
    window[i + 1] -= 1;
    window[j] -= 1;
    window[j + 1] += 1;
    let tail: Vec<u8> = (0..1400u32).map(|i| (i * 7 % 253) as u8).collect();
    (
        [block.as_slice(), tail.as_slice()].concat(),
        [window.as_slice(), tail.as_slice()].concat(),
    )
}

fn run(extra: &[&str]) -> (String, bool) {
    let tmp = test_support::create_tempdir();
    let (basis, source) = fixture();
    let src = tmp.path().join("src.bin");
    let dst = tmp.path().join("dst.bin");
    std::fs::write(&src, &source).expect("write source");
    std::fs::write(&dst, &basis).expect("write basis");
    set_file_mtime(&dst, FileTime::from_unix_time(1_600_000_000, 0)).expect("backdate basis");

    let output = Command::new(oc_rsync_bin())
        .args([
            "-I",
            "--no-whole-file",
            "--checksum-seed=1",
            "--checksum-choice=md5",
            "-v",
        ])
        .args(extra)
        .arg(&src)
        .arg(&dst)
        .output()
        .expect("spawn oc-rsync");
    assert!(output.status.success(), "{output:?}");
    let reconstructed = std::fs::read(&dst).expect("read destination") == source;
    (
        String::from_utf8_lossy(&output.stderr).into_owned(),
        reconstructed,
    )
}

#[test]
fn false_match_is_discarded_and_redone() {
    let (stderr, reconstructed) = run(&[]);
    assert!(reconstructed);
    assert!(
        stderr
            .contains("WARNING: src.bin failed verification -- update discarded (will try again)."),
        "got: {stderr}"
    );
}

/// upstream: receiver.c:1352-1358 - an in-place update is "retained".
#[test]
fn in_place_false_match_is_retained_and_redone() {
    let (stderr, reconstructed) = run(&["--inplace"]);
    assert!(reconstructed);
    assert!(
        stderr
            .contains("WARNING: src.bin failed verification -- update retained (will try again)."),
        "got: {stderr}"
    );
}

/// upstream: receiver.c:1318-1333 nulls `partialptr` once the failed temp is
/// saved, so the line still says "discarded" under `--partial-dir`.
#[test]
fn partial_dir_false_match_still_reads_discarded() {
    let (stderr, reconstructed) = run(&["--partial-dir=.part"]);
    assert!(reconstructed);
    assert!(
        stderr
            .contains("WARNING: src.bin failed verification -- update discarded (will try again)."),
        "got: {stderr}"
    );
}
