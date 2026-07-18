//! SEC-1.n SAFETY-of-LEGITIMATE-USE counterpart to SEC-1.m's attack
//! harness: the sandbox-anchored primitives introduced by SEC-1.f
//! (`fstatat(AT_SYMLINK_NOFOLLOW)`) and SEC-1.g (`unlinkat` family) must
//! not regress the day-to-day legitimate symlink workflows rsync
//! supports with `-l` / `-a`.
//!
//! Rsync transfers legitimate symlinks all the time: relative links,
//! absolute links, symlinks to directories, broken symlinks, symlink
//! chains, and symlinks mixed with hardlinks. The SEC-1 mitigations
//! tighten how the receiver stats and unlinks destination obstacles -
//! they MUST NOT change how legitimate symlinks land on disk.
//!
//! Each scenario shapes a receiver-style destination tree the same way
//! the receiver's `create_symlinks` and `create_hardlinks` paths shape
//! it, calls the same SEC-1 sandbox-anchored helpers the receiver
//! calls, and asserts the SAFETY invariant: every legitimate symlink
//! that rsync would emit lands on disk verbatim, with its target text
//! preserved byte-for-byte, and adjacent files (hardlinked siblings,
//! reachable targets) are left intact.
//!
//! The six scenarios mirror real upstream rsync legitimate-use cases:
//!
//! 1. **Plain relative symlink** (`scenario_1`) - the most common
//!    case. `dir/link -> target.txt` lands as a symlink whose target
//!    text is the relative path.
//!
//! 2. **Absolute symlink** (`scenario_2`) - `dir/link -> /etc/hostname`
//!    lands verbatim. Without `--copy-unsafe-links`, rsync preserves
//!    the absolute link even though it points outside the tree.
//!
//! 3. **Symlink to directory** (`scenario_3`) - `dir/link -> subdir/`
//!    lands as a symlink; the underlying `subdir/file.txt` lands once
//!    as a regular file, not duplicated through the symlink.
//!
//! 4. **Broken symlink** (`scenario_4`) - `dir/dangling -> nonexistent`
//!    lands as a symlink. The sandbox-anchored stat reports the
//!    symlink, not ENOENT-via-follow, and the receiver does not fail.
//!
//! 5. **Symlink chain** (`scenario_5`) - `a -> b -> c -> file.txt`.
//!    Each link is preserved verbatim. The receiver does not flatten
//!    the chain or resolve through it.
//!
//! 6. **Hardlink-with-symlink mixed** (`scenario_6`) - `hl1` and
//!    `hl2` share an inode; `sl -> hl1`. With `-aH`, the hardlink
//!    relationship between `hl1`/`hl2` is preserved AND the symlink
//!    pointing at `hl1` is preserved as a symlink.
//!
//! As with SEC-1.m, this harness does not stand up a full receiver
//! pipeline. The SEC-1 invariants live in
//! `fast_io::lstat_via_sandbox_or_fallback` and the receiver consumes
//! them through the `DirSandbox` carrier (SEC-1.e). Calling the same
//! helpers on a receiver-shaped tree is the integration boundary the
//! SEC-1.f/g cutovers actually defend, and it is the surface that must
//! continue to behave correctly for legitimate symlink workflows.
//!
//! Dependency note: this test file uses the SEC-1.f API surface
//! (`fast_io::DirSandbox`, `fast_io::lstat_via_sandbox_or_fallback`,
//! `fast_io::LstatOutcome`) which is present in current master after
//! #4668 merged. It does not consume the SEC-1.g `UnlinkFlags` /
//! `unlink_via_sandbox_or_fallback` surface from #4671, so the file
//! compiles cleanly against master while #4671 is still in CI.

#![cfg(unix)]

use std::os::unix::fs::{MetadataExt, symlink};
use std::path::{Path, PathBuf};

use fast_io::{DirSandbox, LstatOutcome, lstat_via_sandbox_or_fallback};
use tempfile::{TempDir, tempdir};

/// `tempdir()` may sit under a symlink prefix on macOS / some CI
/// runners; canonicalise so the sandbox open succeeds under
/// `RESOLVE_NO_SYMLINKS`.
fn canonical_tempdir() -> (TempDir, PathBuf) {
    let dir = tempdir().expect("tempdir");
    let canon = std::fs::canonicalize(dir.path()).expect("canonicalize");
    (dir, canon)
}

