//! Integration test that exercises the SEC-1.e [`fast_io::DirSandbox`]
//! carrier in the depth-first descent shape the receiver pipeline uses.
//!
//! This PR (SEC-1.e) only wires the carrier through to the receiver and
//! does not migrate any syscalls; the existing path-based code paths
//! remain the active code. The test therefore:
//!
//! 1. builds a realistic destination tree (root + nested subdirs +
//!    files + symlink + a secondary-operand root),
//! 2. opens a [`fast_io::DirSandbox`] at the destination root the same
//!    way `ReceiverContext::setup_transfer` does,
//! 3. walks the tree via `enter` / `exit`, verifying that
//!    `current_dirfd` always tracks the depth-first cursor,
//! 4. registers the operand root via `secondary` and confirms the
//!    `Arc<OwnedFd>` is shared across lookups,
//! 5. confirms the carrier refuses to descend through a symlink (the
//!    SEC-1 confinement invariant) without disturbing the stack.
//!
//! No behaviour change in the receiver is asserted here - that is
//! covered by the existing receiver integration tests in
//! `crates/transfer/tests/` and by the parallel-receive-delta and
//! incremental-flist test suites the workspace CI already runs. This
//! test exists to prove the carrier shape SEC-1.f-j will consume is
//! correct end-to-end against a real filesystem layout.

#![cfg(unix)]

use std::ffi::OsStr;
use std::os::fd::AsRawFd;
use std::os::unix::fs::symlink;
use std::sync::Arc;

use fast_io::DirSandbox;
use tempfile::tempdir;

/// `tempdir()` paths may include a symlink prefix (macOS `/tmp ->
/// /private/tmp`, some CI runners). [`DirSandbox::open_root`] refuses
/// such paths under `RESOLVE_NO_SYMLINKS`, so canonicalise first.
fn canonical_tempdir() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempdir().expect("tempdir");
    let canon = std::fs::canonicalize(dir.path()).expect("canonicalize");
    (dir, canon)
}

#[test]
fn receiver_shaped_descent_tracks_current_dirfd() {
    let (_keep, root) = canonical_tempdir();

    // Build a destination tree shaped like a typical receiver run:
    //   root/
    //     a/
    //       b/
    //         file.bin
    //       sibling.txt
    //     c/
    std::fs::create_dir(root.join("a")).unwrap();
    std::fs::create_dir(root.join("a/b")).unwrap();
    std::fs::create_dir(root.join("c")).unwrap();
    std::fs::write(root.join("a/b/file.bin"), b"payload").unwrap();
    std::fs::write(root.join("a/sibling.txt"), b"x").unwrap();

    let mut sandbox = DirSandbox::open_root(&root).expect("open root");
    let root_raw = sandbox.current_dirfd().as_raw_fd();
    assert_eq!(sandbox.depth(), 0);

    // Descend into a/b, simulating the receiver walking a parent for a
    // file entry. current_dirfd should hand back the b dirfd.
    sandbox.enter(OsStr::new("a")).expect("enter a");
    let a_raw = sandbox.current_dirfd().as_raw_fd();
    sandbox.enter(OsStr::new("b")).expect("enter a/b");
    let b_raw = sandbox.current_dirfd().as_raw_fd();

    assert_ne!(a_raw, root_raw, "entering must hand out a fresh fd");
    assert_ne!(b_raw, a_raw, "entering deeper must hand out a fresh fd");
    assert_eq!(sandbox.depth(), 2);

    // Pop back to a, then jump sideways into c. The carrier exposes a
    // single descent cursor; cross-branch hops are an exit-to-parent
    // followed by enter, which mirrors how the receiver's depth-first
    // walker emits directory boundaries.
    sandbox.exit();
    assert_eq!(sandbox.current_dirfd().as_raw_fd(), a_raw);
    sandbox.exit();
    assert_eq!(sandbox.current_dirfd().as_raw_fd(), root_raw);

    sandbox.enter(OsStr::new("c")).expect("enter c");
    let c_raw = sandbox.current_dirfd().as_raw_fd();
    // Note: the kernel is free to recycle the file-descriptor number
    // that `exit()` just closed, so we can't assert `c_raw != a_raw`.
    // What matters is that the new fd is a live, distinct kernel
    // object - confirmed by the successful `enter` returning Ok and
    // the depth tracking matching the expected push.
    assert_ne!(c_raw, root_raw);
    assert_eq!(sandbox.depth(), 1);

    sandbox.exit();
    assert_eq!(sandbox.depth(), 0);
    assert_eq!(sandbox.current_dirfd().as_raw_fd(), root_raw);
}

