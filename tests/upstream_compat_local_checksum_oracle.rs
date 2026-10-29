//! Local `--checksum` decisions and output against upstream rsync 3.5.1.
//!
//! oc decides a local `-c` copy by hashing both sides through the parallel
//! checksum service, splitting files of two or more 4 MiB blocks into
//! block vectors. Upstream compares one whole-file `file_checksum()` per side
//! (`generator.c:quick_check_ok()`). The two must reach the same decision for
//! every file, so this oracle runs both binaries on identical fixtures and
//! requires the same itemized transcript, the same `--stats` file counts, the
//! same exit code and the same destination tree.
//!
//! Every changed file keeps its size and mtime, so only the checksum can see
//! the change; the block-split files carry their one-byte edit in the first,
//! a middle and the last block.
//!
//! Gating: skips silently unless `OC_RSYNC_UPSTREAM_COMPAT=1`, then self-skips
//! when upstream 3.5.1 is not installed (`require_upstream_rsync`).
#![cfg(unix)]

use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime};

use tempfile::tempdir;
use test_support::{
    DirDiff, DirDiffOptions, UpstreamVersion, require_upstream_rsync, upstream_compat_enabled,
};

const BLOCK: usize = 4 << 20;
const BIG: usize = 2 * BLOCK + (1 << 20);

fn oc_rsync_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync"))
}

fn write(path: &Path, data: &[u8], mtime: SystemTime) {
    fs::write(path, data).unwrap();
    File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(mtime)
        .unwrap();
}

/// Builds `root/src` and `root/dst` so every file but `same.txt` and
/// `big_same.bin` differs from its destination only in content.
fn build_fixture(root: &Path) {
    let (src, dst) = (root.join("src"), root.join("dst"));
    fs::create_dir_all(&src).unwrap();
    fs::create_dir_all(&dst).unwrap();
    let t = SystemTime::UNIX_EPOCH + Duration::from_secs(1_600_000_000);
    let big: Vec<u8> = (0..BIG).map(|i| (i % 251) as u8).collect();

    write(&src.join("same.txt"), b"same\n", t);
    // Content equal, mtime older: -c skips the data and only sets the time.
    write(
        &dst.join("same.txt"),
        b"same\n",
        t - Duration::from_secs(100),
    );
    write(&src.join("small_diff.txt"), b"aaaa\n", t);
    write(&dst.join("small_diff.txt"), b"aaab\n", t);
    write(&src.join("resized.txt"), b"long content\n", t);
    write(&dst.join("resized.txt"), b"short\n", t);
    write(&src.join("new.txt"), b"new\n", t);
    write(&src.join("big_same.bin"), &big, t);
    write(&dst.join("big_same.bin"), &big, t);
    for (name, offset) in [
        ("edit_first.bin", 3),
        ("edit_mid.bin", BLOCK + 5),
        ("edit_last.bin", BIG - 1),
    ] {
        let mut edited = big.clone();
        edited[offset] ^= 1;
        write(&src.join(name), &big, t);
        write(&dst.join(name), &edited, t);
    }
    for dir in [&src, &dst] {
        File::open(dir).unwrap().set_modified(t).unwrap();
    }
}

/// Runs `binary -a -c -i --stats` and returns (exit code, transcript).
///
/// The byte totals and rates are dropped: local-copy wire totals are a known,
/// separately tracked gap, and rates are timing. Everything that depends on
/// a checksum decision stays.
fn run(binary: &Path, root: &Path) -> (Option<i32>, String) {
    let out = Command::new(binary)
        .args(["-a", "-c", "-i", "--stats"])
        .arg(format!("{}/src/", root.display()))
        .arg(format!("{}/dst/", root.display()))
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    let kept: Vec<&str> = text
        .lines()
        .filter(|line| {
            ![
                "Total bytes",
                "sent ",
                "total size",
                "File list generation",
                "File list transfer",
            ]
            .iter()
            .any(|prefix| line.starts_with(prefix))
        })
        .collect();
    (out.status.code(), kept.join("\n"))
}

#[test]
fn local_checksum_decisions_match_upstream() {
    if !upstream_compat_enabled() {
        return;
    }
    let Some(upstream) = require_upstream_rsync(UpstreamVersion::V3_5_1) else {
        return;
    };
    let work = tempdir().unwrap();
    let (up_root, oc_root) = (work.path().join("up"), work.path().join("oc"));
    build_fixture(&up_root);
    build_fixture(&oc_root);

    let (up_code, up_text) = run(upstream.binary(), &up_root);
    let (oc_code, oc_text) = run(&oc_rsync_bin(), &oc_root);

    assert_eq!(oc_code, up_code, "exit code");
    assert_eq!(oc_text, up_text, "transcript");
    for name in ["edit_first.bin", "edit_mid.bin", "edit_last.bin"] {
        assert!(
            up_text.contains(&format!(">fc........ {name}")),
            "fixture lost its point: upstream did not transfer {name}"
        );
    }
    if let Err(mismatch) = DirDiff::compare(
        &up_root.join("dst"),
        &oc_root.join("dst"),
        DirDiffOptions::archive(),
    )
    .expect("tree walk")
    {
        panic!("{}", mismatch.into_panic_message());
    }
}
