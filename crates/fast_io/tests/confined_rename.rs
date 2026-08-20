//! `confined_rename` must confine each endpoint independently.
//!
//! Regression cover for the escape upstream's `rename-fullpath-symlink-race`
//! test demonstrates: the receiver commits `<root>/sub/name` while `sub` is
//! flipped to a symlink pointing outside the tree, and an unconfined
//! `rename(2)` follows it.
//!
//! upstream: `rsync-3.5.0/syscall.c:1866` `do_rename_at()`.

#![cfg(unix)]

use std::fs;
use std::path::Path;

use tempfile::TempDir;

/// Stage `<dir>/name` with `body` and return its path.
fn write_file(dir: &Path, name: &str, body: &str) -> std::path::PathBuf {
    let path = dir.join(name);
    fs::write(&path, body).expect("write fixture");
    path
}

/// Non-vacuity companion: with no symlink in play the confined rename performs
/// an ordinary nested commit. Without this, the refusal tests below would also
/// pass if `confined_rename` simply failed for every input.
#[test]
fn confined_rename_commits_a_nested_destination() {
    let root = TempDir::new().expect("tempdir");
    fs::create_dir(root.path().join("sub")).expect("mkdir sub");
    let temp = write_file(root.path(), ".tmp", "payload");
    let final_path = root.path().join("sub").join("f0");

    fast_io::confined_rename(root.path(), &temp, &final_path, true).expect("commit");

    assert_eq!(fs::read_to_string(&final_path).unwrap(), "payload");
}

/// The destination parent is a symlink pointing outside the root. Upstream's
/// walk refuses an absolute symlink target, so the commit must fail and nothing
/// may appear in the outside tree.
#[test]
fn confined_rename_refuses_a_destination_parent_symlinked_outside() {
    let base = TempDir::new().expect("tempdir");
    let root = base.path().join("dest");
    let outside = base.path().join("outside");
    fs::create_dir(&root).expect("mkdir dest");
    fs::create_dir(&outside).expect("mkdir outside");
    std::os::unix::fs::symlink(&outside, root.join("sub")).expect("plant symlink");

    let temp = write_file(&root, ".tmp", "payload");
    let final_path = root.join("sub").join("f0");

    let err = fast_io::confined_rename(&root, &temp, &final_path, true)
        .expect_err("a symlinked destination parent must be refused");

    assert!(
        !outside.join("f0").exists(),
        "the payload escaped the destination tree: {err}"
    );
}

/// The escape is only reachable because an absolute `--temp-dir` puts the
/// *source* outside the root. Confining per side means that does not disable
/// confinement of the destination - the all-or-nothing rule upstream 3.5.0
/// replaced is exactly what this pins against.
#[test]
fn an_out_of_tree_source_does_not_disable_destination_confinement() {
    let base = TempDir::new().expect("tempdir");
    let root = base.path().join("dest");
    let outside = base.path().join("outside");
    let tmpd = base.path().join("tmpd");
    fs::create_dir(&root).expect("mkdir dest");
    fs::create_dir(&outside).expect("mkdir outside");
    fs::create_dir(&tmpd).expect("mkdir tmpd");
    std::os::unix::fs::symlink(&outside, root.join("sub")).expect("plant symlink");

    // The temp lives outside `root`, exactly as an absolute --temp-dir stages it.
    let temp = write_file(&tmpd, "f0.tmp", "payload");
    let final_path = root.join("sub").join("f0");

    let err = fast_io::confined_rename(&root, &temp, &final_path, true)
        .expect_err("an out-of-tree source must not license an unconfined destination");

    assert!(
        !outside.join("f0").exists(),
        "the payload escaped the destination tree: {err}"
    );
    assert!(
        temp.exists(),
        "the refused rename must leave the temp intact"
    );
}
