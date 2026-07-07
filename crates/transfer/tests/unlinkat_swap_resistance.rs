//! SEC-1.g integration test: the receiver-side obstacle unlink goes
//! through `fast_io::unlinkat` when a [`DirSandbox`] is wired, and the
//! syscall removes the leaf entry itself instead of following a
//! symlink swap to an attacker-chosen target.
//!
//! These tests do not stand up a full receiver pipeline. They exercise
//! the `unlink_via_sandbox_or_fallback` helper at the same anchor the
//! receiver uses (an open dirfd over the destination root) on a tree
//! shaped like a real run, and assert the TOCTOU-relevant outcome:
//!
//! 1. When `link_path = dest_dir/leaf` and the entry is a symlink, the
//!    sandbox-anchored unlink removes the symlink itself rather than
//!    chasing it to whatever target it points at outside the sandbox.
//! 2. When the relative path has multiple components, the helper falls
//!    back to `std::fs::remove_file` (the SEC-1.g cutover only touches
//!    single-component paths today; multi-component descent is the
//!    SEC-1.g.followup work mirroring SEC-1.f).
//! 3. `UnlinkFlags::Dir` mirrors `rmdir(2)` and refuses to remove a
//!    non-empty directory.

#![cfg(unix)]

use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use fast_io::{DirSandbox, UnlinkFlags, unlink_via_sandbox_or_fallback};
use tempfile::tempdir;

/// `tempdir()` may sit under a symlink prefix on macOS / some CI
/// runners; canonicalise so the sandbox open succeeds under
/// `RESOLVE_NO_SYMLINKS`.
fn canonical_tempdir() -> (tempfile::TempDir, PathBuf) {
    let dir = tempdir().expect("tempdir");
    let canon = std::fs::canonicalize(dir.path()).expect("canonicalize");
    (dir, canon)
}

/// Simulates the attack the receiver-side obstacle unlink defends
/// against: between the receiver's decide-to-delete moment and the
/// syscall, an attacker swaps `dest/target/` for a symlink to
/// `../../sensitive/`. `unlinkat` anchored on the sandbox dirfd
/// removes the symlink itself; the sensitive target outside the
/// destination tree must survive.
#[test]
fn unlink_via_sandbox_does_not_follow_swapped_symlink() {
    let (_keep, parent) = canonical_tempdir();

    // Sensitive tree the attacker hopes to redirect the unlink to.
    let sensitive_dir = parent.join("sensitive");
    std::fs::create_dir(&sensitive_dir).expect("mkdir sensitive");
    let sensitive_file = sensitive_dir.join("secret");
    std::fs::write(&sensitive_file, b"do-not-delete").expect("write secret");

    // Receiver-managed destination tree. The receiver decides to delete
    // `dest/target`; the attacker has swapped it for a symlink pointing
    // at the sensitive tree above.
    let dest = parent.join("dest");
    std::fs::create_dir(&dest).expect("mkdir dest");
    let target = dest.join("target");
    symlink(&sensitive_dir, &target).expect("symlink swap");

    let sandbox = DirSandbox::open_root(&dest).expect("sandbox");

    let leaf = Path::new("target");
    let link_path = dest.join(leaf);
    unlink_via_sandbox_or_fallback(Some(&sandbox), &dest, leaf, &link_path, UnlinkFlags::File)
        .expect("unlink leaf");

    assert!(
        !target.exists(),
        "swapped symlink must be removed itself, not followed"
    );
    assert!(
        sensitive_dir.is_dir(),
        "sensitive directory outside the sandbox must survive"
    );
    assert!(
        sensitive_file.is_file(),
        "file inside sensitive directory must survive"
    );
}

/// Sandbox-anchored unlink removes a regular file at a single-
/// component leaf, matching the obstacle-clear path the receiver
/// takes for symlink and hardlink quick-check misses.
#[test]
fn unlink_via_sandbox_removes_regular_file_at_leaf() {
    let (_keep, root) = canonical_tempdir();
    let path = root.join("obstacle");
    std::fs::write(&path, b"contents").expect("write");

    let sandbox = DirSandbox::open_root(&root).expect("sandbox");

    let leaf = Path::new("obstacle");
    unlink_via_sandbox_or_fallback(Some(&sandbox), &root, leaf, &path, UnlinkFlags::File)
        .expect("unlink");
    assert!(!path.exists(), "regular file leaf must be removed");
}

/// `UnlinkFlags::Dir` mirrors `rmdir(2)` exactly: refuses to remove a
/// non-empty directory, even when the sandbox dirfd is the immediate
/// parent. Pins the contract so future refactors do not silently
/// promote `rmdir` into a recursive removal.
#[test]
fn unlink_via_sandbox_dir_flag_refuses_non_empty_directory() {
    let (_keep, root) = canonical_tempdir();
    let dir = root.join("non-empty");
    std::fs::create_dir(&dir).expect("mkdir");
    std::fs::write(dir.join("inner"), b"x").expect("write inner");

    let sandbox = DirSandbox::open_root(&root).expect("sandbox");
    let leaf = Path::new("non-empty");
    let err = unlink_via_sandbox_or_fallback(Some(&sandbox), &root, leaf, &dir, UnlinkFlags::Dir)
        .expect_err("must refuse to rmdir a non-empty directory");
    let code = err.raw_os_error();
    assert!(
        code == Some(libc::ENOTEMPTY) || code == Some(libc::EEXIST),
        "expected ENOTEMPTY or EEXIST, got {code:?}"
    );
    assert!(dir.is_dir(), "directory must survive a failed rmdir");
}

/// Multi-component paths anchor their parent under
/// `openat2(RESOLVE_BENEATH)` where the kernel supports it and degrade
/// to `std::fs::remove_file` otherwise; either way the leaf is removed
/// end-to-end. This pins the removal contract across both capability
/// states so a future regression cannot silently drop the leaf.
#[test]
fn unlink_via_sandbox_multi_component_removes_leaf() {
    let (_keep, root) = canonical_tempdir();
    std::fs::create_dir(root.join("sub")).expect("mkdir sub");
    let path = root.join("sub/file");
    std::fs::write(&path, b"x").expect("write");

    let sandbox = DirSandbox::open_root(&root).expect("sandbox");
    let rel = Path::new("sub/file");
    unlink_via_sandbox_or_fallback(Some(&sandbox), &root, rel, &path, UnlinkFlags::File)
        .expect("unlink");
    assert!(
        !path.exists(),
        "multi-component leaf must be removed (anchored or fallback)"
    );
}