#[test]
fn descent_refuses_symlink_leaf_without_disturbing_stack() {
    let (_keep, root) = canonical_tempdir();
    std::fs::create_dir(root.join("real")).unwrap();
    // `link` is a symlink that resolves to `real`. Entering `link`
    // must fail under the leaf `O_NOFOLLOW` invariant on every Unix
    // target the carrier supports; on Linux 5.6+ the `openat2`
    // upgrade adds `RESOLVE_NO_SYMLINKS` for the same refusal.
    symlink(root.join("real"), root.join("link")).unwrap();

    let mut sandbox = DirSandbox::open_root(&root).expect("open root");
    let root_raw = sandbox.current_dirfd().as_raw_fd();
    let depth_before = sandbox.depth();

    let err = sandbox
        .enter(OsStr::new("link"))
        .expect_err("symlink leaf must be rejected");

    // Accepted refusals across Unix variants:
    // - Linux openat2 `RESOLVE_NO_SYMLINKS` -> ELOOP (raw 40).
    // - Linux openat `O_NOFOLLOW | O_DIRECTORY` on a symlink leaf ->
    //   ELOOP (raw 40).
    // - macOS / BSD evaluate O_DIRECTORY before O_NOFOLLOW and report
    //   ENOTDIR (raw 20) for symlink-to-directory.
    let code = err.raw_os_error().expect("must carry an errno");
    let accepted: &[i32] = &[
        20, // ENOTDIR on macOS / BSD
        40, // ELOOP on Linux
        62, // ELOOP on macOS / BSD
    ];
    assert!(
        accepted.contains(&code),
        "expected ENOTDIR or ELOOP for symlink leaf, got errno={code} ({err})"
    );

    assert_eq!(sandbox.depth(), depth_before, "failed enter must not push");
    assert_eq!(
        sandbox.current_dirfd().as_raw_fd(),
        root_raw,
        "failed enter must not perturb the cursor"
    );
}

#[test]
fn secondary_operand_shares_handle_across_lookups() {
    let (_keep_root, root) = canonical_tempdir();
    let (_keep_op, operand) = canonical_tempdir();
    std::fs::create_dir(operand.join("subdir")).unwrap();

    let sandbox = DirSandbox::open_root(&root).expect("open root");
    assert_eq!(sandbox.secondary_count(), 0);

    let fd1 = sandbox.secondary(&operand).expect("register");
    let fd2 = sandbox.secondary(&operand).expect("re-lookup");
    assert!(Arc::ptr_eq(&fd1, &fd2));
    assert_eq!(sandbox.secondary_count(), 1);

    // A different operand path produces a different cached handle.
    let (_keep_op2, operand2) = canonical_tempdir();
    let fd3 = sandbox.secondary(&operand2).expect("register second");
    assert!(!Arc::ptr_eq(&fd1, &fd3));
    assert_eq!(sandbox.secondary_count(), 2);
}

