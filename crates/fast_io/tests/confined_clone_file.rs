//! `confined_clone_file` must anchor the destination parent before cloning.
//!
//! A copy-on-write clone creates and fills the destination in one syscall, so
//! the create IS the confinement decision - there is no later commit to anchor.
//! Without this, the platform CoW fast path writes the destination by path and
//! follows a parent symlink straight out of the transfer root, which is the
//! escape `keep_dirlinks_refuses_a_destination_symlink_pointing_outside_the_tree`
//! catches at the engine level.
//!
//! upstream: `rsync-3.5.0/receiver.c` has no CoW fast path - it always stages
//! into a temp and commits with `do_rename_at()`. These pin oc's optimisation
//! to the same invariant.
//!
//! Every assertion here is written to hold whether or not the filesystem can
//! reflink: `Unsupported` is a routine answer on a non-APFS/non-reflink volume
//! and must never be confused with a refusal.

#![cfg(unix)]

use std::fs;

use fast_io::CloneAttempt;
use tempfile::TempDir;

/// Non-vacuity companion: with no symlink in play the call reaches the platform
/// and reports a real outcome rather than failing. Without this, the refusal
/// tests below would also pass if `confined_clone_file` errored for every input.
#[test]
fn confined_clone_file_reaches_the_platform_for_a_plain_destination() {
    let root = TempDir::new().expect("tempdir");
    fs::create_dir(root.path().join("sub")).expect("mkdir sub");
    let src = root.path().join("src.bin");
    fs::write(&src, b"payload").expect("write source");
    let dest = root.path().join("sub").join("f0");

    match fast_io::confined_clone_file(root.path(), &src, &dest).expect("no confinement refusal") {
        CloneAttempt::Cloned => {
            assert_eq!(fs::read(&dest).expect("read clone"), b"payload");
        }
        CloneAttempt::Unsupported => {
            assert!(
                !dest.exists(),
                "an Unsupported result must leave nothing behind - the caller \
                 falls through to a copy that expects to create the file"
            );
        }
    }
}

/// A relative, in-tree symlinked parent is FOLLOWED, not refused - upstream's
/// walk descends it (`syscall.c:2961`). Pinning this stops a future
/// "refuse every symlink" simplification from breaking `-K`.
#[test]
fn confined_clone_file_follows_a_relative_in_tree_parent_symlink() {
    let root = TempDir::new().expect("tempdir");
    fs::create_dir(root.path().join("real")).expect("mkdir real");
    std::os::unix::fs::symlink("real", root.path().join("sub")).expect("plant symlink");
    let src = root.path().join("src.bin");
    fs::write(&src, b"payload").expect("write source");

    let outcome = fast_io::confined_clone_file(root.path(), &src, &root.path().join("sub/f0"))
        .expect("a relative in-tree parent symlink must not be refused");

    if outcome == CloneAttempt::Cloned {
        assert_eq!(
            fs::read(root.path().join("real").join("f0")).expect("read clone"),
            b"payload",
            "the clone must land in the symlink's target, inside the tree"
        );
    }
}

/// The destination parent is a symlink pointing outside the root. Upstream's
/// walk refuses an absolute target, so this must be an `Err` - never
/// `Unsupported`, which the caller is entitled to answer with an unconfined
/// data copy.
#[test]
fn confined_clone_file_refuses_a_parent_symlinked_outside() {
    let base = TempDir::new().expect("tempdir");
    let root = base.path().join("dest");
    let outside = base.path().join("outside");
    fs::create_dir(&root).expect("mkdir dest");
    fs::create_dir(&outside).expect("mkdir outside");
    std::os::unix::fs::symlink(&outside, root.join("sub")).expect("plant symlink");
    let src = base.path().join("src.bin");
    fs::write(&src, b"payload").expect("write source");

    let err = fast_io::confined_clone_file(&root, &src, &root.join("sub").join("f0"))
        .expect_err("a symlinked destination parent must be refused, not reported Unsupported");

    assert!(
        !outside.join("f0").exists(),
        "the clone escaped the destination tree: {err}"
    );
}

/// An existing destination loses the race rather than being replaced, matching
/// the `O_EXCL` semantics of the plain create this tier bypasses.
#[test]
fn confined_clone_file_refuses_an_existing_destination() {
    let root = TempDir::new().expect("tempdir");
    let src = root.path().join("src.bin");
    fs::write(&src, b"payload").expect("write source");
    let dest = root.path().join("f0");
    fs::write(&dest, b"original").expect("seed destination");

    let result = fast_io::confined_clone_file(root.path(), &src, &dest);

    assert!(
        !matches!(result, Ok(CloneAttempt::Cloned)),
        "an existing destination must never be replaced by a clone"
    );
    assert_eq!(
        fs::read(&dest).expect("read destination"),
        b"original",
        "the refused clone must leave the existing file intact"
    );
}
