//! SEC-1.f integration test: the receiver-side lstat-class probe goes
//! through `fast_io::fstatat_nofollow` when a [`DirSandbox`] is wired,
//! and the helper reports the symlink itself instead of resolving to
//! whatever the link points at.
//!
//! These tests do not stand up a full receiver pipeline. They exercise
//! the `lstat_via_sandbox_or_fallback` helper at the same anchor the
//! receiver uses (an open dirfd over the destination root) on a tree
//! shaped like a real run, and assert the TOCTOU-relevant outcome:
//!
//! 1. When `link_path = dest_dir/leaf` and the entry is a symlink, the
//!    sandbox-anchored stat reports `is_symlink() == true` rather than
//!    following the link.
//! 2. When the entry is a regular file, both the sandbox-anchored and
//!    path-based stats agree on `dev` / `ino`.
//! 3. When the relative path has multiple components, the helper anchors
//!    its parent under `openat2(RESOLVE_BENEATH)` where the kernel
//!    supports it (Linux 5.6+) and degrades to
//!    `std::fs::symlink_metadata` otherwise; either way the reported
//!    dev/ino match the real entry.

#![cfg(unix)]

use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use fast_io::{DirSandbox, LstatOutcome, lstat_via_sandbox_or_fallback};
use tempfile::tempdir;

/// `tempdir()` may sit under a symlink prefix on macOS / some CI
/// runners; canonicalise so the sandbox open succeeds under
/// `RESOLVE_NO_SYMLINKS`.
fn canonical_tempdir() -> (tempfile::TempDir, PathBuf) {
    let dir = tempdir().expect("tempdir");
    let canon = std::fs::canonicalize(dir.path()).expect("canonicalize");
    (dir, canon)
}

#[test]
fn sandbox_anchored_lstat_reports_symlink_at_leaf() {
    let (_keep, root) = canonical_tempdir();
    std::fs::write(root.join("real-target"), b"contents").expect("write target");
    symlink(root.join("real-target"), root.join("the-link")).expect("symlink");

    let sandbox = DirSandbox::open_root(&root).expect("sandbox");

    let leaf = Path::new("the-link");
    let link_path = root.join(leaf);
    let outcome = lstat_via_sandbox_or_fallback(Some(&sandbox), &root, leaf, &link_path)
        .expect("sandbox lstat");

    match outcome {
        LstatOutcome::At(meta) => {
            assert!(
                meta.is_symlink(),
                "AT_SYMLINK_NOFOLLOW must report the symlink itself"
            );
            assert!(
                !meta.is_file(),
                "the symlink leaf must not be classified as the regular file it points at"
            );
        }
        LstatOutcome::Std(_) => {
            panic!("expected the sandbox-anchored fstatat path for a single-component leaf");
        }
    }
}

#[test]
fn sandbox_anchored_lstat_dev_ino_matches_path_lstat() {
    use std::os::unix::fs::MetadataExt;

    let (_keep, root) = canonical_tempdir();
    let file_path = root.join("regular");
    std::fs::write(&file_path, b"contents").expect("write");

    let sandbox = DirSandbox::open_root(&root).expect("sandbox");
    let leaf = Path::new("regular");

    let via_sandbox =
        lstat_via_sandbox_or_fallback(Some(&sandbox), &root, leaf, &file_path).expect("sandbox");
    let via_path = std::fs::symlink_metadata(&file_path).expect("symlink_metadata");

    assert_eq!(
        via_sandbox.dev(),
        via_path.dev(),
        "dev id must match across the two stat paths"
    );
    assert_eq!(
        via_sandbox.ino(),
        via_path.ino(),
        "ino must match across the two stat paths"
    );
}

#[test]
fn multi_component_path_anchors_or_falls_back_lstat() {
    // A multi-component relative path now resolves its parent under
    // openat2(RESOLVE_BENEATH) where the kernel supports it (Linux 5.6+,
    // the CI runners) and only degrades to the path-based fallback where
    // it does not (macOS, older kernels). Gate the expected outcome on
    // the capability probe and confirm dev/ino match the real entry in
    // both states.
    let (_keep, root) = canonical_tempdir();
    std::fs::create_dir(root.join("sub")).expect("mkdir sub");
    let file_path = root.join("sub/file");
    std::fs::write(&file_path, b"x").expect("write");

    let sandbox = DirSandbox::open_root(&root).expect("sandbox");

    let rel = Path::new("sub/file");
    let outcome = lstat_via_sandbox_or_fallback(Some(&sandbox), &root, rel, &file_path)
        .expect("multi-comp lstat");

    if fast_io::openat2_supported() {
        assert!(
            matches!(outcome, LstatOutcome::At(_)),
            "multi-component paths must anchor via openat2(RESOLVE_BENEATH) when supported"
        );
    } else {
        assert!(
            matches!(outcome, LstatOutcome::Std(_)),
            "multi-component paths degrade to the path-based fallback without openat2"
        );
    }

    let std_meta = std::fs::symlink_metadata(&file_path).expect("std stat");
    assert_eq!(
        std::os::unix::fs::MetadataExt::dev(&std_meta),
        outcome.dev()
    );
    assert_eq!(
        std::os::unix::fs::MetadataExt::ino(&std_meta),
        outcome.ino()
    );
}

#[test]
fn sandbox_anchored_lstat_returns_enoent_for_missing_leaf() {
    let (_keep, root) = canonical_tempdir();
    let sandbox = DirSandbox::open_root(&root).expect("sandbox");

    let leaf = Path::new("does-not-exist");
    let link_path = root.join(leaf);
    let err = lstat_via_sandbox_or_fallback(Some(&sandbox), &root, leaf, &link_path)
        .expect_err("missing leaf must error");
    assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
}

/// Simulates the receiver-side hardlink quick-check at
/// `crates/transfer/src/receiver/directory/links.rs:227`: the link
/// destination is stated through the sandbox so a mid-syscall swap on
/// the leaf cannot redirect the dev/ino comparison to an attacker-
/// chosen inode.
///
/// The test does not exercise the full receiver run; it confirms that
/// the helper consumed by `create_hardlinks` agrees with the kernel on
/// the symlink leaf rather than following it.
#[test]
fn receiver_shaped_hardlink_quickcheck_uses_at_path() {
    let (_keep, root) = canonical_tempdir();

    // Simulate the leader having already been committed by the receiver
    // and the follower currently being processed: both files exist in
    // the destination tree.
    std::fs::write(root.join("leader"), b"shared").expect("write leader");
    std::fs::hard_link(root.join("leader"), root.join("follower")).expect("hard link");

    let sandbox = DirSandbox::open_root(&root).expect("sandbox");

    let follower_rel = Path::new("follower");
    let follower_path = root.join(follower_rel);
    let leader_path = root.join("leader");

    use std::os::unix::fs::MetadataExt;
    let follower =
        lstat_via_sandbox_or_fallback(Some(&sandbox), &root, follower_rel, &follower_path)
            .expect("follower lstat");
    let leader = std::fs::symlink_metadata(&leader_path).expect("leader lstat");

    assert_eq!(follower.dev(), leader.dev(), "same filesystem");
    assert_eq!(
        follower.ino(),
        leader.ino(),
        "hardlinked follower must share an inode with the leader"
    );
}