#[test]
fn root_arc_outlives_borrowed_cursor() {
    // The carrier hands callers an `Arc<OwnedFd>` for the root so a
    // background disk-commit thread (the receiver's pipelined commit
    // path) can hold an owner that survives the per-entry borrow
    // lifetime. Confirm the Arc clone is a stable handle.
    let (_keep, root) = canonical_tempdir();
    let sandbox = DirSandbox::open_root(&root).expect("open root");
    let arc = sandbox.root_arc();
    let raw_via_arc = arc.as_raw_fd();
    let raw_via_borrow = sandbox.root_dirfd().as_raw_fd();
    assert_eq!(raw_via_arc, raw_via_borrow);
    // Drop the Arc clone first; the borrowed cursor must keep working.
    drop(arc);
    assert!(sandbox.current_dirfd().as_raw_fd() >= 0);
}

// ================================================================
// Regression test for rsync 3.4.3 fix: -K / --copy-dirlinks + sandbox
// ================================================================

/// Regression: upstream rsync 3.4.3 fixed a bug where `-K` (copy-dirlinks)
/// caused "failed verification -- update discarded" errors during delta
/// transfers. The root cause was `secure_relative_open()` rejecting every
/// symlink via per-component `O_NOFOLLOW` walk, even legitimate directory
/// symlinks preserved by `-K`.
///
/// This test verifies that `DirSandbox::enter_follow_dirlinks()` allows
/// descending through an in-tree directory symlink - the receiver-side
/// operation that failed in rsync <= 3.4.2 when `-K` was active.
///
/// Scenario:
/// 1. Destination has `subdir` as a symlink to `real_target` (both within
///    the destination tree).
/// 2. `enter()` rejects the symlink (correct default behavior).
/// 3. `enter_follow_dirlinks()` permits it (the `-K` path).
/// 4. After entering through the symlink, a file `stat` at the current
///    dirfd resolves correctly to the real target's child.
#[test]
fn copy_dirlinks_descent_through_in_tree_directory_symlink() {
    let (_keep, root) = canonical_tempdir();

    // Build destination tree matching the -K pattern:
    //   root/
    //     real_target/
    //       file.bin
    //     subdir -> real_target   (directory symlink, relative target)
    //
    // The symlink target is expressed relative to the symlink's parent so
    // openat2(RESOLVE_BENEATH) accepts it: an absolute target would be
    // rejected with EXDEV even when it resolves in-tree, because the
    // kernel cannot prove a re-resolution from / stays beneath the dirfd.
    // Real -K flows preserve the source symlink target verbatim and those
    // are typically relative.
    let real_target = root.join("real_target");
    std::fs::create_dir(&real_target).unwrap();
    std::fs::write(real_target.join("file.bin"), b"delta-payload").unwrap();
    symlink("real_target", root.join("subdir")).unwrap();

    let mut sandbox = DirSandbox::open_root(&root).expect("open root");

    // Default enter() must reject the symlink - this is the strict
    // SEC-1 invariant and must NOT regress.
    let err = sandbox
        .enter(OsStr::new("subdir"))
        .expect_err("default enter must reject directory symlink");
    let code = err.raw_os_error().expect("must carry errno");
    assert!(
        code == libc::ELOOP || code == libc::ENOTDIR,
        "expected ELOOP or ENOTDIR, got errno={code}"
    );
    assert_eq!(sandbox.depth(), 0, "failed enter must not push");

    // enter_follow_dirlinks() must succeed - this is the -K fix.
    sandbox
        .enter_follow_dirlinks(OsStr::new("subdir"))
        .expect("enter_follow_dirlinks must succeed for in-tree directory symlink");
    assert_eq!(sandbox.depth(), 1);

    // The current dirfd should resolve to the real target, allowing
    // file operations through the symlink. Verify by statting the
    // child file through the sandbox dirfd.
    let meta = fast_io::fstatat_nofollow(sandbox.current_dirfd(), OsStr::new("file.bin"))
        .expect("stat file.bin through symlinked directory");
    assert!(
        meta.is_file(),
        "file.bin must be visible through the followed directory symlink"
    );

    sandbox.exit();
    assert_eq!(sandbox.depth(), 0);
}
