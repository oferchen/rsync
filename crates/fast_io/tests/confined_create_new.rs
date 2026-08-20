//! `confined_create_new` must anchor the destination parent before creating.
//!
//! The direct-write strategy skips upstream's stage-then-rename, so it never
//! reaches `confined_rename` and cannot inherit its confinement. Without an
//! anchored create, a parent flipped to a symlink between the write decision
//! and the create redirects the new file out of the tree.
//!
//! These pin the primitive on every unix, not only where `WriteStrategy::Direct`
//! happens to be chosen: `O_TMPFILE` is Linux-only, so a behavioural test alone
//! would exercise this on macOS and skip it on Linux.
//!
//! upstream: `rsync-3.5.0/syscall.c:2891` `ds_descend()`.

#![cfg(unix)]

use std::fs;
use std::io::Write;

use tempfile::TempDir;

/// Non-vacuity companion: with no symlink in play the confined create makes an
/// ordinary nested file. Without this, the refusal tests below would also pass
/// if `confined_create_new` simply failed for every input.
#[test]
fn confined_create_new_creates_a_nested_destination() {
    let root = TempDir::new().expect("tempdir");
    fs::create_dir(root.path().join("sub")).expect("mkdir sub");
    let dest = root.path().join("sub").join("f0");

    let mut file = fast_io::confined_create_new(root.path(), &dest).expect("create");
    file.write_all(b"payload").expect("write");
    drop(file);

    assert_eq!(fs::read_to_string(&dest).unwrap(), "payload");
}

/// A relative, in-tree symlinked parent is FOLLOWED, not refused - upstream's
/// walk descends it (`syscall.c:2961`). Pinning this is what stops a future
/// "refuse every symlink" simplification from breaking `-K`.
#[test]
fn confined_create_new_follows_a_relative_in_tree_parent_symlink() {
    let root = TempDir::new().expect("tempdir");
    fs::create_dir(root.path().join("real")).expect("mkdir real");
    std::os::unix::fs::symlink("real", root.path().join("sub")).expect("plant symlink");
    let dest = root.path().join("sub").join("f0");

    let mut file = fast_io::confined_create_new(root.path(), &dest).expect("create through link");
    file.write_all(b"payload").expect("write");
    drop(file);

    assert_eq!(
        fs::read_to_string(root.path().join("real").join("f0")).unwrap(),
        "payload",
        "the file must land in the symlink's target, inside the tree"
    );
}

/// The destination parent is a symlink pointing outside the root. Upstream's
/// walk refuses an absolute target, so the create must fail and nothing may
/// appear in the outside tree.
#[test]
fn confined_create_new_refuses_a_parent_symlinked_outside() {
    let base = TempDir::new().expect("tempdir");
    let root = base.path().join("dest");
    let outside = base.path().join("outside");
    fs::create_dir(&root).expect("mkdir dest");
    fs::create_dir(&outside).expect("mkdir outside");
    std::os::unix::fs::symlink(&outside, root.join("sub")).expect("plant symlink");

    let dest = root.join("sub").join("f0");
    let err = fast_io::confined_create_new(&root, &dest)
        .expect_err("a symlinked destination parent must be refused");

    assert!(
        !outside.join("f0").exists(),
        "the create escaped the destination tree: {err}"
    );
}

/// The leaf itself is a symlink out of the tree. `O_EXCL` alone already refuses
/// an existing name, but `O_NOFOLLOW` is what makes the refusal mean "do not
/// follow" rather than "it exists" - and neither may write through the link.
#[test]
fn confined_create_new_refuses_a_leaf_symlinked_outside() {
    let base = TempDir::new().expect("tempdir");
    let root = base.path().join("dest");
    let outside = base.path().join("outside");
    fs::create_dir(&root).expect("mkdir dest");
    fs::create_dir(&outside).expect("mkdir outside");
    std::os::unix::fs::symlink(outside.join("f0"), root.join("f0")).expect("plant symlink");

    let err = fast_io::confined_create_new(&root, &root.join("f0"))
        .expect_err("a symlinked leaf must be refused");

    assert!(
        !outside.join("f0").exists(),
        "the create followed the leaf symlink out of the tree: {err}"
    );
}

/// `O_EXCL` survives the move onto the anchored path: a name that already
/// exists as a regular file loses the race rather than being truncated.
#[test]
fn confined_create_new_refuses_an_existing_regular_file() {
    let root = TempDir::new().expect("tempdir");
    let dest = root.path().join("f0");
    fs::write(&dest, "original").expect("seed");

    let err = fast_io::confined_create_new(root.path(), &dest)
        .expect_err("an existing destination must lose the race");

    assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
    assert_eq!(
        fs::read_to_string(&dest).unwrap(),
        "original",
        "the refused create must not truncate the existing file"
    );
}