/// Mirrors the receiver's `create_symlinks` shape: probe the
/// destination leaf through the sandbox-anchored stat (SEC-1.f), then
/// create the symlink via `std::os::unix::fs::symlink`, exactly as
/// `receiver/directory/links.rs` does on the post-SEC-1.f code path.
///
/// Returns the `LstatOutcome` the receiver would observe for the new
/// leaf. Callers assert the outcome reports the symlink (not the
/// target it points at) - the SEC-1 invariant for legitimate symlinks.
fn receiver_shaped_symlink_create(
    sandbox: &DirSandbox,
    dest_root: &Path,
    leaf: &Path,
    target: &Path,
) -> LstatOutcome {
    let link_path = dest_root.join(leaf);
    // The receiver's pre-create obstacle probe. ENOENT here is the
    // legitimate empty-destination case; anything else means a stale
    // obstacle that the receiver would unlink first. We assert ENOENT
    // so the legitimate path is taken.
    let probe = lstat_via_sandbox_or_fallback(Some(sandbox), dest_root, leaf, &link_path);
    assert!(
        matches!(
            probe.as_ref().err().map(std::io::Error::kind),
            Some(std::io::ErrorKind::NotFound)
        ),
        "legitimate symlink-create scenario requires an empty leaf; \
         got {probe:?}"
    );
    symlink(target, &link_path).expect("symlink create");
    lstat_via_sandbox_or_fallback(Some(sandbox), dest_root, leaf, &link_path)
        .expect("post-create lstat must succeed")
}

/// Asserts an `LstatOutcome` reports a symlink leaf rather than the
/// target it points at. This is the legitimate-use mirror of SEC-1.m's
/// "must not resolve to sensitive target" assertion.
fn assert_outcome_is_symlink(outcome: &LstatOutcome, ctx: &str) {
    match outcome {
        LstatOutcome::At(meta) => assert!(
            meta.is_symlink(),
            "{ctx}: sandbox-anchored lstat must report a symlink leaf"
        ),
        LstatOutcome::Std(meta) => assert!(
            meta.is_symlink(),
            "{ctx}: fallback lstat must report a symlink leaf"
        ),
    }
}

// Scenario 1: Plain relative symlink.
// SAFETY invariant: a relative-target symlink lands on disk with its
// target text preserved verbatim, and the sandbox-anchored stat reports
// the symlink itself (not the file it points at).

#[test]
fn scenario_1_plain_relative_symlink_preserved_verbatim() {
    let (_keep, dest) = canonical_tempdir();
    std::fs::write(dest.join("target.txt"), b"plain-relative-target").expect("write target");

    let sandbox = DirSandbox::open_root(&dest).expect("sandbox");

    let outcome =
        receiver_shaped_symlink_create(&sandbox, &dest, Path::new("link"), Path::new("target.txt"));
    assert_outcome_is_symlink(&outcome, "scenario_1");

    let link_path = dest.join("link");
    assert!(
        link_path.symlink_metadata().expect("lmeta").is_symlink(),
        "scenario_1: dest leaf must be a symlink"
    );
    assert_eq!(
        std::fs::read_link(&link_path).expect("read_link"),
        PathBuf::from("target.txt"),
        "scenario_1: relative target text must be preserved verbatim"
    );
    // The reachable target must still exist as a regular file with
    // its original contents.
    assert_eq!(
        std::fs::read(dest.join("target.txt")).expect("read target"),
        b"plain-relative-target",
        "scenario_1: target file must be untouched"
    );
}

// Scenario 2: Absolute symlink.
// SAFETY invariant: an absolute-target symlink lands on disk verbatim,
// even when the target points outside the destination tree. Without
// `--copy-unsafe-links`, rsync preserves the symlink rather than
// dereferencing it; the sandbox-anchored stat reports the symlink
// itself.

#[test]
fn scenario_2_absolute_symlink_preserved_verbatim() {
    let (_keep, dest) = canonical_tempdir();
    let sandbox = DirSandbox::open_root(&dest).expect("sandbox");

    // `/etc/hostname` is a stable absolute target on Linux and macOS.
    // The test does not require the target to exist - rsync preserves
    // the symlink regardless - but the path text is what we assert.
    let abs_target = PathBuf::from("/etc/hostname");
    let outcome = receiver_shaped_symlink_create(&sandbox, &dest, Path::new("link"), &abs_target);
    assert_outcome_is_symlink(&outcome, "scenario_2");

    let link_path = dest.join("link");
    assert!(
        link_path.symlink_metadata().expect("lmeta").is_symlink(),
        "scenario_2: dest leaf must be a symlink, not a copy of /etc/hostname"
    );
    assert_eq!(
        std::fs::read_link(&link_path).expect("read_link"),
        abs_target,
        "scenario_2: absolute target text must be preserved verbatim"
    );
}

// Scenario 3: Symlink to directory.
// SAFETY invariant: a symlink to a directory lands as a symlink and
// the underlying directory contents land once as regular files - the
// receiver does not duplicate them through the symlink.

