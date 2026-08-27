//! Regression test for `--remove-source-files` on local copies.
//!
//! Confirms that running `oc-rsync -av --remove-source-files SRC/ DST/`
//! deletes each transferred file from the source tree while leaving the
//! source directory structure intact. Mirrors the upstream rsync
//! testsuite `delete.test` scenario:
//!
//! ```sh
//! $RSYNC -av --remove-source-files "$fromdir/" "$todir/"
//! diff -r "$chkdir/empty" "$fromdir"   # only empty dirs remain
//! ```
//!
//! # Upstream Reference
//!
//! - `sender.c:395` - `successful_send()` performs the unlink
//! - `options.c:765,2964-2965` - `remove_source_files` global and
//!   `--remove-source-files` forwarding to the server-side sender
//! - `testsuite/delete.test` - end-to-end coverage

use std::fs;
use std::process::Command;

use tempfile::tempdir;

/// `--remove-source-files` removes every transferred file from the source
/// tree (including nested entries) while leaving the directory layout
/// behind, mirroring upstream `successful_send()`.
#[test]
fn remove_source_files_unlinks_transferred_files() {
    let rsync_bin = test_support::oc_rsync_bin();

    let tmp = tempdir().expect("create tempdir");
    let from = tmp.path().join("from");
    let to = tmp.path().join("to");
    fs::create_dir_all(from.join("dir/subdir")).expect("mkdir from/dir/subdir");
    fs::create_dir_all(from.join("emptydir")).expect("mkdir from/emptydir");
    fs::create_dir_all(&to).expect("mkdir to");

    fs::write(from.join("top.txt"), b"top content").expect("write top.txt");
    fs::write(from.join("dir/inner.txt"), b"inner content").expect("write inner.txt");
    fs::write(from.join("dir/subdir/leaf.txt"), b"leaf content").expect("write leaf.txt");

    let from_arg = {
        let mut s = from.clone().into_os_string();
        s.push("/");
        s
    };

    let status = Command::new(&rsync_bin)
        .arg("-a")
        .arg("--remove-source-files")
        .arg(&from_arg)
        .arg(&to)
        .status()
        .expect("spawn oc-rsync");
    assert!(status.success(), "oc-rsync exited with {status:?}");

    assert!(to.join("top.txt").is_file(), "destination top.txt missing");
    assert!(
        to.join("dir/inner.txt").is_file(),
        "destination dir/inner.txt missing"
    );
    assert!(
        to.join("dir/subdir/leaf.txt").is_file(),
        "destination dir/subdir/leaf.txt missing"
    );

    assert!(
        !from.join("top.txt").exists(),
        "source top.txt should have been removed"
    );
    assert!(
        !from.join("dir/inner.txt").exists(),
        "source dir/inner.txt should have been removed"
    );
    assert!(
        !from.join("dir/subdir/leaf.txt").exists(),
        "source dir/subdir/leaf.txt should have been removed"
    );

    // upstream: sender.c:152-156 - successful_send() never removes the
    // directory entry itself, so the empty dir hierarchy must survive.
    assert!(from.join("dir").is_dir(), "source dir/ should remain");
    assert!(
        from.join("dir/subdir").is_dir(),
        "source dir/subdir/ should remain"
    );
    assert!(
        from.join("emptydir").is_dir(),
        "source emptydir/ should remain"
    );
}

/// `--remove-source-files` under `--dry-run` must NOT touch the source
/// tree. Upstream `successful_send()` returns early when `do_xfers` is
/// false (sender.c:405-406 via the `!remove_source_files` short-circuit
/// combined with the global `do_xfers` gate).
#[test]
fn remove_source_files_dry_run_preserves_source() {
    let rsync_bin = test_support::oc_rsync_bin();

    let tmp = tempdir().expect("create tempdir");
    let from = tmp.path().join("from");
    let to = tmp.path().join("to");
    fs::create_dir_all(&from).expect("mkdir from");
    fs::create_dir_all(&to).expect("mkdir to");
    fs::write(from.join("keep.txt"), b"keep").expect("write keep.txt");

    let from_arg = {
        let mut s = from.clone().into_os_string();
        s.push("/");
        s
    };

    let status = Command::new(&rsync_bin)
        .arg("-a")
        .arg("--dry-run")
        .arg("--remove-source-files")
        .arg(&from_arg)
        .arg(&to)
        .status()
        .expect("spawn oc-rsync");
    assert!(status.success(), "oc-rsync exited with {status:?}");

    assert!(
        from.join("keep.txt").is_file(),
        "dry-run must not remove source files"
    );
}
