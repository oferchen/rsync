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

/// `confined_clone_file` takes an already-OPEN source descriptor, so the caller
/// owns the source-side confinement decision and cannot have it re-resolved by
/// the libc resolver behind its back. These tests therefore supply the
/// descriptor the caller's confined open would have produced.
fn open_source(path: &std::path::Path) -> fs::File {
    fs::File::open(path).expect("open source")
}

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

    match fast_io::confined_clone_file(root.path(), &open_source(&src), &dest)
        .expect("no confinement refusal")
    {
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

    let outcome =
        fast_io::confined_clone_file(root.path(), &open_source(&src), &root.path().join("sub/f0"))
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

    let err = fast_io::confined_clone_file(&root, &open_source(&src), &root.join("sub").join("f0"))
        .expect_err("a symlinked destination parent must be refused, not reported Unsupported");

    assert!(
        !outside.join("f0").exists(),
        "the clone escaped the destination tree: {err}"
    );
}

/// The clone must read the descriptor it was HANDED, never re-resolve the
/// source path. This is the sender TOCTOU the upstream `symlink-race-source`
/// cell catches: the caller's confined open is what decides the source, and a
/// re-open by path afterwards can be pointed anywhere in between - including
/// outside the transfer root.
///
/// Taking `&File` makes the escape unspellable, so this is a REVERSION GUARD:
/// restoring the `&Path` signature and the `File::open(src)` inside `clone_at`
/// makes it read `OUTSIDE-SECRET` and fail.
///
/// upstream: `rsync-3.5.0/syscall.c:2896-2961` - the confined walk refuses an
/// absolute symlink target, and that refusal is worthless if a later syscall
/// resolves the same name again with the libc resolver.
#[test]
fn confined_clone_file_clones_the_handed_descriptor_not_the_path() {
    let base = TempDir::new().expect("tempdir");
    let root = base.path().join("dest");
    let outside = base.path().join("outside");
    fs::create_dir(&root).expect("mkdir dest");
    fs::create_dir(&outside).expect("mkdir outside");

    let src = base.path().join("payload.bin");
    fs::write(&src, b"IN-TREE").expect("write source");
    let source = open_source(&src);

    // The window opens here: the descriptor is already resolved, and the name
    // it came from now points at content outside the tree.
    fs::remove_file(&src).expect("unlink source");
    let secret = outside.join("secret.bin");
    fs::write(&secret, b"OUTSIDE-SECRET").expect("write secret");
    std::os::unix::fs::symlink(&secret, &src).expect("plant symlink");

    // Non-vacuity control: the fixture only discriminates while the NAME and
    // the DESCRIPTOR disagree. Reading the name must reach the outside secret;
    // if the plant silently failed, every assertion below would be empty.
    assert_eq!(
        fs::read(&src).expect("read through the planted name"),
        b"OUTSIDE-SECRET",
        "the fixture is inert - the source name still resolves in-tree"
    );

    let dest = root.join("f0");
    match fast_io::confined_clone_file(&root, &source, &dest).expect("no confinement refusal") {
        CloneAttempt::Cloned => assert_eq!(
            fs::read(&dest).expect("read clone"),
            b"IN-TREE",
            "the clone re-resolved the source NAME and copied content from \
             outside the transfer root"
        ),
        CloneAttempt::Unsupported => assert!(
            !dest.exists(),
            "an Unsupported result must leave nothing behind"
        ),
    }
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

    let result = fast_io::confined_clone_file(root.path(), &open_source(&src), &dest);

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
