//! Unit tests for [`DirSandbox`](super::DirSandbox).
//!
//! Exercise the stack/cache invariants on tempdirs and confirm the
//! symlink-rejection policy fires at both the root open
//! ([`secure_open_dir`](crate::secure_open_dir::secure_open_dir)) and
//! the descent open ([`super::DirSandbox::enter`]).

use std::os::fd::AsRawFd;
use std::os::unix::fs::symlink;
use std::sync::Arc;
use std::thread;

use tempfile::tempdir;

use super::DirSandbox;

/// `tempdir()` may return a path under a symlinked prefix (macOS
/// `/tmp -> /private/tmp`, some CI runners stage `/tmp` through a
/// symlink). `secure_open_dir` refuses such paths under
/// `RESOLVE_NO_SYMLINKS`, so every test that opens a tempdir as the
/// sandbox root first canonicalises.
fn canonical_tempdir() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempdir().expect("tempdir");
    let canon = std::fs::canonicalize(dir.path()).expect("canonicalize tempdir");
    (dir, canon)
}

#[test]
fn open_root_yields_live_fd() {
    let (_keep, root) = canonical_tempdir();
    let sandbox = DirSandbox::open_root(&root).expect("open root");
    assert!(sandbox.current_dirfd().as_raw_fd() >= 0);
    assert_eq!(sandbox.depth(), 0);
}

#[test]
fn open_root_rejects_symlink_root() {
    let (_keep, root) = canonical_tempdir();
    let target = root.join("real");
    std::fs::create_dir(&target).expect("create real dir");
    let link = root.join("link");
    symlink(&target, &link).expect("create symlink");

    let err = DirSandbox::open_root(&link).expect_err("symlink root must be rejected");
    let code = err.raw_os_error();
    // Linux + openat2 returns ELOOP; Linux + plain open(O_NOFOLLOW)
    // also returns ELOOP; macOS/BSD evaluate O_DIRECTORY before
    // O_NOFOLLOW and return ENOTDIR for symlink-to-directory.
    assert!(
        code == Some(libc::ELOOP) || code == Some(libc::ENOTDIR),
        "expected ELOOP or ENOTDIR for symlink root, got: {err}"
    );
}

#[test]
fn enter_and_exit_balance_the_stack() {
    let (_keep, root) = canonical_tempdir();
    std::fs::create_dir(root.join("a")).expect("mkdir a");
    std::fs::create_dir(root.join("a/b")).expect("mkdir a/b");

    let mut sandbox = DirSandbox::open_root(&root).expect("open root");
    let root_raw = sandbox.current_dirfd().as_raw_fd();
    assert_eq!(sandbox.depth(), 0);

    sandbox.enter(std::ffi::OsStr::new("a")).expect("enter a");
    assert_eq!(sandbox.depth(), 1);
    let a_raw = sandbox.current_dirfd().as_raw_fd();
    assert_ne!(a_raw, root_raw, "entering must hand out a new fd");

    sandbox.enter(std::ffi::OsStr::new("b")).expect("enter b");
    assert_eq!(sandbox.depth(), 2);
    let b_raw = sandbox.current_dirfd().as_raw_fd();
    assert_ne!(b_raw, a_raw);

    sandbox.exit();
    assert_eq!(sandbox.depth(), 1);
    assert_eq!(
        sandbox.current_dirfd().as_raw_fd(),
        a_raw,
        "exit must restore the prior parent dirfd"
    );

    sandbox.exit();
    assert_eq!(sandbox.depth(), 0);
    assert_eq!(
        sandbox.current_dirfd().as_raw_fd(),
        root_raw,
        "exit to empty must return the root"
    );
}

#[test]
fn exit_on_empty_stack_is_noop() {
    let (_keep, root) = canonical_tempdir();
    let mut sandbox = DirSandbox::open_root(&root).expect("open root");
    sandbox.exit();
    sandbox.exit();
    assert_eq!(sandbox.depth(), 0);
}