#[test]
fn scenario_3_symlink_to_directory_preserved_with_unique_contents() {
    let (_keep, dest) = canonical_tempdir();
    // Receiver-shaped tree: a real subdir with a file, plus a leaf
    // symlink pointing at the subdir.
    std::fs::create_dir(dest.join("subdir")).expect("mkdir subdir");
    std::fs::write(dest.join("subdir/file.txt"), b"once-and-only-once").expect("write file");

    let sandbox = DirSandbox::open_root(&dest).expect("sandbox");

    let outcome =
        receiver_shaped_symlink_create(&sandbox, &dest, Path::new("link"), Path::new("subdir"));
    assert_outcome_is_symlink(&outcome, "scenario_3");

    let link_path = dest.join("link");
    assert!(
        link_path.symlink_metadata().expect("lmeta").is_symlink(),
        "scenario_3: dest leaf must be a symlink to subdir, not a copy of it"
    );
    assert_eq!(
        std::fs::read_link(&link_path).expect("read_link"),
        PathBuf::from("subdir"),
        "scenario_3: directory-symlink target text must be preserved verbatim"
    );

    // The real directory still holds exactly one file; the symlink
    // did not cause the receiver to duplicate it.
    let entries: Vec<_> = std::fs::read_dir(dest.join("subdir"))
        .expect("read_dir subdir")
        .map(|e| e.expect("entry").file_name())
        .collect();
    assert_eq!(
        entries.len(),
        1,
        "scenario_3: subdir must hold exactly one entry, not be duplicated through the symlink"
    );
    assert_eq!(
        std::fs::read(dest.join("subdir/file.txt")).expect("read file.txt"),
        b"once-and-only-once",
        "scenario_3: subdir/file.txt must be untouched by the symlink-create path"
    );
}

// Scenario 4: Broken symlink.
// SAFETY invariant: a symlink whose target does not exist lands as a
// symlink. The sandbox-anchored stat reports the symlink (not ENOENT
// from following it) and the receiver does not fail.

#[test]
fn scenario_4_broken_symlink_preserved_without_follow_error() {
    let (_keep, dest) = canonical_tempdir();
    let sandbox = DirSandbox::open_root(&dest).expect("sandbox");

    let outcome = receiver_shaped_symlink_create(
        &sandbox,
        &dest,
        Path::new("dangling"),
        Path::new("nonexistent"),
    );
    // The whole point: the SEC-1.f primitive uses AT_SYMLINK_NOFOLLOW,
    // so a dangling symlink reports as a symlink, not ENOENT via the
    // dereference. Any path-based lstat would also see the symlink,
    // but the historical regression risk was a helper that secretly
    // followed; this assertion guards against that.
    assert_outcome_is_symlink(&outcome, "scenario_4");

    let link_path = dest.join("dangling");
    assert!(
        link_path.symlink_metadata().expect("lmeta").is_symlink(),
        "scenario_4: dest leaf must be a symlink even though its target is missing"
    );
    assert_eq!(
        std::fs::read_link(&link_path).expect("read_link"),
        PathBuf::from("nonexistent"),
        "scenario_4: broken-symlink target text must be preserved verbatim"
    );
    // The target genuinely does not exist; the receiver-shaped path
    // did not accidentally create it.
    assert!(
        !dest.join("nonexistent").exists(),
        "scenario_4: dangling target must not be auto-materialised"
    );
}

// Scenario 5: Symlink chain `a -> b -> c -> file.txt`.
// SAFETY invariant: every link in the chain lands verbatim. The
// receiver does not flatten the chain or resolve any link to its
// terminal target.

