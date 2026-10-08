//! A pull that resumes from a `--partial-dir` file must keep the sender from
//! emitting backward block matches.
//!
//! The receiver writes such a resume in place into the partial file
//! (`receiver.c:1153-1155` `one_inplace`, `:1212-1213`), so the sender has to
//! treat the basis as one being overwritten: `sender.c:629` sets
//! `updating_basis_file` from `inplace_partial && fnamecmp_type ==
//! FNAMECMP_PARTIAL_DIR`, and `match.c` then refuses a block that lies behind
//! the write position. `inplace_partial` is the negotiated
//! `CF_INPLACE_PARTIAL_DIR` capability alone (`compat.c:789-790`). A pull
//! client never forwards `--partial-dir` to the server (`options.c:3062`), so a
//! sender that also demanded a local partial dir never set the flag, matched
//! every reversed block, and corrupted an upstream receiver's in-place
//! reconstruction (measured against rsync 3.5.1 on a 2 MiB file: "failed
//! verification -- update put into partial-dir", then a full redo).
//!
//! Upstream 3.5.1 on this fixture: half the blocks match, half go literal.

#![cfg(unix)]

use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

const BLOCK: usize = 1024;
const BLOCKS: usize = 8;

fn binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync"))
}

/// Deterministic pseudo-random bytes (xorshift64), so no two blocks collide.
fn pseudo_random(len: usize, seed: u64) -> Vec<u8> {
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

/// A stand-in `ssh` that drops the host argument and execs the rest locally.
fn fake_rsh(root: &Path) -> PathBuf {
    let path = root.join("fake-rsh.sh");
    let mut f = fs::File::create(&path).expect("create rsh");
    f.write_all(b"#!/bin/sh\nshift\nexec \"$@\"\n")
        .expect("write rsh");
    drop(f);
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).expect("chmod rsh");
    path
}

/// Reads `"<label>: N bytes"` from `--stats` output.
fn stat_bytes(stdout: &str, label: &str) -> u64 {
    let line = stdout
        .lines()
        .find(|l| l.starts_with(label))
        .unwrap_or_else(|| panic!("no {label:?} line in:\n{stdout}"));
    line[label.len()..]
        .trim()
        .trim_end_matches(" bytes")
        .replace(',', "")
        .parse()
        .unwrap_or_else(|e| panic!("bad {label:?} line {line:?}: {e}"))
}

#[test]
fn pull_partial_dir_resume_refuses_backward_matches_like_upstream() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();
    let src_dir = root.join("src");
    let dst_dir = root.join("dst");
    fs::create_dir_all(&src_dir).expect("src dir");
    fs::create_dir_all(dst_dir.join(".pd")).expect("partial dir");

    let data = pseudo_random(BLOCK * BLOCKS, 0x1374);
    fs::write(src_dir.join("f"), &data).expect("write src");
    // The partial holds the same blocks in reverse order: source block i sits
    // at basis block BLOCKS-1-i, so only the first half lies at or ahead of the
    // write position.
    let reversed: Vec<u8> = data.chunks(BLOCK).rev().flatten().copied().collect();
    fs::write(dst_dir.join(".pd").join("f"), &reversed).expect("write partial");

    let bin = binary();
    let out = Command::new(&bin)
        .args(["-t", "--no-whole-file", "--stats"])
        .arg(format!("--block-size={BLOCK}"))
        .arg("--partial-dir=.pd")
        .arg("-e")
        .arg(fake_rsh(root))
        .arg(format!("--rsync-path={}", bin.display()))
        .arg(format!("fake:{}", src_dir.join("f").display()))
        .arg(format!("{}/", dst_dir.display()))
        .output()
        .expect("run oc-rsync pull");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "pull failed: {}\n{stdout}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(fs::read(dst_dir.join("f")).expect("read dest"), data);

    let half = (BLOCK * BLOCKS / 2) as u64;
    assert_eq!(stat_bytes(&stdout, "Matched data:"), half, "{stdout}");
    assert_eq!(stat_bytes(&stdout, "Literal data:"), half, "{stdout}");
}