#[test]
fn enter_rejects_symlink_child() {
    let (_keep, root) = canonical_tempdir();
    let real = root.join("real");
    std::fs::create_dir(&real).expect("create real dir");
    symlink(&real, root.join("link")).expect("symlink");

    let mut sandbox = DirSandbox::open_root(&root).expect("open root");
    let err = sandbox
        .enter(std::ffi::OsStr::new("link"))
        .expect_err("symlink child must be rejected");
    let code = err.raw_os_error();
    assert!(
        code == Some(libc::ELOOP) || code == Some(libc::ENOTDIR),
        "expected ELOOP or ENOTDIR for symlink child, got: {err}"
    );
    // Stack must be untouched when the open fails.
    assert_eq!(sandbox.depth(), 0);
}

#[test]
fn enter_rejects_missing_child() {
    let (_keep, root) = canonical_tempdir();
    let mut sandbox = DirSandbox::open_root(&root).expect("open root");
    let err = sandbox
        .enter(std::ffi::OsStr::new("does-not-exist"))
        .expect_err("missing child must error");
    assert_eq!(err.raw_os_error(), Some(libc::ENOENT));
    assert_eq!(sandbox.depth(), 0);
}

#[test]
fn enter_rejects_file_child() {
    let (_keep, root) = canonical_tempdir();
    std::fs::write(root.join("file"), b"x").expect("write file");
    let mut sandbox = DirSandbox::open_root(&root).expect("open root");
    let err = sandbox
        .enter(std::ffi::OsStr::new("file"))
        .expect_err("file child must error");
    assert_eq!(err.raw_os_error(), Some(libc::ENOTDIR));
    assert_eq!(sandbox.depth(), 0);
}

#[test]
fn secondary_is_idempotent() {
    let (_keep_root, root) = canonical_tempdir();
    let (_keep_other, other) = canonical_tempdir();
    let sandbox = DirSandbox::open_root(&root).expect("open root");
    assert_eq!(sandbox.secondary_count(), 0);

    let fd1 = sandbox.secondary(&other).expect("register operand");
    assert_eq!(sandbox.secondary_count(), 1);

    let fd2 = sandbox.secondary(&other).expect("re-lookup operand");
    assert_eq!(sandbox.secondary_count(), 1);
    assert!(
        Arc::ptr_eq(&fd1, &fd2),
        "second call must return the same Arc"
    );
}

#[test]
fn secondary_rejects_symlink_operand() {
    let (_keep_root, root) = canonical_tempdir();
    let (_keep_other, other) = canonical_tempdir();
    let target = other.join("real");
    std::fs::create_dir(&target).expect("create real");
    let link = other.join("link");
    symlink(&target, &link).expect("symlink");

    let sandbox = DirSandbox::open_root(&root).expect("open root");
    let err = sandbox
        .secondary(&link)
        .expect_err("symlink operand must be rejected");
    let code = err.raw_os_error();
    assert!(
        code == Some(libc::ELOOP) || code == Some(libc::ENOTDIR),
        "expected ELOOP or ENOTDIR for symlink operand, got: {err}"
    );
    assert_eq!(sandbox.secondary_count(), 0);
}

#[test]
fn secondary_concurrent_registrations_collapse_to_one() {
    let (_keep_root, root) = canonical_tempdir();
    let (_keep_other, other) = canonical_tempdir();
    let sandbox = Arc::new(DirSandbox::open_root(&root).expect("open root"));

    let handles: Vec<_> = (0..8)
        .map(|_| {
            let sandbox = Arc::clone(&sandbox);
            let other = other.clone();
            thread::spawn(move || sandbox.secondary(&other).expect("register"))
        })
        .collect();

    let mut fds = Vec::new();
    for handle in handles {
        fds.push(handle.join().expect("thread"));
    }

    // Every thread must observe the same cached Arc, and the cache
    // must contain exactly one entry regardless of registration races.
    let first = &fds[0];
    for other in &fds[1..] {
        assert!(
            Arc::ptr_eq(first, other),
            "all threads must share one operand handle"
        );
    }
    assert_eq!(sandbox.secondary_count(), 1);
}

#[test]
fn root_arc_clones_share_owner() {
    let (_keep, root) = canonical_tempdir();
    let sandbox = DirSandbox::open_root(&root).expect("open root");
    let arc1 = sandbox.root_arc();
    let arc2 = sandbox.root_arc();
    assert!(Arc::ptr_eq(&arc1, &arc2));
    assert_eq!(arc1.as_raw_fd(), sandbox.root_dirfd().as_raw_fd());
}

