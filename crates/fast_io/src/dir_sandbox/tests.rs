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

/// EDG-SANDBOX.2 contract test - chdir-symlink-race trap.
///
/// Locks the documented error-class contract that the
/// receiver's silent-skip audit (`docs/audits/edg-sandbox-silent-skip.md`)
/// and the PR #5565 refined error discrimination both rely on:
/// `DirSandbox::enter` MUST refuse to traverse a symlink at the leaf,
/// even when the symlink's target points outside the sandbox root (the
/// classic chdir-symlink-race shape where an attacker drops
/// `subdir -> ../outside` between the receiver's `mkdir` and its first
/// per-entry syscall).
///
/// The kernel rejects with `ELOOP` on Linux + `openat2(RESOLVE_NO_SYMLINKS)`
/// and on plain `openat(O_NOFOLLOW)`; macOS/BSD evaluates `O_DIRECTORY`
/// before `O_NOFOLLOW` and returns `ENOTDIR` for a symlink-to-directory.
/// On Linux + `openat2(RESOLVE_BENEATH)` a `..` segment that escapes the
/// root surfaces as `EXDEV`. All three are valid refusal classes; the
/// invariant is that `enter` never silently succeeds and the stack stays
/// untouched.
#[test]
fn enter_through_symlink_to_outside_refuses() {
    let (_keep_root, root) = canonical_tempdir();
    // The "outside" target lives in a sibling tempdir so the symlink
    // genuinely points outside the sandbox root. The chdir-symlink-race
    // POC drops a similar shape mid-transfer to redirect per-entry
    // syscalls to an attacker-chosen parent.
    let (_keep_outside, outside) = canonical_tempdir();
    symlink(&outside, root.join("subdir")).expect("plant trap symlink");

    let mut sandbox = DirSandbox::open_root(&root).expect("open root");
    let err = sandbox
        .enter(std::ffi::OsStr::new("subdir"))
        .expect_err("symlink trap must be refused");
    let code = err.raw_os_error();
    assert!(
        code == Some(libc::ELOOP) || code == Some(libc::ENOTDIR) || code == Some(libc::EXDEV),
        "expected ELOOP, ENOTDIR, or EXDEV for symlink-to-outside trap, got: {err}"
    );
    // The descent stack must stay empty so the receiver's subsequent
    // `current_dirfd()` call still anchors on the sandbox root, not on
    // an attacker-controlled descriptor.
    assert_eq!(sandbox.depth(), 0);
}

/// EDG-SANDBOX.2 positive contract test.
///
/// Sibling of [`enter_through_symlink_to_outside_refuses`]: confirms that a
/// real on-tree subdirectory is accepted so the audit's "refine error
/// discrimination" rule (PR #5565) does not regress the happy path. A
/// receiver that fails closed on every error class must still let
/// legitimate descents through.
#[test]
fn enter_to_legitimate_subdir_returns_ok() {
    let (_keep, root) = canonical_tempdir();
    std::fs::create_dir(root.join("subdir")).expect("mkdir subdir");

    let mut sandbox = DirSandbox::open_root(&root).expect("open root");
    sandbox
        .enter(std::ffi::OsStr::new("subdir"))
        .expect("legitimate subdir must succeed");
    assert_eq!(sandbox.depth(), 1);
    sandbox.exit();
    assert_eq!(sandbox.depth(), 0);
}
