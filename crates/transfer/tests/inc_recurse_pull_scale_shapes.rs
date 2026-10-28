//! Incremental-recursion pulls of the two tree shapes that stress the sub-list
//! window: one directory wider than the sender's lookahead ceiling, and a
//! chain of nested directories each holding one sub-list.
//!
//! upstream: rsync.h:151-152 bounds the sender's lookahead at
//! MIN_FILECNT_LOOKAHEAD (1000) and MAX_FILECNT_LOOKAHEAD (10000) entries. A
//! directory of 12,000 files arrives as one sub-list above that ceiling, and a
//! 1,000-deep chain makes the receiver free its oldest lists
//! (generator.c:2691-2712 check_for_finished_files) while newer ones are still
//! being sent (sender.c:525-545). Either shape deadlocks a transfer whose two
//! ends disagree on when a list may be released.
//!
//! The tests keep no clock of their own: a hang is ended by nextest's
//! slow-timeout, and `run_reporting_hangs` turns that into a failure that
//! names each thread's state and wait channel. rsync 3.5.1 completes both
//! shapes on the same trees.
#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use test_support::{
    LSH_STUB_BIN, LshRunnerStub, create_tempdir, require_binaries, run_reporting_hangs,
};

/// Above upstream's MAX_FILECNT_LOOKAHEAD of 10,000.
const WIDE_FILES: usize = 12_000;
/// The deepest chain whose paths stay under upstream's MAXPATHLEN (4096) here.
const DEEP_LEVELS: usize = 1_000;

fn pull(src: &Path, dest: &Path) {
    let stub = LshRunnerStub::locate().expect("lsh-stub located");
    let oc = test_support::oc_rsync_bin();
    let output = run_reporting_hangs(
        Command::new(&oc)
            .arg("-r")
            .arg(format!("--rsh={}", stub.path().display()))
            .arg(format!("--rsync-path={}", oc.display()))
            .arg(format!("localhost:{}/", src.display()))
            .arg(format!("{}/", dest.display())),
    )
    .expect("pull runs");
    assert!(
        output.status.success(),
        "pull failed with {}: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Every regular file under `root`, relative to it and sorted.
fn regular_files(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).expect("read_dir") {
            let path = entry.expect("dir entry").path();
            if path.is_dir() {
                stack.push(path);
            } else {
                found.push(path.strip_prefix(root).expect("under root").to_path_buf());
            }
        }
    }
    found.sort();
    found
}

fn assert_trees_match(src: &Path, dest: &Path, expected: usize) {
    let sent = regular_files(src);
    assert_eq!(sent.len(), expected, "fixture size");
    assert_eq!(regular_files(dest), sent, "every file arrives, none extra");
}

#[test]
fn pull_of_a_directory_wider_than_the_lookahead_ceiling_completes() {
    require_binaries!("oc-rsync", LSH_STUB_BIN);
    let tmp = create_tempdir();
    let (src, dest) = (tmp.path().join("src"), tmp.path().join("dest"));
    // A sibling on each side of the wide directory, so the wide sub-list is
    // neither the first nor the last one the receiver frees.
    for dir in ["a", "wide", "z"] {
        fs::create_dir_all(src.join(dir)).expect("mkdir src");
    }
    fs::write(src.join("a/first"), b"a").expect("write");
    fs::write(src.join("z/last"), b"z").expect("write");
    for i in 0..WIDE_FILES {
        fs::write(src.join(format!("wide/f{i:05}")), []).expect("write wide file");
    }

    pull(&src, &dest);

    assert_trees_match(&src, &dest, WIDE_FILES + 2);
}

#[test]
fn pull_of_a_deep_directory_chain_completes() {
    require_binaries!("oc-rsync", LSH_STUB_BIN);
    let tmp = create_tempdir();
    let (src, dest) = (tmp.path().join("src"), tmp.path().join("dest"));
    let mut dir = src.clone();
    for level in 0..DEEP_LEVELS {
        dir.push("d");
        fs::create_dir_all(&dir).expect("mkdir src");
        fs::write(dir.join("f"), level.to_string()).expect("write");
    }

    pull(&src, &dest);

    assert_trees_match(&src, &dest, DEEP_LEVELS);
}