// ================================================================
// enter_follow_dirlinks tests: -K / --copy-dirlinks regression fix
// ================================================================

#[test]
fn enter_follow_dirlinks_allows_in_tree_symlink() {
    let (_keep, root) = canonical_tempdir();
    // Create a real directory and a symlink to it inside the tree.
    let real = root.join("real");
    std::fs::create_dir(&real).expect("create real dir");
    symlink(&real, root.join("link")).expect("symlink");

    let mut sandbox = DirSandbox::open_root(&root).expect("open root");
    // enter() would reject this symlink; enter_follow_dirlinks permits it.
    sandbox
        .enter_follow_dirlinks(std::ffi::OsStr::new("link"))
        .expect("enter_follow_dirlinks must succeed for in-tree symlink");
    assert_eq!(sandbox.depth(), 1);
    assert!(sandbox.current_dirfd().as_raw_fd() >= 0);
    sandbox.exit();
    assert_eq!(sandbox.depth(), 0);
}

#[test]
fn enter_follow_dirlinks_works_on_real_directory() {
    let (_keep, root) = canonical_tempdir();
    std::fs::create_dir(root.join("real")).expect("mkdir");

    let mut sandbox = DirSandbox::open_root(&root).expect("open root");
    sandbox
        .enter_follow_dirlinks(std::ffi::OsStr::new("real"))
        .expect("enter_follow_dirlinks must succeed for real directory");
    assert_eq!(sandbox.depth(), 1);
}

#[test]
fn enter_follow_dirlinks_rejects_nonexistent() {
    let (_keep, root) = canonical_tempdir();
    let mut sandbox = DirSandbox::open_root(&root).expect("open root");
    let err = sandbox
        .enter_follow_dirlinks(std::ffi::OsStr::new("nope"))
        .expect_err("missing child must error");
    assert_eq!(err.raw_os_error(), Some(libc::ENOENT));
    assert_eq!(sandbox.depth(), 0);
}

#[test]
fn enter_follow_dirlinks_rejects_file_child() {
    let (_keep, root) = canonical_tempdir();
    std::fs::write(root.join("file"), b"x").expect("write file");

    let mut sandbox = DirSandbox::open_root(&root).expect("open root");
    let err = sandbox
        .enter_follow_dirlinks(std::ffi::OsStr::new("file"))
        .expect_err("file child must error");
    assert_eq!(err.raw_os_error(), Some(libc::ENOTDIR));
    assert_eq!(sandbox.depth(), 0);
}

#[test]
fn enter_follow_dirlinks_nested_descent_through_symlink() {
    let (_keep, root) = canonical_tempdir();
    // Tree: root/real_a/real_b, root/link_a -> root/real_a
    std::fs::create_dir(root.join("real_a")).expect("mkdir real_a");
    std::fs::create_dir(root.join("real_a/real_b")).expect("mkdir real_a/real_b");
    symlink(root.join("real_a"), root.join("link_a")).expect("symlink");

    let mut sandbox = DirSandbox::open_root(&root).expect("open root");
    // Descend through the symlink, then into a real subdirectory.
    sandbox
        .enter_follow_dirlinks(std::ffi::OsStr::new("link_a"))
        .expect("enter symlink");
    assert_eq!(sandbox.depth(), 1);
    sandbox
        .enter(std::ffi::OsStr::new("real_b"))
        .expect("enter real child within symlinked parent");
    assert_eq!(sandbox.depth(), 2);
    sandbox.exit();
    sandbox.exit();
    assert_eq!(sandbox.depth(), 0);
}

#[test]
fn enter_follow_dirlinks_preserves_stack_on_failure() {
    let (_keep, root) = canonical_tempdir();
    std::fs::create_dir(root.join("real")).expect("mkdir");

    let mut sandbox = DirSandbox::open_root(&root).expect("open root");
    sandbox
        .enter(std::ffi::OsStr::new("real"))
        .expect("enter real");
    assert_eq!(sandbox.depth(), 1);

    // Attempt to enter a nonexistent child via the follow path.
    let _ = sandbox.enter_follow_dirlinks(std::ffi::OsStr::new("gone"));
    // Stack must not be corrupted on failure.
    assert_eq!(sandbox.depth(), 1);
}
