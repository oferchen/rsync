//! Regression: `--delete --files-from` must not delete-scan the transfer root.
//!
//! Upstream `generator.c:do_delete_pass()` calls `delete_in_dir()` for a flist
//! directory only when it carries `FLAG_CONTENT_DIR` - a directory the sender
//! actually recursed into. Under `--files-from` the transfer root is an implied
//! (non-content) directory, so upstream never scans the destination root for
//! extraneous entries. Our receiver used to register the root `.` as a
//! delete-scan target unconditionally, deleting a stale top-level destination
//! file that upstream preserves. This exercises the end-to-end oc-rsync binary
//! to pin the content-dir gate.
//!
//! # Upstream Reference
//!
//! - `generator.c:358-390 do_delete_pass()` - `if (!(file->flags &
//!   FLAG_CONTENT_DIR)) continue;` before `delete_in_dir()`.
//! - `flist.c:2239` - `int flags = recurse ? FLAG_CONTENT_DIR : 0;` (the root is
//!   a content dir only for a recursive transfer).

use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

use tempfile::tempdir;

/// Locates the workspace `oc-rsync` binary the test runner built.
fn locate_oc_rsync() -> Option<PathBuf> {
    if let Some(p) = env::var_os("CARGO_BIN_EXE_oc-rsync") {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Some(p);
        }
    }
    let exe = env::current_exe().ok()?;
    let mut dir = exe.parent()?;
    let name = format!("oc-rsync{}", env::consts::EXE_SUFFIX);
    while !dir.ends_with("target") {
        let candidate = dir.join(&name);
        if candidate.is_file() {
            return Some(candidate);
        }
        dir = dir.parent()?;
    }
    for sub in ["debug", "release"] {
        let candidate = dir.join(sub).join(&name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// `--delete --files-from`: a stale destination-root file that is absent from
/// the file list must survive, because the `--files-from` root is not a content
/// directory and upstream never delete-scans it.
#[test]
fn files_from_delete_preserves_stale_dest_root_file() {
    let Some(oc) = locate_oc_rsync() else {
        eprintln!("oc-rsync binary not found; skipping");
        return;
    };

    let tmp = tempdir().unwrap();
    let root = tmp.path();
    let src = root.join("src");
    let dst = root.join("dst");
    fs::create_dir_all(src.join("subdir")).unwrap();
    fs::create_dir_all(dst.join("subdir")).unwrap();
    fs::write(src.join("subdir/keep.txt"), b"keep").unwrap();
    fs::write(root.join("list.txt"), b"subdir/keep.txt\n").unwrap();
    // Extraneous file at the destination ROOT, absent from the file list.
    fs::write(dst.join("root_stale.txt"), b"stale").unwrap();

    let status = Command::new(&oc)
        .arg("-a")
        .arg("--delete")
        .arg(format!("--files-from={}", root.join("list.txt").display()))
        .arg(format!("{}/", src.display()))
        .arg(format!("{}/", dst.display()))
        .status()
        .expect("run oc-rsync");
    assert!(status.success(), "transfer should succeed");

    assert!(
        dst.join("root_stale.txt").is_file(),
        "the --files-from root is a non-content dir; upstream never \
         delete-scans it, so the stale destination-root file must survive",
    );
    assert!(
        dst.join("subdir/keep.txt").is_file(),
        "the listed file must be present",
    );
}

/// Control: a plain recursive `--delete` (no `--files-from`) still deletes a
/// stale destination-root file, because the recursive transfer root IS a
/// content directory. Guards against the content-dir gate over-suppressing the
/// normal delete pass.
#[test]
fn recursive_delete_still_removes_stale_dest_root_file() {
    let Some(oc) = locate_oc_rsync() else {
        eprintln!("oc-rsync binary not found; skipping");
        return;
    };

    let tmp = tempdir().unwrap();
    let root = tmp.path();
    let src = root.join("src");
    let dst = root.join("dst");
    fs::create_dir_all(src.join("subdir")).unwrap();
    fs::create_dir_all(&dst).unwrap();
    fs::write(src.join("subdir/keep.txt"), b"keep").unwrap();
    fs::write(dst.join("root_stale.txt"), b"stale").unwrap();

    let status = Command::new(&oc)
        .arg("-a")
        .arg("--delete")
        .arg(format!("{}/", src.display()))
        .arg(format!("{}/", dst.display()))
        .status()
        .expect("run oc-rsync");
    assert!(status.success(), "transfer should succeed");

    assert!(
        !dst.join("root_stale.txt").exists(),
        "a recursive --delete root is a content dir; the stale file must be \
         removed (no over-suppression from the content-dir gate)",
    );
}