#[test]
fn scenario_5_symlink_chain_preserved_link_by_link() {
    let (_keep, dest) = canonical_tempdir();
    std::fs::write(dest.join("file.txt"), b"chain-terminus").expect("write file.txt");

    let sandbox = DirSandbox::open_root(&dest).expect("sandbox");

    // Create the chain bottom-up, the order the receiver would emit
    // it as it walks the file list. Each create call routes through
    // the SEC-1.f sandbox-anchored probe + std::os::unix::fs::symlink.
    let outcome_c =
        receiver_shaped_symlink_create(&sandbox, &dest, Path::new("c"), Path::new("file.txt"));
    let outcome_b = receiver_shaped_symlink_create(&sandbox, &dest, Path::new("b"), Path::new("c"));
    let outcome_a = receiver_shaped_symlink_create(&sandbox, &dest, Path::new("a"), Path::new("b"));

    assert_outcome_is_symlink(&outcome_c, "scenario_5/c");
    assert_outcome_is_symlink(&outcome_b, "scenario_5/b");
    assert_outcome_is_symlink(&outcome_a, "scenario_5/a");

    // Each link is preserved as a symlink with its exact target text.
    // No link was flattened into a duplicate of file.txt.
    assert_eq!(
        std::fs::read_link(dest.join("a")).expect("read_link a"),
        PathBuf::from("b"),
        "scenario_5: a must point at b verbatim"
    );
    assert_eq!(
        std::fs::read_link(dest.join("b")).expect("read_link b"),
        PathBuf::from("c"),
        "scenario_5: b must point at c verbatim"
    );
    assert_eq!(
        std::fs::read_link(dest.join("c")).expect("read_link c"),
        PathBuf::from("file.txt"),
        "scenario_5: c must point at file.txt verbatim"
    );

    // The terminal target keeps its original contents - no link in
    // the chain was resolved-and-rewritten.
    assert_eq!(
        std::fs::read(dest.join("file.txt")).expect("read file.txt"),
        b"chain-terminus",
        "scenario_5: terminal file.txt must be untouched"
    );
}

// Scenario 6: Hardlink siblings plus a symlink pointing at one of them.
// SAFETY invariant: with `-aH`, the hardlink relationship between
// `hl1`/`hl2` is preserved (same dev/ino) AND the symlink `sl -> hl1`
// is preserved as a symlink. The sandbox-anchored stat used by the
// receiver's hardlink quick-check correctly distinguishes the symlink
// leaf from its target inode.

#[test]
fn scenario_6_hardlink_pair_with_symlink_preserved() {
    let (_keep, dest) = canonical_tempdir();

    // Hardlink leader + follower, as the receiver's `create_hardlinks`
    // would lay them down: leader transferred as a regular file,
    // follower created via `std::fs::hard_link` once the leader is on
    // disk.
    std::fs::write(dest.join("hl1"), b"shared-inode-payload").expect("write hl1");
    std::fs::hard_link(dest.join("hl1"), dest.join("hl2")).expect("hard_link hl2");

    let sandbox = DirSandbox::open_root(&dest).expect("sandbox");

    // The symlink sibling is then created the same way
    // `create_symlinks` would create it.
    let outcome_sl =
        receiver_shaped_symlink_create(&sandbox, &dest, Path::new("sl"), Path::new("hl1"));
    assert_outcome_is_symlink(&outcome_sl, "scenario_6/sl");

    // Hardlink invariant: hl1 and hl2 share an inode. The
    // sandbox-anchored stat must report the same dev/ino for both
    // (it is the same physical inode regardless of which name we
    // anchor through).
    let stat_hl1 =
        lstat_via_sandbox_or_fallback(Some(&sandbox), &dest, Path::new("hl1"), &dest.join("hl1"))
            .expect("stat hl1");
    let stat_hl2 =
        lstat_via_sandbox_or_fallback(Some(&sandbox), &dest, Path::new("hl2"), &dest.join("hl2"))
            .expect("stat hl2");
    assert_eq!(
        stat_hl1.dev(),
        stat_hl2.dev(),
        "scenario_6: hardlinked pair must live on the same filesystem"
    );
    assert_eq!(
        stat_hl1.ino(),
        stat_hl2.ino(),
        "scenario_6: hardlinked pair must share an inode (the -H invariant)"
    );

    // Symlink invariant: `sl` is a symlink whose target text is the
    // exact string `hl1`, not the inode it shares. The sandbox-
    // anchored stat reports it as a symlink, distinct from the
    // hardlinked inode.
    let sl_path = dest.join("sl");
    assert!(
        sl_path.symlink_metadata().expect("lmeta sl").is_symlink(),
        "scenario_6: sl must be a symlink, not a third hardlink"
    );
    assert_eq!(
        std::fs::read_link(&sl_path).expect("read_link sl"),
        PathBuf::from("hl1"),
        "scenario_6: symlink target text must be preserved verbatim"
    );
    let sl_meta = std::fs::symlink_metadata(&sl_path).expect("symlink_metadata sl");
    assert_ne!(
        sl_meta.ino(),
        stat_hl1.ino(),
        "scenario_6: symlink leaf must have a distinct inode from the hardlink it points at"
    );

    // The hardlinked payload survived all three creates with its
    // original contents.
    assert_eq!(
        std::fs::read(dest.join("hl1")).expect("read hl1"),
        b"shared-inode-payload",
        "scenario_6: hardlink payload must be untouched"
    );
    assert_eq!(
        std::fs::read(dest.join("hl2")).expect("read hl2"),
        b"shared-inode-payload",
        "scenario_6: hardlink payload via hl2 must match (same inode)"
    );
}
